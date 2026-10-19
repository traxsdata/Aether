/*
 * net.c - IPv4 networking: Ethernet, ARP, IP, UDP (for DHCP and DNS only)
 * and a TCP client.
 *
 * The design is deliberately one-way. æther opens connections; it never
 * accepts them:
 *   - there is no listening socket of any kind, so a segment that isn't
 *     part of a connection an app opened is dropped without an answer
 *     (no RST, no ICMP: the machine doesn't even confirm it exists);
 *   - the only UDP traffic accepted is the DHCP offer/ack for our own
 *     transaction and DNS answers from the configured server to the random
 *     port and id of a query we're waiting on;
 *   - before a connection is made, the firewall decides by app and host
 *     name (firewall.c, asking the user the first time), DLP refuses apps
 *     that have read Confidential data, and the address the name resolves
 *     to must be a public one (no loopback, link-local or multicast, and
 *     no LAN or real special-use address unless it was typed as one, which
 *     blunts DNS rebinding; TEST-NET lab ranges are exempt).
 *
 * Everything is polled from the window manager loop (net_poll) and from
 * callers waiting in a system call, with interrupts off: the kernel isn't
 * preemptible, so there's no locking. Callers block with WAIT_NET and are
 * woken when something about their connection changes.
 */
#include "net.h"
#include "dlp.h"
#include "kernel.h"
#include "mm.h"
#include "proc.h"
#include "rng.h"
#include "uaccess.h"
#include "usb.h"
#include "wm.h"

/* ---- Byte order and checksums -------------------------------------------------------------- */

static inline uint16_t htons(uint16_t v) { return (uint16_t)(v << 8 | v >> 8); }
static inline uint32_t htonl(uint32_t v) { return __builtin_bswap32(v); }
#define ntohs htons
#define ntohl htonl

uint16_t net_checksum(const void *data, size_t len, uint32_t sum) {
    const uint8_t *p = data;
    while (len > 1) {
        sum += (uint32_t)p[0] << 8 | p[1];
        p += 2;
        len -= 2;
    }
    if (len) sum += (uint32_t)p[0] << 8;
    while (sum >> 16) sum = (sum & 0xFFFF) + (sum >> 16);
    return htons((uint16_t)~sum);
}

/* The TCP/UDP pseudo-header, folded into a starting sum. */
static uint32_t pseudo_sum(uint32_t src, uint32_t dst, uint8_t proto, uint16_t len) {
    uint32_t s = ntohl(src), d = ntohl(dst);
    return (s >> 16) + (s & 0xFFFF) + (d >> 16) + (d & 0xFFFF) + proto + len;
}

/* ---- Wire formats ------------------------------------------------------------------------------- */

#define ETH_IP 0x0800
#define ETH_ARP 0x0806
#define IP_ICMP 1
#define IP_TCP 6
#define IP_UDP 17

struct eth_hdr {
    uint8_t dst[6], src[6];
    uint16_t type;
} __attribute__((packed));

struct arp_pkt {
    uint16_t htype, ptype;
    uint8_t hlen, plen;
    uint16_t op;
    uint8_t sha[6];
    uint32_t spa;
    uint8_t tha[6];
    uint32_t tpa;
} __attribute__((packed));

struct ip_hdr {
    uint8_t ver_ihl, tos;
    uint16_t len, id, frag;
    uint8_t ttl, proto;
    uint16_t csum;
    uint32_t src, dst;
} __attribute__((packed));

struct udp_hdr {
    uint16_t sport, dport, len, csum;
} __attribute__((packed));

struct tcp_hdr {
    uint16_t sport, dport;
    uint32_t seq, ack;
    uint8_t off, flags;
    uint16_t win, csum, urg;
} __attribute__((packed));

#define TCP_FIN 0x01
#define TCP_SYN 0x02
#define TCP_RST 0x04
#define TCP_PSH 0x08
#define TCP_ACK 0x10

#define MTU 1500
#define FRAME_MAX (MTU + sizeof(struct eth_hdr))

/* ---- Interface state ------------------------------------------------------------------------- */

static struct nic_ops *nic;
static struct {
    int configured;
    uint32_t ip, mask, gw, dns; /* network byte order */
    uint64_t lease_ms, renew_at;
} cfg;
static int static_mode; /* a user-configured address: DHCP must not replace it */
static uint64_t rx_packets, tx_packets, dropped;
static uint16_t ip_id;
static const uint8_t bcast_mac[6] = {0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF};

/* ---- ARP --------------------------------------------------------------------------------------- */

#define ARP_ENTRIES 16
#define ARP_TTL_MS 120000

static struct {
    uint32_t ip;
    uint8_t mac[6];
    uint64_t at;
} arp_cache[ARP_ENTRIES];

static void eth_send(const uint8_t *dst, uint16_t type, const void *payload, size_t len) {
    static uint8_t frame[FRAME_MAX];
    if (!nic || len > MTU) return;
    struct eth_hdr *e = (struct eth_hdr *)frame;
    memcpy(e->dst, dst, 6);
    memcpy(e->src, nic->mac, 6);
    e->type = htons(type);
    memcpy(frame + sizeof *e, payload, len);
    if (!nic->tx(frame, sizeof *e + len)) tx_packets++;
}

static void arp_send(uint16_t op, const uint8_t *tha, uint32_t tpa) {
    struct arp_pkt a = {htons(1), htons(ETH_IP), 6, 4, htons(op), {0}, cfg.ip, {0}, tpa};
    memcpy(a.sha, nic->mac, 6);
    if (tha) memcpy(a.tha, tha, 6);
    eth_send(op == 1 ? bcast_mac : tha, ETH_ARP, &a, sizeof a);
}

static void arp_learn(uint32_t ip, const uint8_t *mac) {
    int slot = 0;
    for (int i = 0; i < ARP_ENTRIES; i++) {
        if (arp_cache[i].ip == ip) {
            slot = i;
            break;
        }
        if (arp_cache[i].at < arp_cache[slot].at) slot = i;
    }
    arp_cache[slot].ip = ip;
    memcpy(arp_cache[slot].mac, mac, 6);
    arp_cache[slot].at = ticks;
}

static const uint8_t *arp_lookup(uint32_t ip) {
    for (int i = 0; i < ARP_ENTRIES; i++)
        if (arp_cache[i].ip == ip && arp_cache[i].at && ticks - arp_cache[i].at < ARP_TTL_MS) return arp_cache[i].mac;
    return 0;
}

static void arp_input(const struct arp_pkt *a, size_t len) {
    if (len < sizeof *a || a->htype != htons(1) || a->ptype != htons(ETH_IP) || a->hlen != 6 || a->plen != 4) return;
    if (!cfg.configured || a->tpa != cfg.ip) return; /* only about us */
    if (a->spa == 0 || a->spa == 0xFFFFFFFFu || (a->sha[0] & 1)) return;
    arp_learn(a->spa, a->sha);
    if (a->op == htons(1)) arp_send(2, a->sha, a->spa); /* someone (the router) needs our address */
}

/* ---- IP --------------------------------------------------------------------------------------- */

/* Sends one datagram. With no ARP entry for the next hop it asks and drops
 * the packet; TCP retransmits and DNS/DHCP retry, so nothing waits here. */
static int ip_send(uint32_t dst, uint8_t proto, const void *payload, size_t len) {
    static uint8_t pkt[MTU];
    if (len + sizeof(struct ip_hdr) > MTU) return -E_INVAL;
    struct ip_hdr *h = (struct ip_hdr *)pkt;
    h->ver_ihl = 0x45;
    h->tos = 0;
    h->len = htons((uint16_t)(sizeof *h + len));
    h->id = htons(ip_id++);
    h->frag = htons(0x4000); /* don't fragment */
    h->ttl = 64;
    h->proto = proto;
    h->csum = 0;
    h->src = cfg.ip;
    h->dst = dst;
    h->csum = net_checksum(h, sizeof *h, 0);
    memcpy(pkt + sizeof *h, payload, len);
    const uint8_t *mac;
    if (dst == 0xFFFFFFFFu) {
        mac = bcast_mac;
    } else {
        uint32_t hop = (dst & cfg.mask) == (cfg.ip & cfg.mask) ? dst : cfg.gw;
        if (!(mac = arp_lookup(hop))) {
            arp_send(1, 0, hop);
            return -E_NET;
        }
    }
    eth_send(mac, ETH_IP, pkt, sizeof *h + len);
    return 0;
}

