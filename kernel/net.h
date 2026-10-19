/* net.h - the network stack (net.c), its card driver (e1000.c) and the firewall (firewall.c). */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "../include/abi.h"

struct proc;

/* What the stack needs from a card. Polled: rx returns a frame's length,
 * 0 when there's nothing, or -1 for a frame it dropped. */
struct nic_ops {
    int (*tx)(const void *frame, size_t len);
    int (*rx)(void *buf, size_t max);
    int (*link_up)(void);
    uint8_t mac[6];
};

struct nic_ops *e1000_init(void);

void net_init(void);
void net_poll(void);               /* from the window manager loop and from waiting callers */
void net_proc_exit(struct proc *p); /* reset its connections */
int net_state(void);                /* 0 no card, 1 no address yet, 2 online */
void net_get_config(struct net_config *out);
int net_apply_config(const struct net_config *cfg); /* 0, or -E_INVAL / -E_NOTSUP */

int64_t sys_net_connect(uint64_t uhost, uint64_t len, uint64_t port, uint64_t timeout_ms);
int64_t sys_net_send(uint64_t h, uint64_t ubuf, uint64_t len);
int64_t sys_net_recv(uint64_t h, uint64_t ubuf, uint64_t len, int64_t timeout_ms);
int64_t sys_net_close(uint64_t h);
int64_t sys_net_info(uint64_t uout);
int64_t sys_firewall(uint64_t op, uint64_t urule, uint64_t index);

/* Firewall (firewall.c). fw_check decides one connection: 1 allow, 0 deny,
 * -1 no rule or built-in policy matches (ask the user). fw_remember adds the user's answer. */
int fw_check(const char *app, const char *host, uint16_t port, int allow_builtin);
void fw_remember(const char *app, const char *host, uint16_t port, int allow);
int fw_count(void);
int fw_get(int i, struct fw_rule *out);
int fw_add(const struct fw_rule *r);
int fw_remove(int i);
void fw_load(void); /* once the encrypted volume is unlocked */
int fw_host_match(const char *pattern, const char *host);
/* The built-in web policy (the browser on ports 443 and 80): the index of
 * the entry that allows app:port, or -1; fw_builtin_get describes entry i. */
int fw_builtin(const char *app, uint16_t port);
int fw_builtin_get(int i, struct fw_rule *out);

/* Helpers shared with the unit tests. */
int dns_build_query(uint8_t *out, size_t max, const char *name, uint16_t id);
int dns_parse_reply(const uint8_t *msg, size_t len, uint16_t id, const char *name, uint32_t *ip, uint32_t *ttl);
int net_host_ok(const char *host);        /* a well-formed DNS name or dotted IPv4 literal */
int net_parse_ipv4(const char *s, uint32_t *ip); /* network byte order */
int net_addr_allowed(uint32_t ip, int literal);  /* 0 for loopback, link-local, multicast...; LAN/special-use only as a literal */
uint16_t net_checksum(const void *data, size_t len, uint32_t sum);