static void udp_input(uint32_t src, uint32_t dst, const uint8_t *data, size_t len);
static void tcp_input(uint32_t src, uint32_t dst, const uint8_t *data, size_t len);

static void ip_input(const uint8_t *data, size_t len) {
    const struct ip_hdr *h = (const struct ip_hdr *)data;
    if (len < sizeof *h || (h->ver_ihl >> 4) != 4) goto drop;
    size_t ihl = (size_t)(h->ver_ihl & 15) * 4, total = ntohs(h->len);
    if (ihl < sizeof *h || total < ihl || total > len) goto drop;
    if (net_checksum(h, ihl, 0) != 0) goto drop;
    if (ntohs(h->frag) & 0x3FFF) goto drop; /* fragments: never needed by what we accept */
    int to_us = cfg.configured ? h->dst == cfg.ip : 1; /* before DHCP, the offer is addressed to it */
    if (!to_us && h->dst != 0xFFFFFFFFu) goto drop;
    if (h->proto == IP_UDP) udp_input(h->src, h->dst, data + ihl, total - ihl);
    else if (h->proto == IP_TCP && to_us && cfg.configured) tcp_input(h->src, h->dst, data + ihl, total - ihl);
    else goto drop; /* ICMP included: no echo replies, no error reports */
    return;
drop:
    dropped++;
}

/* ---- UDP: only for DHCP and DNS --------------------------------------------------------------- */

static int udp_send(uint32_t dst, uint16_t sport, uint16_t dport, const void *payload, size_t len) {
    static uint8_t buf[MTU];
    if (len + sizeof(struct udp_hdr) + sizeof(struct ip_hdr) > MTU) return -E_INVAL;
    struct udp_hdr *u = (struct udp_hdr *)buf;
    u->sport = htons(sport);
    u->dport = htons(dport);
    u->len = htons((uint16_t)(sizeof *u + len));
    u->csum = 0;
    memcpy(buf + sizeof *u, payload, len);
    u->csum = net_checksum(buf, sizeof *u + len, pseudo_sum(cfg.ip, dst, IP_UDP, (uint16_t)(sizeof *u + len)));
    if (!u->csum) u->csum = 0xFFFF;
    return ip_send(dst, IP_UDP, buf, sizeof *u + len);
}

/* ---- DHCP ---------------------------------------------------------------------------------------- */

#define DHCP_DISCOVER 1
#define DHCP_OFFER 2
#define DHCP_REQUEST 3
#define DHCP_ACK 5
#define DHCP_NAK 6

struct dhcp_msg {
    uint8_t op, htype, hlen, hops;
    uint32_t xid;
    uint16_t secs, flags;
    uint32_t ciaddr, yiaddr, siaddr, giaddr;
    uint8_t chaddr[16];
    uint8_t sname[64], file[128];
    uint32_t magic;
    uint8_t options[312];
} __attribute__((packed));

static struct {
    int state; /* 0 idle, 1 discovering, 2 requesting, 3 bound, 4 renewing */
    uint32_t xid, offered, server;
    uint64_t next_try;
    int tries;
} dhcp;

static void dhcp_send(int type) {
    struct dhcp_msg m;
    memset(&m, 0, sizeof m);
    m.op = 1;
    m.htype = 1;
    m.hlen = 6;
    m.xid = dhcp.xid;
    m.flags = htons(0x8000); /* broadcast the reply: we have no address yet */
    memcpy(m.chaddr, nic->mac, 6);
    m.magic = htonl(0x63825363);
    uint8_t *o = m.options;
    *o++ = 53, *o++ = 1, *o++ = (uint8_t)type;
    if (type == DHCP_REQUEST && dhcp.state == 2) {
        *o++ = 50, *o++ = 4, memcpy(o, &dhcp.offered, 4), o += 4;
        *o++ = 54, *o++ = 4, memcpy(o, &dhcp.server, 4), o += 4;
    }
    if (dhcp.state == 4) m.ciaddr = cfg.ip;
    *o++ = 55, *o++ = 4, *o++ = 1, *o++ = 3, *o++ = 6, *o++ = 51; /* mask, router, DNS, lease */
    *o++ = 12, *o++ = 5, memcpy(o, "aether", 5), o += 5;           /* host name */
    *o++ = 255;
    uint32_t saved = cfg.ip;
    if (dhcp.state != 4) cfg.ip = 0;
    udp_send(dhcp.state == 4 ? dhcp.server : 0xFFFFFFFFu, 68, 67, &m, sizeof m);
    cfg.ip = saved;
}

static void dhcp_start(void) {
    random_bytes(&dhcp.xid, sizeof dhcp.xid);
    dhcp.state = 1;
    dhcp.tries = 0;
    dhcp.next_try = 0;
}

/* Finds option `code`; returns its length (0 if absent) and sets *val. */
static int dhcp_opt(const struct dhcp_msg *m, size_t len, int code, const uint8_t **val) {
    size_t n = len - offsetof(struct dhcp_msg, options);
    const uint8_t *o = m->options;
    size_t i = 0;
    while (i < n) {
        uint8_t c = o[i];
        if (c == 255) break;
        if (c == 0) {
            i++;
            continue;
        }
        if (i + 1 >= n) break;
        uint8_t l = o[i + 1];
        if (i + 2 + l > n) break;
        if (c == code) {
            *val = o + i + 2;
            return l;
        }
        i += 2u + l;
    }
    return 0;
}

static uint32_t opt_u32(const uint8_t *v) {
    uint32_t x;
    memcpy(&x, v, 4);
    return x;
}

static void dhcp_input(const uint8_t *data, size_t len) {
    if (static_mode) return; /* a static address was chosen: ignore late DHCP answers */
    const struct dhcp_msg *m = (const struct dhcp_msg *)data;
    if (len < offsetof(struct dhcp_msg, options) || m->op != 2 || m->xid != dhcp.xid || memcmp(m->chaddr, nic->mac, 6) ||
        m->magic != htonl(0x63825363))
        return;
    const uint8_t *v;
    if (dhcp_opt(m, len, 53, &v) != 1) return;
    int type = v[0];
    uint32_t server = dhcp_opt(m, len, 54, &v) == 4 ? opt_u32(v) : 0;
    if (type == DHCP_OFFER && dhcp.state == 1) {
        if (!m->yiaddr || !server) return;
        dhcp.offered = m->yiaddr;
        dhcp.server = server;
        dhcp.state = 2;
        dhcp.tries = 0;
        dhcp_send(DHCP_REQUEST);
        dhcp.next_try = ticks + 2000;
    } else if (type == DHCP_ACK && (dhcp.state == 2 || dhcp.state == 4)) {
        if (server && server != dhcp.server) return;
        cfg.ip = m->yiaddr;
        cfg.mask = dhcp_opt(m, len, 1, &v) == 4 ? opt_u32(v) : htonl(0xFFFFFF00u);
        cfg.gw = dhcp_opt(m, len, 3, &v) >= 4 ? opt_u32(v) : 0;
        cfg.dns = dhcp_opt(m, len, 6, &v) >= 4 ? opt_u32(v) : cfg.gw;
        uint32_t lease = dhcp_opt(m, len, 51, &v) == 4 ? ntohl(opt_u32(v)) : 3600;
        if (lease < 60) lease = 60;
        if (lease > 7 * 86400) lease = 7 * 86400;
        cfg.lease_ms = (uint64_t)lease * 1000;
        cfg.renew_at = ticks + cfg.lease_ms / 2;
        if (!cfg.configured || dhcp.state == 2) {
            uint8_t *a = (uint8_t *)&cfg.ip, *g = (uint8_t *)&cfg.gw, *d = (uint8_t *)&cfg.dns;
            kprintf("[net] address %u.%u.%u.%u, router %u.%u.%u.%u, DNS %u.%u.%u.%u, lease %us\n", a[0], a[1], a[2],
                    a[3], g[0], g[1], g[2], g[3], d[0], d[1], d[2], d[3], lease);
        }
        cfg.configured = 1;
        dhcp.state = 3;
        if (cfg.gw) arp_send(1, 0, cfg.gw); /* have the router's address ready for the first connection */
    } else if (type == DHCP_NAK && (dhcp.state == 2 || dhcp.state == 4)) {
        kprintf("[net] DHCP server refused our address; starting over\n");
        cfg.configured = 0;
        dhcp_start();
    }
}

static void dhcp_timer(void) {
    if (dhcp.state == 3 && ticks >= cfg.renew_at) {
        dhcp.state = 4;
        dhcp.tries = 0;
        dhcp.next_try = 0;
    }
    if (dhcp.state == 3 || dhcp.state == 0 || ticks < dhcp.next_try) return;
    if (!nic->link_up()) {
        dhcp.next_try = ticks + 1000;
        return;
    }
    if (dhcp.state == 4 && ticks >= cfg.renew_at + cfg.lease_ms / 2) { /* the lease ran out */
        cfg.configured = 0;
        dhcp_start();
    }
    if (dhcp.state == 2 && ++dhcp.tries > 4) dhcp_start();
    dhcp_send(dhcp.state == 1 ? DHCP_DISCOVER : DHCP_REQUEST);
    dhcp.next_try = ticks + (dhcp.state == 4 ? 30000 : 2000 + (random_u64() % 1000));
}

/* ---- DNS --------------------------------------------------------------------------------------- */

#define DNS_QUERIES 8
#define DNS_CACHE 32

static struct {
    int busy, done;
    uint16_t id, port;
    char name[NET_HOST_MAX];
    uint32_t ip, ttl;
    struct proc *waiter;
} dnsq[DNS_QUERIES];

static struct {
    char name[NET_HOST_MAX];
    uint32_t ip;
    uint64_t until;
} dns_cache[DNS_CACHE];

static int lower(int c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; }

static int name_eq(const char *a, const char *b) {
    for (; *a && *b; a++, b++)
        if (lower(*a) != lower(*b)) return 0;
    return *a == *b;
}

int net_parse_ipv4(const char *s, uint32_t *ip) {
    uint8_t b[4];
    for (int i = 0; i < 4; i++) {
        if (*s < '0' || *s > '9') return -1;
        unsigned v = 0;
        int digits = 0;
        while (*s >= '0' && *s <= '9') {
            v = v * 10 + (unsigned)(*s++ - '0');
            if (++digits > 3 || v > 255) return -1;
        }
        b[i] = (uint8_t)v;
        if (i < 3 && *s++ != '.') return -1;
    }
    if (*s) return -1;
    memcpy(ip, b, 4);
    return 0;
}

/* Letters, digits and hyphens in dot-separated labels of 1..63 bytes; or an IPv4 literal. */
int net_host_ok(const char *host) {
    size_t n = strlen(host);
    if (!n || n >= NET_HOST_MAX) return 0;
    uint32_t ip;
    if (!net_parse_ipv4(host, &ip)) return 1;
    size_t label = 0;
    int letters = 0;
    for (size_t i = 0; i <= n; i++) {
        char c = host[i];
        if (c == '.' || c == 0) {
            if (!label || label > 63 || host[i - 1] == '-') return 0;
            label = 0;
            continue;
        }
        if (!label && c == '-') return 0;
        if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-')) return 0;
        if (!(c >= '0' && c <= '9')) letters = 1;
        label++;
    }
    return letters; /* "1.2.3" isn't a name */
}

int dns_build_query(uint8_t *out, size_t max, const char *name, uint16_t id) {
    size_t n = strlen(name);
    if (12 + n + 2 + 4 > max) return -1;
    memset(out, 0, 12);
    out[0] = (uint8_t)(id >> 8);
    out[1] = (uint8_t)id;
    out[2] = 0x01; /* recursion desired */
    out[5] = 1;    /* one question */
    size_t o = 12;
    const char *s = name;
    while (*s) {
        const char *e = s;
        while (*e && *e != '.') e++;
        size_t l = (size_t)(e - s);
        if (!l || l > 63) return -1;
        out[o++] = (uint8_t)l;
        for (size_t i = 0; i < l; i++) out[o++] = (uint8_t)lower(s[i]);
        s = *e ? e + 1 : e;
    }
    out[o++] = 0;
    out[o++] = 0, out[o++] = 1; /* type A */
    out[o++] = 0, out[o++] = 1; /* class IN */
    return (int)o;
}

/* Skips a (possibly compressed) name; returns the offset after it, or -1. */
static int dns_skip_name(const uint8_t *msg, size_t len, size_t off) {
    for (int guard = 0; guard < 128; guard++) {
        if (off >= len) return -1;
        uint8_t l = msg[off];
        if ((l & 0xC0) == 0xC0) return off + 2 <= len ? (int)(off + 2) : -1;
        if (l & 0xC0) return -1;
        off += 1u + l;
        if (!l) return (int)off;
    }
    return -1;
}

/* Compares the name at `off` (following pointers, with a hop limit) to `name`. */
static int dns_name_is(const uint8_t *msg, size_t len, size_t off, const char *name) {
    const char *s = name;
    for (int hops = 0; hops < 16;) {
        if (off >= len) return 0;
        uint8_t l = msg[off];
        if ((l & 0xC0) == 0xC0) {
            if (off + 1 >= len) return 0;
            off = (size_t)(l & 0x3F) << 8 | msg[off + 1];
            hops++;
            continue;
        }
        if (l & 0xC0) return 0;
        if (!l) return *s == 0;
        if (off + 1 + l > len) return 0;
        if (s != name) {
            if (*s != '.') return 0;
            s++;
        }
        for (uint8_t i = 0; i < l; i++, s++)
            if (!*s || lower(*s) != lower(msg[off + 1 + i])) return 0;
        off += 1u + l;
    }
    return 0;
}

/* Decode a (possibly compressed) DNS name at `off` into dotted text. */
static int dns_name_read(const uint8_t *msg, size_t len, size_t off, char *out, size_t outn) {
    size_t n = 0;
    for (int hops = 0; hops < 16;) {
        if (off >= len) return -1;
        uint8_t l = msg[off];
        if ((l & 0xC0) == 0xC0) {
            if (off + 1 >= len) return -1;
            off = (size_t)(l & 0x3F) << 8 | msg[off + 1];
            hops++;
            continue;
        }
        if (l & 0xC0) return -1;
        if (!l) {
            out[n] = 0;
            return 0;
        }
        if (off + 1 + l > len) return -1;
        if (n && n + 1 < outn) out[n++] = '.';
        for (uint8_t i = 0; i < l; i++) {
            if (n + 1 >= outn) return -1;
            out[n++] = (char)lower(msg[off + 1 + i]);
        }
        off += 1u + l;
    }
    return -1;
}

/* The first A record for `name` in a reply to query `id`. 0, or -1 for a
 * malformed or mismatched reply, -2 for "no such name". */
int dns_parse_reply(const uint8_t *msg, size_t len, uint16_t id, const char *name, uint32_t *ip, uint32_t *ttl) {
    if (len < 12 || ((uint16_t)msg[0] << 8 | msg[1]) != id) return -1;
    if (!(msg[2] & 0x80) || (msg[2] & 0x78)) return -1; /* must be a standard-query response */
    int rcode = msg[3] & 15;
    unsigned qd = (unsigned)msg[4] << 8 | msg[5], an = (unsigned)msg[6] << 8 | msg[7];
    if (qd != 1) return -1;
    if (!dns_name_is(msg, len, 12, name)) return -1; /* the question has to be ours */
    int off = dns_skip_name(msg, len, 12);
    if (off < 0 || (size_t)off + 4 > len) return -1;
    if (msg[off] != 0 || msg[off + 1] != 1 || msg[off + 2] != 0 || msg[off + 3] != 1) return -1;
    off += 4;
    if (rcode == 3) return -2;
    if (rcode) return -1;
    /* Answers: accept an A record only for the name we asked about, or for a
     * target reached through the answer's CNAME chain. An unrelated A record
     * in the same reply must not be treated as this name's address. */
    char names[8][NET_HOST_MAX];
    int nnames = 1;
    strlcpy(names[0], name, sizeof names[0]);
    for (unsigned i = 0; i < an && i < 32; i++) {
        size_t owner = (size_t)off;
        off = dns_skip_name(msg, len, owner);
        if (off < 0 || (size_t)off + 10 > len) return -1;
        uint16_t type = (uint16_t)(msg[off] << 8 | msg[off + 1]), cls = (uint16_t)(msg[off + 2] << 8 | msg[off + 3]);
        uint32_t t = (uint32_t)msg[off + 4] << 24 | (uint32_t)msg[off + 5] << 16 | (uint32_t)msg[off + 6] << 8 | msg[off + 7];
        uint16_t rdlen = (uint16_t)(msg[off + 8] << 8 | msg[off + 9]);
        off += 10;
        if ((size_t)off + rdlen > len) return -1;
        int owner_ok = 0;
        for (int k = 0; k < nnames; k++)
            if (dns_name_is(msg, len, owner, names[k])) {
                owner_ok = 1;
                break;
            }
        if (type == 1 && cls == 1 && rdlen == 4 && owner_ok) {
            memcpy(ip, msg + off, 4);
            *ttl = t;
            return 0;
        }
        if (type == 5 && cls == 1 && owner_ok && nnames < (int)ARRAY_LEN(names)) {
            char target[NET_HOST_MAX];
            if (!dns_name_read(msg, len, (size_t)off, target, sizeof target)) strlcpy(names[nnames++], target, NET_HOST_MAX);
        }
        off += rdlen;
    }
    return -2;
}

static void dns_input(uint32_t src, uint16_t dport, const uint8_t *data, size_t len) {
    if (src != cfg.dns) return;
    for (int i = 0; i < DNS_QUERIES; i++) {
        if (!dnsq[i].busy || dnsq[i].done || dnsq[i].port != dport) continue;
        uint32_t ip = 0, ttl = 0;
        int r = dns_parse_reply(data, len, dnsq[i].id, dnsq[i].name, &ip, &ttl);
        if (r == -1) continue; /* this port may belong to another query: keep looking */
        dnsq[i].done = r == 0 ? 1 : 2;
        dnsq[i].ip = ip;
        dnsq[i].ttl = ttl;
        if (dnsq[i].waiter) proc_wake_kind(dnsq[i].waiter, WAIT_NET);
        return;
    }
}

static void udp_input(uint32_t src, uint32_t dst, const uint8_t *data, size_t len) {
    const struct udp_hdr *u = (const struct udp_hdr *)data;
    if (len < sizeof *u) goto drop;
    size_t ulen = ntohs(u->len);
    if (ulen < sizeof *u || ulen > len || ulen > 0xFFFF) goto drop;
    /* A UDP checksum of zero means "not computed" in IPv4, but DHCP and DNS
     * always compute one. Checking it rejects corrupted and spoofed replies
     * that guessed the transaction fields but not the checksum. */
    if (u->csum && net_checksum(data, ulen, pseudo_sum(src, dst, IP_UDP, (uint16_t)ulen)) != 0) goto drop;
    uint16_t sport = ntohs(u->sport), dport = ntohs(u->dport);
    if (sport == 67 && dport == 68) dhcp_input(data + sizeof *u, ulen - sizeof *u);
    else if (sport == 53 && cfg.configured) dns_input(src, dport, data + sizeof *u, ulen - sizeof *u);
    else goto drop;
    return;
drop:
    dropped++;
}

static void block_net(int64_t ms);

/* Resolves `name` for the calling process: 0 and *ip, or -E_NET / -E_TIMEOUT. */
static int dns_resolve(const char *name, uint32_t *ip, uint64_t deadline) {
    for (int i = 0; i < DNS_CACHE; i++)
        if (dns_cache[i].until > ticks && name_eq(dns_cache[i].name, name)) {
            *ip = dns_cache[i].ip;
            return 0;
        }
    int q = -1;
    for (int i = 0; i < DNS_QUERIES; i++)
        if (!dnsq[i].busy) {
            q = i;
            break;
        }
    if (q < 0) return -E_LIMIT;
    dnsq[q].busy = 1;
    dnsq[q].done = 0;
    dnsq[q].waiter = current;
    strlcpy(dnsq[q].name, name, sizeof dnsq[q].name);
    int err = -E_TIMEOUT;
    for (int attempt = 0; attempt < 3 && ticks < deadline; attempt++) {
        /* A fresh random id and source port each time, so a forged answer has
         * to guess 32 bits while the real one is on its way. */
        uint32_t r = (uint32_t)random_u64();
        dnsq[q].id = (uint16_t)r;
        dnsq[q].port = (uint16_t)(20000 + (r >> 16) % 40000);
        uint8_t pkt[NET_HOST_MAX + 32];
        int n = dns_build_query(pkt, sizeof pkt, name, dnsq[q].id);
        if (n < 0) {
            err = -E_INVAL;
            break;
        }
        udp_send(cfg.dns, dnsq[q].port, 53, pkt, (size_t)n);
        uint64_t until = MIN(deadline, ticks + 1500);
        while (!dnsq[q].done && ticks < until) {
            if (current->state == PROC_ZOMBIE) break;
            block_net((int64_t)(until - ticks));
            net_poll();
        }
        if (dnsq[q].done == 1) {
            *ip = dnsq[q].ip;
            uint32_t ttl = MAX(30u, MIN(dnsq[q].ttl, 3600u));
            int slot = 0;
            for (int i = 0; i < DNS_CACHE; i++)
                if (dns_cache[i].until < dns_cache[slot].until) slot = i;
            strlcpy(dns_cache[slot].name, name, sizeof dns_cache[slot].name);
            dns_cache[slot].ip = *ip;
            dns_cache[slot].until = ticks + (uint64_t)ttl * 1000;
            err = 0;
            break;
        }
        if (dnsq[q].done == 2) {
            err = -E_NET;
            break;
        }
    }
    dnsq[q].busy = 0;
    dnsq[q].waiter = 0;
    return err;
}

/* ---- Address policy ---------------------------------------------------------------------------- */

int net_addr_allowed(uint32_t nip, int literal) {
    uint32_t ip = ntohl(nip);
    uint8_t a = (uint8_t)(ip >> 24), b = (uint8_t)(ip >> 16), c = (uint8_t)(ip >> 8);
    if (a == 0 || a == 127 || a >= 224) return 0;    /* this host, loopback, multicast, reserved, broadcast */
    if (a == 169 && b == 254) return 0;              /* link-local (and cloud metadata services) */
    /* Other IANA special-purpose ranges, including cloud metadata endpoints
     * (192.0.0.192), benchmarking and 6to4 relay. These are never public web
     * sites and must not be reached through a DNS name; a literal typed by
     * the user still goes through the firewall. TEST-NET ranges are omitted:
     * they are never assigned on real networks and are used by local test
     * harnesses, so a name resolving there reaches only a lab. */
    int special = (a == 192 && b == 0 && c == 0) ||
                  (a == 192 && b == 88 && c == 99) ||
                  (a == 198 && (b == 18 || b == 19));
    int lan = a == 10 || (a == 172 && (b & 0xF0) == 16) || (a == 192 && b == 168) || (a == 100 && (b & 0xC0) == 64);
    return (special || lan) ? literal : 1; /* a name resolving here is a rebinding attack until proven otherwise */
}

/* ---- TCP --------------------------------------------------------------------------------------- */

enum { T_FREE, T_SYN_SENT, T_ESTABLISHED, T_FIN_WAIT1, T_FIN_WAIT2, T_CLOSE_WAIT, T_CLOSING, T_LAST_ACK, T_TIME_WAIT };

#define MAX_CONNS 64
#define CONNS_PER_PROC 16
#define RXBUF (64 * 1024)
#define TXBUF (64 * 1024)
#define RTO_INIT 1000
#define RTO_MAX 30000
#define MAX_RETRIES 8
#define TIME_WAIT_MS 2000
#define LINGER_MS 20000 /* an app-closed connection gets this long to finish its goodbye */
#define DEFAULT_MSS 536

struct tcb {
    int state;
    struct proc *owner; /* 0 once the app closed it */
    int handle;
    uint32_t gen; /* distinguishes a reused slot/handle from the old connection */
    uint32_t rip;
    uint16_t lport, rport;
    uint32_t iss, snd_una, snd_nxt, snd_wnd, rcv_nxt;
    uint16_t mss;
    uint8_t *rx, *tx;
    uint32_t rx_head, rx_len, tx_len;
    uint32_t rto;
    uint64_t rto_at, expire_at;
    int retries;
    int fin_queued, fin_sent, peer_fin, error;
    uint32_t last_wnd; /* the window we last advertised */
};

static struct tcb conns[MAX_CONNS];

static uint32_t rx_space(const struct tcb *c) { return RXBUF - c->rx_len; }

static void tcp_send_seg(struct tcb *c, uint32_t seq, uint8_t flags, const uint8_t *data, uint32_t len) {
    uint8_t seg[sizeof(struct tcp_hdr) + 4 + 1460];
    struct tcp_hdr *t = (struct tcp_hdr *)seg;
    size_t hl = sizeof *t + ((flags & TCP_SYN) ? 4 : 0);
    if (len > sizeof seg - hl) len = (uint32_t)(sizeof seg - hl);
    t->sport = htons(c->lport);
    t->dport = htons(c->rport);
    t->seq = htonl(seq);
    t->ack = (flags & TCP_ACK) ? htonl(c->rcv_nxt) : 0;
    t->off = (uint8_t)((hl / 4) << 4);
    t->flags = flags;
    uint32_t wnd = MIN(rx_space(c), 65535u);
    c->last_wnd = wnd;
    t->win = htons((uint16_t)wnd);
    t->csum = 0;
    t->urg = 0;
    if (flags & TCP_SYN) { /* MSS option: what fits in our MTU */
        seg[sizeof *t] = 2;
        seg[sizeof *t + 1] = 4;
        seg[sizeof *t + 2] = (MTU - 40) >> 8;
        seg[sizeof *t + 3] = (MTU - 40) & 0xFF;
    }
    if (len) memcpy(seg + hl, data, len);
    t->csum = net_checksum(seg, hl + len, pseudo_sum(cfg.ip, c->rip, IP_TCP, (uint16_t)(hl + len)));
    ip_send(c->rip, IP_TCP, seg, hl + len);
}

static void tcp_ack(struct tcb *c) { tcp_send_seg(c, c->snd_nxt, TCP_ACK, 0, 0); }

static void conn_wake(struct tcb *c) {
    if (c->owner) proc_wake_kind(c->owner, WAIT_NET);
}

static void conn_free(struct tcb *c) {
    if (c->rx) kfree(c->rx);
    if (c->tx) kfree(c->tx);
    memset(c, 0, sizeof *c);
}

static void conn_fail(struct tcb *c, int err) {
    c->error = err;
    if (c->owner) {
        c->state = T_CLOSING; /* keeps the slot until the app closes its handle */
        c->expire_at = 0;
        conn_wake(c);
    } else {
        conn_free(c);
    }
}

static void timer_arm(struct tcb *c) {
    if (!c->rto_at) c->rto_at = ticks + c->rto;
}

/* Sends what the window allows, then a FIN once everything queued is out. */
static void tcp_output(struct tcb *c, int probe) {
    if (c->error) return;
    if (c->state != T_ESTABLISHED && c->state != T_CLOSE_WAIT && c->state != T_FIN_WAIT1 && c->state != T_CLOSING &&
        c->state != T_LAST_ACK)
        return;
    for (;;) {
        uint32_t inflight = c->snd_nxt - c->snd_una - (c->fin_sent ? 1u : 0u);
        if (inflight >= c->tx_len || c->fin_sent) break;
        uint32_t avail = c->tx_len - inflight;
        uint32_t wnd = c->snd_wnd > inflight ? c->snd_wnd - inflight : 0;
        if (!wnd && probe && !inflight) wnd = 1; /* zero-window probe */
        uint32_t n = MIN(MIN(avail, wnd), (uint32_t)c->mss);
        if (!n) break;
        tcp_send_seg(c, c->snd_nxt, TCP_ACK | TCP_PSH, c->tx + inflight, n);
        c->snd_nxt += n;
        timer_arm(c);
        probe = 0;
    }
    uint32_t inflight = c->snd_nxt - c->snd_una;
    if (c->fin_queued && !c->fin_sent && inflight == c->tx_len) {
        tcp_send_seg(c, c->snd_nxt, TCP_FIN | TCP_ACK, 0, 0);
        c->snd_nxt++;
        c->fin_sent = 1;
        timer_arm(c);
        if (c->state == T_ESTABLISHED) c->state = T_FIN_WAIT1;
        else if (c->state == T_CLOSE_WAIT) c->state = T_LAST_ACK;
    }
}

static struct tcb *conn_find(uint32_t rip, uint16_t rport, uint16_t lport) {
    for (int i = 0; i < MAX_CONNS; i++)
        if (conns[i].state != T_FREE && conns[i].rip == rip && conns[i].rport == rport && conns[i].lport == lport)
            return &conns[i];
    return 0;
}

static int seq_lt(uint32_t a, uint32_t b) { return (int32_t)(a - b) < 0; }
static int seq_le(uint32_t a, uint32_t b) { return (int32_t)(a - b) <= 0; }

static void tcp_input(uint32_t src, uint32_t dst, const uint8_t *data, size_t len) {
    const struct tcp_hdr *t = (const struct tcp_hdr *)data;
    if (len < sizeof *t) goto drop;
    size_t hl = (size_t)(t->off >> 4) * 4;
    if (hl < sizeof *t || hl > len) goto drop;
    if (net_checksum(data, len, pseudo_sum(src, dst, IP_TCP, (uint16_t)len)) != 0) goto drop;
    struct tcb *c = conn_find(src, ntohs(t->sport), ntohs(t->dport));
    if (!c || c->error) goto drop; /* no listener, no answer */
    uint8_t fl = t->flags;
    uint32_t seq = ntohl(t->seq), ack = ntohl(t->ack);
    const uint8_t *payload = data + hl;
    uint32_t plen = (uint32_t)(len - hl);

    if (c->state == T_SYN_SENT) {
        if ((fl & TCP_ACK) && ack != c->iss + 1) goto drop;
        if (fl & TCP_RST) {
            if (fl & TCP_ACK) conn_fail(c, -E_NET); /* connection refused */
            return;
        }
        if (!(fl & TCP_SYN) || !(fl & TCP_ACK)) goto drop;
        c->rcv_nxt = seq + 1;
        c->snd_una = ack;
        c->snd_wnd = ntohs(t->win);
        c->mss = DEFAULT_MSS;
        for (size_t o = sizeof *t; o < hl;) { /* options: only MSS matters to us */
            uint8_t kind = data[o];
            if (kind == 0) break;
            if (kind == 1) {
                o++;
                continue;
            }
            if (o + 1 >= hl || data[o + 1] < 2 || o + data[o + 1] > hl) break;
            if (kind == 2 && data[o + 1] == 4) {
                uint16_t m = (uint16_t)(data[o + 2] << 8 | data[o + 3]);
                c->mss = (uint16_t)MAX(64, MIN(m, MTU - 40));
            }
            o += data[o + 1];
        }
        c->state = T_ESTABLISHED;
        c->retries = 0;
        c->rto = RTO_INIT;
        c->rto_at = 0;
        tcp_ack(c);
        conn_wake(c);
        return;
    }

    /* Synchronized states. A segment must overlap what we're willing to
     * receive (RFC 793's acceptability test, with a zero window meaning
     * "exactly rcv_nxt"). */
    uint32_t wnd = MAX(c->last_wnd, 1u);
    int acceptable = plen ? (seq_le(c->rcv_nxt, seq + plen - 1) && seq_lt(seq, c->rcv_nxt + wnd))
                          : (seq_le(c->rcv_nxt, seq) && seq_lt(seq, c->rcv_nxt + wnd));
    if (fl & TCP_RST) {
        /* RFC 5961: only an exact match resets; an in-window guess gets a challenge ACK. */
        if (seq == c->rcv_nxt) conn_fail(c, -E_NET);
        else if (acceptable) tcp_ack(c);
        return;
    }
    if (fl & TCP_SYN) {
        tcp_ack(c); /* challenge ACK, never a reset */
        return;
    }
    if (!acceptable) {
        if (c->state != T_TIME_WAIT || plen || (fl & TCP_FIN)) tcp_ack(c);
        return;
    }
    if (!(fl & TCP_ACK)) goto drop;

    /* ACK */
    if (seq_lt(c->snd_nxt, ack)) { /* acks something we never sent */
        tcp_ack(c);
        return;
    }
    if (seq_lt(c->snd_una, ack)) {
        uint32_t acked = ack - c->snd_una;
        int fin_acked = c->fin_sent && ack == c->snd_nxt;
        uint32_t data_acked = MIN(acked - (fin_acked ? 1u : 0u), c->tx_len);
        memmove(c->tx, c->tx + data_acked, c->tx_len - data_acked);
        c->tx_len -= data_acked;
        c->snd_una = ack;
        c->retries = 0;
        c->rto = RTO_INIT;
        c->rto_at = c->snd_una != c->snd_nxt ? ticks + c->rto : 0;
        if (fin_acked) {
            if (c->state == T_FIN_WAIT1) c->state = T_FIN_WAIT2;
            else if (c->state == T_CLOSING) {
                c->state = T_TIME_WAIT;
                c->expire_at = ticks + TIME_WAIT_MS;
            } else if (c->state == T_LAST_ACK) {
                if (c->owner) c->state = T_CLOSING, c->peer_fin = 1, c->expire_at = 0;
                else {
                    conn_free(c);
                    return;
                }
            }
        }
        conn_wake(c);
    }
    if (seq_le(c->snd_una, ack)) c->snd_wnd = ntohs(t->win);

    /* Data, in order only; anything ahead of rcv_nxt is dropped and the
     * duplicate ACK makes the peer resend. */
    if (plen && c->state != T_TIME_WAIT && !c->peer_fin) {
        uint32_t skip = c->rcv_nxt - seq; /* overlap with what we already have */
        if (skip < plen) {
            uint32_t n = MIN(plen - skip, rx_space(c));
            for (uint32_t i = 0; i < n; i++) c->rx[(c->rx_head + c->rx_len + i) % RXBUF] = payload[skip + i];
            c->rx_len += n;
            c->rcv_nxt += n;
            if (n < plen - skip) fl &= (uint8_t)~TCP_FIN; /* we didn't take it all, so the FIN isn't next yet */
            if (n) conn_wake(c);
        }
        if (!c->owner) { /* the app is gone: nobody will read this */
            tcp_send_seg(c, c->snd_nxt, TCP_RST | TCP_ACK, 0, 0);
            conn_free(c);
            return;
        }
    }
    if ((fl & TCP_FIN) && seq + plen == c->rcv_nxt && !c->peer_fin) {
        c->rcv_nxt++;
        c->peer_fin = 1;
        if (c->state == T_ESTABLISHED) c->state = T_CLOSE_WAIT;
        else if (c->state == T_FIN_WAIT1) c->state = T_CLOSING;
        else if (c->state == T_FIN_WAIT2) {
            c->state = T_TIME_WAIT;
            c->expire_at = ticks + TIME_WAIT_MS;
        }
        conn_wake(c);
    }
    if (plen || (fl & TCP_FIN)) tcp_ack(c);
    tcp_output(c, 0);
    return;
drop:
    dropped++;
}

static void tcp_timers(void) {
    for (int i = 0; i < MAX_CONNS; i++) {
        struct tcb *c = &conns[i];
        if (c->state == T_FREE) continue;
        if (c->expire_at && ticks >= c->expire_at) {
            if (c->owner && c->state == T_TIME_WAIT) { /* keep it for the app; just stop timing */
                c->expire_at = 0;
                continue;
            }
            if (!c->owner) {
                if (c->state != T_TIME_WAIT) tcp_send_seg(c, c->snd_nxt, TCP_RST | TCP_ACK, 0, 0);
                conn_free(c);
                continue;
            }
        }
        if (!c->rto_at || ticks < c->rto_at || c->error) continue;
        c->rto_at = 0;
        if (++c->retries > (c->state == T_SYN_SENT ? 5 : MAX_RETRIES)) {
            if (c->state != T_SYN_SENT) tcp_send_seg(c, c->snd_nxt, TCP_RST | TCP_ACK, 0, 0);
            conn_fail(c, -E_TIMEOUT);
            continue;
        }
        c->rto = MIN(c->rto * 2, (uint32_t)RTO_MAX);
        if (c->state == T_SYN_SENT) {
            tcp_send_seg(c, c->iss, TCP_SYN, 0, 0);
            timer_arm(c);
            continue;
        }
        /* Go back to the oldest unacknowledged byte and send again. */
        int probe = c->snd_una == c->snd_nxt; /* nothing in flight: the window must be shut */
        c->snd_nxt = c->snd_una;
        if (c->fin_sent) {
            c->fin_sent = 0;
            if (c->state == T_FIN_WAIT1) c->state = T_ESTABLISHED;
            else if (c->state == T_LAST_ACK) c->state = T_CLOSE_WAIT;
            else if (c->state == T_CLOSING && !c->error) c->state = T_CLOSE_WAIT; /* both FINs crossed */
        }
        tcp_output(c, probe);
        if (!c->rto_at && (c->tx_len || c->fin_queued)) timer_arm(c);
    }
}

/* ---- Polling --------------------------------------------------------------------------------- */

static uint64_t last_timer;

void net_poll(void) {
    if (!nic) return;
    static uint8_t frame[FRAME_MAX + 64];
    for (int budget = 0; budget < 64; budget++) {
        int n = nic->rx(frame, sizeof frame);
        if (n == 0) break;
        if (n < 0) {
            dropped++;
            continue;
        }
        rx_packets++;
        if (n < (int)sizeof(struct eth_hdr)) {
            dropped++;
            continue;
        }
        const struct eth_hdr *e = (const struct eth_hdr *)frame;
        if (memcmp(e->dst, nic->mac, 6) && memcmp(e->dst, bcast_mac, 6)) {
            dropped++;
            continue;
        }
        uint16_t type = ntohs(e->type);
        if (type == ETH_ARP) arp_input((const struct arp_pkt *)(frame + sizeof *e), (size_t)n - sizeof *e);
        else if (type == ETH_IP) ip_input(frame + sizeof *e, (size_t)n - sizeof *e);
        else dropped++;
    }
    if (ticks - last_timer >= 10) {
        last_timer = ticks;
        dhcp_timer();
        tcp_timers();
    }
}

int net_state(void) { return !nic ? 0 : cfg.configured ? 2 : 1; }

void net_get_config(struct net_config *out) {
    memset(out, 0, sizeof *out);
    out->mode = static_mode ? NET_MODE_STATIC : NET_MODE_DHCP;
    out->ip = cfg.ip;
    out->netmask = cfg.mask;
    out->gateway = cfg.gw;
    out->dns = cfg.dns;
}

int net_apply_config(const struct net_config *in) {
    if (!nic) return -E_NOTSUP;
    if (in->mode == NET_MODE_DHCP) {
        static_mode = 0;
        cfg.configured = 0;
        cfg.ip = cfg.mask = cfg.gw = cfg.dns = 0;
        cfg.renew_at = 0;
        dhcp_start();
        kprintf("[net] using DHCP\n");
        return 0;
    }
    if (in->mode != NET_MODE_STATIC || !in->ip || !in->netmask) return -E_INVAL;
    static_mode = 1;
    dhcp.state = 0; /* stop the DHCP timers */
    cfg.ip = in->ip;
    cfg.mask = in->netmask;
    cfg.gw = in->gateway;
    cfg.dns = in->dns ? in->dns : in->gateway;
    cfg.configured = 1;
    cfg.renew_at = 0;
    if (cfg.gw) arp_send(1, 0, cfg.gw);
    uint8_t *a = (uint8_t *)&cfg.ip, *m = (uint8_t *)&cfg.mask, *g = (uint8_t *)&cfg.gw, *d = (uint8_t *)&cfg.dns;
    kprintf("[net] static %u.%u.%u.%u mask %u.%u.%u.%u gateway %u.%u.%u.%u dns %u.%u.%u.%u\n", a[0], a[1], a[2],
            a[3], m[0], m[1], m[2], m[3], g[0], g[1], g[2], g[3], d[0], d[1], d[2], d[3]);
    return 0;
}

void net_init(void) {
    nic = e1000_init();
    if (!nic) nic = usb_net_init();
    if (!nic) {
        kprintf("[net] no supported network card\n");
        return;
    }
    dhcp_start();
}

/* ---- System calls ------------------------------------------------------------------------------ */

static void block_net(int64_t ms) {
    if (ms <= 0) return;
    proc_block(WAIT_NET, MIN(ms, 20)); /* wake at least every 20 ms to poll the card */
}

static uint32_t next_conn_gen = 1;

static struct tcb *conn_of_gen(uint64_t h, uint32_t *gen) {
    if (!h || h > CONNS_PER_PROC) return 0;
    for (int i = 0; i < MAX_CONNS; i++)
        if (conns[i].state != T_FREE && conns[i].owner == current && conns[i].handle == (int)h) {
            if (gen) *gen = conns[i].gen;
            return &conns[i];
        }
    return 0;
}

static struct tcb *conn_of(uint64_t h) { return conn_of_gen(h, 0); }

/* Hosts this process may reach without asking again, from "Allow once". */
#define ONCE_SLOTS 32
static struct {
    struct proc *p;
    int pid;
    char host[NET_HOST_MAX];
    uint16_t port;
} once[ONCE_SLOTS];

static int once_allowed(const char *host, uint16_t port) {
    for (int i = 0; i < ONCE_SLOTS; i++)
        if (once[i].p == current && once[i].pid == current->pid && once[i].port == port && name_eq(once[i].host, host))
            return 1;
    return 0;
}

static void once_add(const char *host, uint16_t port) {
    int slot = 0;
    for (int i = 0; i < ONCE_SLOTS; i++)
        if (!once[i].p) {
            slot = i;
            break;
        }
    once[slot].p = current;
    once[slot].pid = current->pid;
    once[slot].port = port;
    strlcpy(once[slot].host, host, sizeof once[slot].host);
}

/* The firewall's verdict for this process, asking the user when no rule decides.
 * `allow_builtin` is false for a non-public IP literal: the browser's silent
 * web policy must not let remote page content reach the router or a local
 * service, so those destinations always need a user decision. */
static int firewall_decide(const char *host, uint16_t port, uint64_t deadline, int allow_builtin) {
    int v = fw_check(current->name, host, port, allow_builtin);
    if (v >= 0) return v;
    if (allow_builtin && once_allowed(host, port)) return 1;
    for (;;) {
        cur_thread->dialog_done = 0;
        int r = wm_dialog_net(current, host, port);
        if (r == 0) break;
        if (r != -E_LIMIT || ticks >= deadline) return 0;
        proc_block(WAIT_SLEEP, 100); /* another dialog is up: wait our turn */
        if (current->state == PROC_ZOMBIE) return 0;
    }
    while (!cur_thread->dialog_done) {
        proc_block(WAIT_DIALOG, -1);
        if (current->state == PROC_ZOMBIE) return 0;
    }
    switch (cur_thread->dialog_result) {
    case 1: once_add(host, port); return 1;
    case 2: fw_remember(current->name, host, port, 1); return 1;
    default: return 0;
    }
}

int64_t sys_net_connect(uint64_t uhost, uint64_t len, uint64_t uport, uint64_t timeout_ms) {
    if (!(current->caps & CAP_NET)) return -E_PERM;
    if (!len || len >= NET_HOST_MAX || !uport || uport > 65535) return -E_INVAL;
    char host[NET_HOST_MAX];
    if (copy_in(host, uhost, len)) return -E_FAULT;
    host[len] = 0;
    if (strlen(host) != len) return -E_INVAL;
    if (len > 1 && host[len - 1] == '.') host[--len] = 0;
    for (size_t i = 0; i < len; i++) host[i] = (char)lower(host[i]);
    if (!net_host_ok(host)) return -E_INVAL;
    uint16_t port = (uint16_t)uport;
    uint64_t deadline = ticks + (timeout_ms ? MIN(timeout_ms, 120000u) : 30000u);

    if (dlp_egress(current, DLP_EGRESS_NET)) return -E_DENIED;
    if (!nic) return -E_NET;
    while (!cfg.configured && nic->link_up() && ticks < MIN(deadline, ticks + 8000)) {
        block_net(50);
        net_poll();
        if (current->state == PROC_ZOMBIE) return -E_NET;
    }
    if (!cfg.configured) return -E_NET;

    int nconn = 0;
    for (int i = 0; i < MAX_CONNS; i++)
        if (conns[i].state != T_FREE && conns[i].owner == current) nconn++;
    if (nconn >= CONNS_PER_PROC) return -E_LIMIT;

    /* Parse and normalize a literal before the firewall sees it, so a rule
     * for 1.2.3.4 also matches 01.2.3.4. Loopback/link-local/multicast are
     * refused outright; LAN and special-use literals get an explicit user
     * prompt instead of the browser's silent policy. */
    uint32_t ip = 0;
    int literal = !net_parse_ipv4(host, &ip);
    int non_public_literal = 0;
    if (literal) {
        uint8_t *b = (uint8_t *)&ip;
        if (!net_addr_allowed(ip, 1)) return -E_DENIED;
        non_public_literal = !net_addr_allowed(ip, 0);
        snprintf(host, NET_HOST_MAX, "%u.%u.%u.%u", b[0], b[1], b[2], b[3]);
    }
    if (!firewall_decide(host, port, deadline, !non_public_literal)) {
        kprintf("[fw] blocked %s -> %s:%u\n", current->name, host, port);
        return -E_DENIED;
    }

    if (!literal) {
        int err = dns_resolve(host, &ip, deadline);
        if (err) return err == -E_TIMEOUT ? -E_NET : err;
    }
    if (!net_addr_allowed(ip, literal)) {
        uint8_t *b = (uint8_t *)&ip;
        kprintf("[fw] blocked %s -> %s (%u.%u.%u.%u): not a public address\n", current->name, host, b[0], b[1], b[2],
                b[3]);
        return -E_DENIED;
    }
    /* The checks above can wait for DHCP, the user's firewall dialog, and DNS.
     * Another thread may have read Confidential data in the meantime. */
    if (dlp_egress(current, DLP_EGRESS_NET)) return -E_DENIED;

    /* The firewall and DNS may have blocked for seconds; another thread of
     * this process may have opened connections in the meantime. */
    nconn = 0;
    for (int i = 0; i < MAX_CONNS; i++)
        if (conns[i].state != T_FREE && conns[i].owner == current) nconn++;
    if (nconn >= CONNS_PER_PROC) return -E_LIMIT;

    struct tcb *c = 0;
    for (int i = 0; i < MAX_CONNS; i++)
        if (conns[i].state == T_FREE) {
            c = &conns[i];
            break;
        }
    if (!c) return -E_LIMIT;
    int h = 1;
    while (conn_of((uint64_t)h)) h++;
    if (h > CONNS_PER_PROC) return -E_LIMIT;
    memset(c, 0, sizeof *c);
    c->gen = next_conn_gen++;
    if (!next_conn_gen) next_conn_gen = 1;
    c->rx = kmalloc(RXBUF);
    c->tx = kmalloc(TXBUF);
    if (!c->rx || !c->tx) {
        conn_free(c);
        return -E_NOMEM;
    }
    c->owner = current;
    c->handle = h;
    c->rip = ip;
    c->rport = port;
    do c->lport = (uint16_t)(49152 + random_u64() % 16384);
    while (conn_find(ip, port, c->lport));
    c->iss = (uint32_t)random_u64();
    c->snd_una = c->snd_nxt = c->iss;
    c->rto = RTO_INIT;
    c->state = T_SYN_SENT;
    tcp_send_seg(c, c->iss, TCP_SYN, 0, 0);
    c->snd_nxt = c->iss + 1;
    timer_arm(c);
    /* The first SYN usually just triggered the ARP request; resend soon. */
    c->rto_at = ticks + 200;

    uint32_t gen = c->gen;
    while (c->state == T_SYN_SENT && !c->error && ticks < deadline) {
        block_net((int64_t)(deadline - ticks));
        net_poll();
        if (current->state == PROC_ZOMBIE) break;
        /* Another thread may have closed the handle while we waited, and the
         * slot may since belong to a new connection with the same handle. The
         * generation catches that, not just the pointer. */
        uint32_t now = 0;
        struct tcb *same = conn_of_gen((uint64_t)h, &now);
        if (same != c || now != gen) return -E_NET;
    }
    if (c->state != T_ESTABLISHED || c->error) {
        int err = c->error ? c->error : -E_TIMEOUT;
        conn_free(c);
        return err;
    }
    /* The TCP handshake can also block. Re-check before handing the app a
     * connection it may immediately use. */
    if (dlp_egress(current, DLP_EGRESS_NET)) {
        tcp_send_seg(c, c->snd_nxt, TCP_RST | TCP_ACK, 0, 0);
        conn_free(c);
        return -E_DENIED;
    }
    kprintf("[net] %s connected to %s:%u\n", current->name, host, port);
    return h;
}

int64_t sys_net_send(uint64_t h, uint64_t ubuf, uint64_t len) {
    uint32_t gen = 0;
    struct tcb *c = conn_of_gen(h, &gen);
    if (!c) return -E_INVAL;
    if (dlp_egress(current, DLP_EGRESS_NET)) return -E_DENIED; /* read something Confidential since connecting */
    if (!len) return 0;
    while (!c->error && (c->state == T_ESTABLISHED || c->state == T_CLOSE_WAIT) && !c->fin_queued &&
           c->tx_len == TXBUF) {
        block_net(1000);
        net_poll();
        if (current->state == PROC_ZOMBIE) return -E_NET;
        uint32_t now = 0;
        if (!(c = conn_of_gen(h, &now)) || now != gen) return -E_NET; /* handle reused while we waited */
    }
    if (c->error) return c->error;
    if ((c->state != T_ESTABLISHED && c->state != T_CLOSE_WAIT) || c->fin_queued) return -E_NET;
    /* The app may have become Confidential while this thread was blocked on
     * a full transmit window. Do not send the bytes it is about to hand us. */
    if (dlp_egress(current, DLP_EGRESS_NET)) return -E_DENIED;
    uint32_t n = (uint32_t)MIN(len, (uint64_t)(TXBUF - c->tx_len));
    if (copy_in(c->tx + c->tx_len, ubuf, n)) return -E_FAULT;
    c->tx_len += n;
    tcp_output(c, 0);
    return n;
}

int64_t sys_net_recv(uint64_t h, uint64_t ubuf, uint64_t len, int64_t timeout_ms) {
    uint32_t gen = 0;
    struct tcb *c = conn_of_gen(h, &gen);
    if (!c) return -E_INVAL;
    uint64_t deadline = timeout_ms < 0 ? UINT64_MAX : ticks + (uint64_t)timeout_ms;
    for (;;) {
        net_poll();
        if (c->rx_len) break;
        if (c->peer_fin) return 0;
        if (c->error) return c->error;
        if (ticks >= deadline) return -E_TIMEOUT;
        block_net(deadline == UINT64_MAX ? 1000 : (int64_t)(deadline - ticks));
        if (current->state == PROC_ZOMBIE) return -E_NET;
        uint32_t now = 0;
        if (!(c = conn_of_gen(h, &now)) || now != gen) return -E_NET; /* handle reused while we waited */
    }
    uint32_t n = (uint32_t)MIN(len, (uint64_t)c->rx_len);
    uint32_t first = MIN(n, RXBUF - c->rx_head);
    if (copy_out(ubuf, c->rx + c->rx_head, first)) return -E_FAULT;
    if (n > first && copy_out(ubuf + first, c->rx, n - first)) return -E_FAULT;
    c->rx_head = (c->rx_head + n) % RXBUF;
    c->rx_len -= n;
    /* Tell the peer when the window opens up again after being nearly shut. */
    if (c->last_wnd < 2u * c->mss && rx_space(c) >= 2u * c->mss && !c->error) tcp_ack(c);
    return n;
}

int64_t sys_net_close(uint64_t h) {
    struct tcb *c = conn_of(h);
    if (!c) return -E_INVAL;
    /* A half-open connection has no peer state worth lingering for. Free it
     * now; an ownerless SYN retransmitting for 20 s is a quota bypass. */
    if (c->state == T_SYN_SENT) {
        tcp_send_seg(c, c->snd_nxt, TCP_RST | TCP_ACK, 0, 0);
        conn_free(c);
        return 0;
    }
    c->owner = 0;
    c->handle = 0;
    if (c->error || c->state == T_TIME_WAIT || (c->state == T_CLOSING && c->peer_fin && !c->tx_len && c->fin_sent &&
                                               c->snd_una == c->snd_nxt)) {
        conn_free(c);
        return 0;
    }
    if (c->rx_len) { /* unread data: a reset, so the peer knows it wasn't received */
        tcp_send_seg(c, c->snd_nxt, TCP_RST | TCP_ACK, 0, 0);
        conn_free(c);
        return 0;
    }
    c->fin_queued = 1;
    c->expire_at = ticks + LINGER_MS;
    tcp_output(c, 0);
    return 0;
}

void net_proc_exit(struct proc *p) {
    for (int i = 0; i < MAX_CONNS; i++) {
        struct tcb *c = &conns[i];
        if (c->state == T_FREE || c->owner != p) continue;
        if (!c->error && c->state != T_SYN_SENT && c->state != T_TIME_WAIT)
            tcp_send_seg(c, c->snd_nxt, TCP_RST | TCP_ACK, 0, 0);
        conn_free(c);
    }
    for (int i = 0; i < DNS_QUERIES; i++)
        if (dnsq[i].waiter == p) {
            /* The resolving thread is dead and will never run its cleanup. */
            dnsq[i].busy = 0;
            dnsq[i].done = 0;
            dnsq[i].waiter = 0;
            dnsq[i].name[0] = 0;
        }
    for (int i = 0; i < ONCE_SLOTS; i++)
        if (once[i].p == p) once[i].p = 0;
}

int64_t sys_net_info(uint64_t uout) {
    struct net_info ni;
    memset(&ni, 0, sizeof ni);
    if (nic) {
        ni.link = nic->link_up() ? 1 : 0;
        memcpy(ni.mac, nic->mac, 6);
    }
    ni.configured = (uint32_t)cfg.configured;
    if (cfg.configured) {
        ni.ip = cfg.ip;
        ni.netmask = cfg.mask;
        ni.gateway = cfg.gw;
        ni.dns = cfg.dns;
    }
    ni.rx_packets = rx_packets;
    ni.tx_packets = tx_packets;
    ni.dropped = dropped;
    for (int i = 0; i < MAX_CONNS; i++)
        if (conns[i].state != T_FREE) ni.connections++;
    /* Global traffic counters can carry data between processes (a tainted
     * app moves them; an untainted app reads them). Keep exact totals for
     * system/trusted callers only. */
    if (current->trust == TRUST_UNTRUSTED) {
        /* An untrusted app needs to know whether there is a link, but not the
         * user's addresses, DNS server or a stable hardware identifier. */
        memset(ni.mac, 0, sizeof ni.mac);
        ni.ip = ni.netmask = ni.gateway = ni.dns = 0;
        ni.rx_packets = ni.tx_packets = ni.dropped = 0;
        ni.connections = 0;
    }
    return copy_out(uout, &ni, sizeof ni) ? -E_FAULT : 0;
}

int64_t sys_firewall(uint64_t op, uint64_t urule, uint64_t index) {
    if (!(current->caps & CAP_FIREWALL)) return -E_PERM;
    struct fw_rule r;
    switch (op) {
    case FW_COUNT: return fw_count();
    case FW_GET:
        if (fw_get((int)index, &r)) return -E_NOENT;
        return copy_out(urule, &r, sizeof r) ? -E_FAULT : 0;
    case FW_ADD:
        if (copy_in(&r, urule, sizeof r)) return -E_FAULT;
        r.app[sizeof r.app - 1] = 0;
        r.host[sizeof r.host - 1] = 0;
        return fw_add(&r);
    case FW_REMOVE: return fw_remove((int)index);
    case FW_BUILTIN:
        if (fw_builtin_get((int)index, &r)) return -E_NOENT;
        return copy_out(urule, &r, sizeof r) ? -E_FAULT : 0;
    default: return -E_INVAL;
    }
}
