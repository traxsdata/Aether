/*
 * afs.c - the æther file system (see afs.h for the format's security story).
 *
 * Layout, in 4 KiB blocks:
 *   0                 superblock (plain, but its fields are authenticated
 *                     by the key wrap, so they can't be altered unnoticed)
 *   1 ..              free-space bitmap (plain: 1 bit per block)
 *   inode_start ..    inode table, sealed, 15 inodes per block
 *   data_start ..     sealed file data, folder records and pointer blocks
 *
 * The block cache holds decrypted payloads; writes go straight through to
 * the disk (simple, and a crash loses at most the call in progress).
 */
#include "afs.h"
#include "blk.h"
#include "crypto.h"
#include "kernel.h"
#include "mm.h"
#include "rng.h"
#include "rtc.h"

#define AFS_VERSION 1
#define KDF_ITERS 200000
#define KDF_ITERS_MAX 1000000u /* refuse hostile superblocks that make boot spin for hours */
#define SECTORS_PER_BLOCK (AFS_BLOCK / 512)
#define INODES_PER_BLOCK 15
#define PTRS (AFS_PAYLOAD / 4) /* block pointers per pointer block */
#define MAX_INODES 65535
#define NCACHE 256

static const char magic[8] = {'A', 'E', 'T', 'H', 'E', 'R', 'F', 'S'};

struct afs_super {
    char magic[8];
    uint32_t version, block_size;
    uint32_t nblocks, bitmap_start, bitmap_blocks, inode_start, inode_blocks, ninodes, data_start;
    uint32_t kdf_iters;
    uint8_t uuid[16];
    uint8_t salt[32];
    uint64_t created;
    /* Everything above is the additional data of the key wrap below. */
    uint8_t wrap_nonce[12];
    uint8_t wrap_tag[16];
    uint8_t wrapped_key[32];
};
#define SUPER_AAD_LEN __builtin_offsetof(struct afs_super, wrap_nonce)

/* What a sealed block's tag vouches for besides its contents. */
enum { K_DATA = 1, K_PTR = 2, K_DPTR = 3, K_INODES = 4 };
struct aad {
    uint32_t kind, ino, gen, index;
};

struct drec {
    uint32_t ino;
    uint8_t type, len;
    char name[NAME_MAX];
};
_Static_assert(sizeof(struct drec) == 64, "folder record size");

struct afs_state {
    struct blkdev *dev;
    struct afs_super sb;
    uint8_t vk[32], inode_key[32];
    uint8_t *bitmap;   /* bitmap_blocks * AFS_BLOCK bytes */
    uint8_t *inode_used; /* 1 byte per inode */
    uint64_t bitmap_phys, inode_phys;
    uint32_t free_blocks, free_inodes, hint;
    int mounted, temporary;
};
static struct afs_state fs;

static uint8_t raw[AFS_BLOCK] __attribute__((aligned(16)));
static uint32_t kdf_iters = KDF_ITERS; /* lowered only while the self-tests run */
static int testing; /* the self-tests have parked the live volume (see the end of this file) */

/* ---- Block cache ------------------------------------------------------- */

static struct {
    uint32_t blk, stamp;
    struct aad aad; /* which file/position this plaintext was authenticated for */
    uint8_t *data;
} cache[NCACHE];
static uint32_t cache_clock;

static int aad_eq(const struct aad *x, const struct aad *y) {
    return x->kind == y->kind && x->ino == y->ino && x->gen == y->gen && x->index == y->index;
}

static void cache_init(void) {
    for (int i = 0; i < NCACHE; i++) {
        if (!cache[i].data) cache[i].data = kmalloc(AFS_PAYLOAD);
        if (!cache[i].data) panic("afs: no memory for the block cache");
        crypto_wipe(cache[i].data, AFS_PAYLOAD); /* never keep plaintext from another volume */
        cache[i].blk = 0;
        cache[i].aad = (struct aad){0, 0, 0, 0};
    }
}

static uint8_t *cache_find(uint32_t blk, const struct aad *aad) {
    for (int i = 0; i < NCACHE; i++)
        if (cache[i].blk == blk && aad_eq(&cache[i].aad, aad)) {
            cache[i].stamp = ++cache_clock;
            return cache[i].data;
        }
    return 0;
}

static uint8_t *cache_slot(uint32_t blk, const struct aad *aad) {
    uint8_t *hit = cache_find(blk, aad);
    if (hit) return hit;
    int victim = 0;
    for (int i = 1; i < NCACHE; i++)
        if (cache[i].stamp < cache[victim].stamp) victim = i;
    cache[victim].blk = blk;
    cache[victim].aad = *aad;
    cache[victim].stamp = ++cache_clock;
    return cache[victim].data;
}

static void cache_drop(uint32_t blk) {
    for (int i = 0; i < NCACHE; i++)
        if (cache[i].blk == blk) {
            crypto_wipe(cache[i].data, AFS_PAYLOAD);
            cache[i].blk = 0;
            cache[i].aad = (struct aad){0, 0, 0, 0};
            cache[i].stamp = 0;
        }
}

/* ---- Raw and sealed block I/O ------------------------------------------ */

static int raw_read(uint32_t blk, void *buf) {
    return blk_read(fs.dev, (uint64_t)blk * SECTORS_PER_BLOCK, SECTORS_PER_BLOCK, buf) ? -E_IO : 0;
}

static int raw_write(uint32_t blk, const void *buf) {
    return blk_write(fs.dev, (uint64_t)blk * SECTORS_PER_BLOCK, SECTORS_PER_BLOCK, buf) ? -E_IO : 0;
}

static int read_sealed(uint32_t blk, const uint8_t key[32], struct aad a, uint8_t *out) {
    uint8_t *c = cache_find(blk, &a);
    if (c) {
        memcpy(out, c, AFS_PAYLOAD);
        return 0;
    }
    if (raw_read(blk, raw)) return -E_IO;
    if (aead_open(key, raw, &a, sizeof a, raw + 28, AFS_PAYLOAD, out, raw + 12)) {
        if (!testing) kprintf("[afs] block %u failed its integrity check (tampered with, or a wrong key)\n", blk);
        return -E_IO;
    }
    memcpy(cache_slot(blk, &a), out, AFS_PAYLOAD);
    return 0;
}

static int write_sealed(uint32_t blk, const uint8_t key[32], struct aad a, const uint8_t *in) {
    /* A block is never intentionally shared between files or positions, but
     * a hostile disk can create such a reference. Drop any old plaintext for
     * this physical block before installing the new authenticated version. */
    cache_drop(blk);
    random_bytes(raw, 12);
    aead_seal(key, raw, &a, sizeof a, in, AFS_PAYLOAD, raw + 28, raw + 12);
    if (raw_write(blk, raw)) return -E_IO;
    memcpy(cache_slot(blk, &a), in, AFS_PAYLOAD);
    return 0;
}

static int data_block_ok(uint32_t b) { return b >= fs.sb.data_start && b < fs.sb.nblocks; }

static void file_key(uint32_t ino, uint32_t gen, uint8_t out[32]) {
    struct {
        char tag[12];
        uint32_t ino, gen;
    } info = {"aether-file", ino, gen};
    hkdf_sha256(fs.sb.uuid, sizeof fs.sb.uuid, fs.vk, sizeof fs.vk, &info, sizeof info, out, 32);
}

/* ---- Free space -------------------------------------------------------- */

static int bit_get(uint32_t b) { return fs.bitmap[b / 8] >> (b % 8) & 1; }

static int bitmap_flush(uint32_t b) {
    uint32_t k = b / (AFS_BLOCK * 8);
    return raw_write(fs.sb.bitmap_start + k, fs.bitmap + (uint64_t)k * AFS_BLOCK);
}

static uint32_t balloc(void) {
    uint32_t n = fs.sb.nblocks, start = fs.sb.data_start;
    if (fs.hint < start || fs.hint >= n) fs.hint = start;
    for (uint32_t i = 0; i < n - start; i++) {
        uint32_t b = start + (fs.hint - start + i) % (n - start);
        if (!bit_get(b)) {
            fs.bitmap[b / 8] |= (uint8_t)(1 << (b % 8));
            if (bitmap_flush(b)) return 0;
            fs.free_blocks--;
            fs.hint = b + 1;
            return b;
        }
    }
    return 0;
}

static void bfree(uint32_t b) {
    if (b < fs.sb.data_start || b >= fs.sb.nblocks || !bit_get(b)) return;
    fs.bitmap[b / 8] &= (uint8_t)~(1 << (b % 8));
    bitmap_flush(b);
    fs.free_blocks++;
    cache_drop(b);
}

/* ---- Inodes ------------------------------------------------------------ */

static uint8_t ibuf[AFS_PAYLOAD];

int afs_iget(uint32_t ino, struct afs_inode *out) {
    if (!fs.mounted) return -E_LOCKED;
    if (ino == 0 || ino >= fs.sb.ninodes) return -E_INVAL;
    uint32_t k = ino / INODES_PER_BLOCK;
    int err = read_sealed(fs.sb.inode_start + k, fs.inode_key, (struct aad){K_INODES, 0, 0, k}, ibuf);
    if (err) return err;
    memcpy(out, ibuf + (ino % INODES_PER_BLOCK) * sizeof *out, sizeof *out);
    /* A corrupted (or maliciously crafted) inode must not reach the rest of
     * the file system. Labels index tables and mode selects behaviour. */
    if (out->mode != AFS_FREE && out->mode != AFS_FILE && out->mode != AFS_DIR) return -E_IO;
    if (out->label >= LABEL_COUNT) return -E_IO;
    if (out->mode != AFS_FREE) {
        for (int i = 0; i < AFS_NDIRECT; i++)
            if (out->direct[i] && !data_block_ok(out->direct[i])) return -E_IO;
        if (out->indirect && !data_block_ok(out->indirect)) return -E_IO;
        if (out->dindirect && !data_block_ok(out->dindirect)) return -E_IO;
    }
    return 0;
}

int afs_iput(uint32_t ino, const struct afs_inode *in) {
    if (!fs.mounted) return -E_LOCKED;
    if (ino == 0 || ino >= fs.sb.ninodes) return -E_INVAL;
    uint32_t k = ino / INODES_PER_BLOCK;
    struct aad a = {K_INODES, 0, 0, k};
    int err = read_sealed(fs.sb.inode_start + k, fs.inode_key, a, ibuf);
    if (err) return err;
    memcpy(ibuf + (ino % INODES_PER_BLOCK) * sizeof *in, in, sizeof *in);
    return write_sealed(fs.sb.inode_start + k, fs.inode_key, a, ibuf);
}

int afs_ialloc(uint16_t mode, uint8_t label, uint32_t parent) {
    if (!fs.mounted) return -E_LOCKED;
    for (uint32_t ino = AFS_ROOT; ino < fs.sb.ninodes; ino++) {
        if (fs.inode_used[ino]) continue;
        struct afs_inode n;
        int err = afs_iget(ino, &n);
        if (err) return err;
        uint32_t gen = n.gen + 1;
        memset(&n, 0, sizeof n);
        n.mode = mode;
        n.label = label;
        n.nlink = 1;
        n.gen = gen;
        n.parent = parent;
        n.ctime = n.mtime = rtc_unix();
        if ((err = afs_iput(ino, &n))) return err;
        fs.inode_used[ino] = 1;
        fs.free_inodes--;
        return (int)ino;
    }
    return -E_NOSPACE;
}

/* ---- Block mapping ----------------------------------------------------- */

static uint8_t pbuf[AFS_PAYLOAD], pbuf2[AFS_PAYLOAD];

/* A fresh, zeroed, sealed pointer block. */
static uint32_t new_ptr_block(const uint8_t key[32], struct aad a) {
    uint32_t b = balloc();
    if (!b) return 0;
    static uint8_t zero[AFS_PAYLOAD];
    if (write_sealed(b, key, a, zero)) {
        bfree(b);
        return 0;
    }
    return b;
}

/*
 * Physical block of logical block `lb` of a file, allocating it (and any
 * pointer blocks on the way) if `alloc`. *fresh is set when the data block
 * was just allocated, i.e. its contents are zero. 0 = hole (or no space).
 */
static uint32_t bmap(uint32_t ino, struct afs_inode *n, const uint8_t key[32], uint64_t lb, int alloc, int *fresh) {
    *fresh = 0;
    if (lb < AFS_NDIRECT) {
        if (!n->direct[lb] && alloc && (n->direct[lb] = balloc())) *fresh = 1;
        return n->direct[lb];
    }
    lb -= AFS_NDIRECT;
    uint32_t *slot_blk, level_index;
    struct aad pa;
    if (lb < PTRS) {
        if (!n->indirect) {
            if (!alloc) return 0;
            if (!(n->indirect = new_ptr_block(key, (struct aad){K_PTR, ino, n->gen, 0}))) return 0;
        }
        slot_blk = &n->indirect;
        level_index = 0;
    } else {
        lb -= PTRS;
        if (lb >= (uint64_t)PTRS * PTRS) return 0;
        struct aad da = {K_DPTR, ino, n->gen, 0};
        if (!n->dindirect) {
            if (!alloc) return 0;
            if (!(n->dindirect = new_ptr_block(key, da))) return 0;
        }
        if (read_sealed(n->dindirect, key, da, pbuf2)) return 0;
        uint32_t *top = (uint32_t *)pbuf2, i = (uint32_t)(lb / PTRS);
        if (!top[i]) {
            if (!alloc) return 0;
            if (!(top[i] = new_ptr_block(key, (struct aad){K_PTR, ino, n->gen, 1 + i}))) return 0;
            if (write_sealed(n->dindirect, key, da, pbuf2)) return 0;
        }
        static uint32_t second;
        second = top[i];
        slot_blk = &second;
        level_index = 1 + i;
        lb %= PTRS;
    }
    pa = (struct aad){K_PTR, ino, n->gen, level_index};
    if (read_sealed(*slot_blk, key, pa, pbuf)) return 0;
    uint32_t *ptr = (uint32_t *)pbuf;
    if (!ptr[lb] && alloc) {
        if (!(ptr[lb] = balloc())) return 0;
        *fresh = 1;
        if (write_sealed(*slot_blk, key, pa, pbuf)) return 0;
    }
    return ptr[lb];
}

/* Frees data blocks from logical block `from` on (and all pointer blocks if from == 0).
 * Returns 0, or -E_IO if a pointer block cannot be read or updated; callers
 * must not then claim the file was truncated. */
static int free_from(uint32_t ino, struct afs_inode *n, const uint8_t key[32], uint64_t from) {
    for (uint64_t lb = from; lb < AFS_NDIRECT; lb++) {
        bfree(n->direct[lb]);
        n->direct[lb] = 0;
    }
    uint64_t first = from > AFS_NDIRECT ? from - AFS_NDIRECT : 0;
    if (n->indirect) {
        struct aad pa = {K_PTR, ino, n->gen, 0};
        if (first < PTRS) {
            if (read_sealed(n->indirect, key, pa, pbuf)) return -E_IO;
            uint32_t *ptr = (uint32_t *)pbuf;
            for (uint64_t i = first; i < PTRS; i++) bfree(ptr[i]), ptr[i] = 0;
            if (first == 0) {
                bfree(n->indirect);
                n->indirect = 0;
            } else if (write_sealed(n->indirect, key, pa, pbuf)) {
                return -E_IO;
            }
        }
    }
    first = from > AFS_NDIRECT + PTRS ? from - AFS_NDIRECT - PTRS : 0;
    if (n->dindirect) {
        struct aad da = {K_DPTR, ino, n->gen, 0};
        if (read_sealed(n->dindirect, key, da, pbuf2)) return -E_IO;
        uint32_t *top = (uint32_t *)pbuf2;
        for (uint32_t i = 0; i < PTRS; i++) {
            if (!top[i] || (uint64_t)(i + 1) * PTRS <= first) continue;
            struct aad pa = {K_PTR, ino, n->gen, 1 + i};
            if (read_sealed(top[i], key, pa, pbuf)) continue;
            uint32_t *ptr = (uint32_t *)pbuf;
            uint64_t base = (uint64_t)i * PTRS;
            for (uint32_t j = 0; j < PTRS; j++)
                if (base + j >= first) bfree(ptr[j]), ptr[j] = 0;
            if (base >= first) {
                bfree(top[i]);
                top[i] = 0;
            } else if (write_sealed(top[i], key, pa, pbuf)) {
                return -E_IO;
            }
        }
        if (first == 0) {
            bfree(n->dindirect);
            n->dindirect = 0;
        } else if (write_sealed(n->dindirect, key, da, pbuf2)) {
            return -E_IO;
        }
    }
    return 0;
}

int afs_ifree(uint32_t ino) {
    struct afs_inode n;
    int err = afs_iget(ino, &n);
    if (err) return err;
    uint8_t key[32];
    file_key(ino, n.gen, key);
    err = free_from(ino, &n, key, 0);
    crypto_wipe(key, sizeof key);
    if (err) return err;
    uint32_t gen = n.gen;
    memset(&n, 0, sizeof n);
    n.gen = gen;
    if ((err = afs_iput(ino, &n))) return err;
    if (fs.inode_used[ino]) {
        fs.inode_used[ino] = 0;
        fs.free_inodes++;
    }
    return 0;
}

/* ---- File contents ----------------------------------------------------- */

static uint8_t dbuf[AFS_PAYLOAD];

int64_t afs_read(uint32_t ino, uint64_t off, void *buf, uint64_t len) {
    struct afs_inode n;
    int err = afs_iget(ino, &n);
    if (err) return err;
    if (off >= n.size) return 0;
    len = MIN(len, n.size - off);
    uint8_t key[32];
    file_key(ino, n.gen, key);
    uint64_t done = 0;
    while (done < len) {
        uint64_t lb = (off + done) / AFS_PAYLOAD, in = (off + done) % AFS_PAYLOAD;
        uint64_t chunk = MIN(len - done, AFS_PAYLOAD - in);
        int fresh;
        uint32_t b = bmap(ino, &n, key, lb, 0, &fresh);
        if (b && !data_block_ok(b)) {
            crypto_wipe(key, sizeof key);
            return -E_IO;
        }
        if (!b) {
            memset((uint8_t *)buf + done, 0, chunk);
        } else if ((err = read_sealed(b, key, (struct aad){K_DATA, ino, n.gen, (uint32_t)lb}, dbuf))) {
            crypto_wipe(key, sizeof key);
            return done ? (int64_t)done : err;
        } else {
            memcpy((uint8_t *)buf + done, dbuf + in, chunk);
        }
        done += chunk;
    }
    crypto_wipe(key, sizeof key);
    crypto_wipe(dbuf, sizeof dbuf);
    return (int64_t)done;
}

int64_t afs_write(uint32_t ino, uint64_t off, const void *buf, uint64_t len) {
    struct afs_inode n;
    int err = afs_iget(ino, &n);
    if (err) return err;
    uint64_t max = (uint64_t)(AFS_NDIRECT + PTRS + (uint64_t)PTRS * PTRS) * AFS_PAYLOAD;
    if (off > max || len > max - off) return -E_NOSPACE;
    uint8_t key[32];
    file_key(ino, n.gen, key);
    uint64_t done = 0;
    while (done < len) {
        uint64_t lb = (off + done) / AFS_PAYLOAD, in = (off + done) % AFS_PAYLOAD;
        uint64_t chunk = MIN(len - done, AFS_PAYLOAD - in);
        int fresh;
        uint32_t b = bmap(ino, &n, key, lb, 1, &fresh);
        if (!b) {
            err = -E_NOSPACE;
            break;
        }
        if (!data_block_ok(b)) {
            err = -E_IO;
            break;
        }
        struct aad a = {K_DATA, ino, n.gen, (uint32_t)lb};
        if (fresh || (in == 0 && chunk == AFS_PAYLOAD)) {
            memset(dbuf, 0, sizeof dbuf);
        } else if ((err = read_sealed(b, key, a, dbuf))) {
            break;
        }
        memcpy(dbuf + in, (const uint8_t *)buf + done, chunk);
        if ((err = write_sealed(b, key, a, dbuf))) break;
        done += chunk;
        if (off + done > n.size) n.size = off + done;
    }
    crypto_wipe(key, sizeof key);
    crypto_wipe(dbuf, sizeof dbuf);
    n.mtime = rtc_unix();
    int e2 = afs_iput(ino, &n);
    if (e2) return e2; /* the size/metadata did not reach the disk */
    if (done) return (int64_t)done;
    return err;
}

int afs_truncate(uint32_t ino, uint64_t size) {
    struct afs_inode n;
    int err = afs_iget(ino, &n);
    if (err) return err;
    uint64_t max = (uint64_t)(AFS_NDIRECT + PTRS + (uint64_t)PTRS * PTRS) * AFS_PAYLOAD;
    if (size > max) return -E_NOSPACE;
    if (size >= n.size) { /* growing: the new range reads as zeros (a hole) */
        n.size = size;
        n.mtime = rtc_unix();
        return afs_iput(ino, &n);
    }
    uint8_t key[32];
    file_key(ino, n.gen, key);
    /* Round up without the (size + PAYLOAD - 1) overflow a hostile size could
     * use to wrap `keep` to zero and free blocks that must remain. */
    uint64_t keep = size / AFS_PAYLOAD + (size % AFS_PAYLOAD != 0);
    if ((err = free_from(ino, &n, key, keep))) {
        crypto_wipe(key, sizeof key);
        return err;
    }
    if (size % AFS_PAYLOAD) { /* zero the tail of the last block so it can't reappear */
        int fresh;
        uint64_t lb = size / AFS_PAYLOAD;
        uint32_t b = bmap(ino, &n, key, lb, 0, &fresh);
        struct aad a = {K_DATA, ino, n.gen, (uint32_t)lb};
        if (b && !data_block_ok(b)) {
            crypto_wipe(key, sizeof key);
            return -E_IO;
        }
        if (b) {
            if (read_sealed(b, key, a, dbuf)) {
                crypto_wipe(key, sizeof key);
                return -E_IO;
            }
            memset(dbuf + size % AFS_PAYLOAD, 0, AFS_PAYLOAD - size % AFS_PAYLOAD);
            if (write_sealed(b, key, a, dbuf)) {
                crypto_wipe(key, sizeof key);
                return -E_IO;
            }
        }
    }
    crypto_wipe(key, sizeof key);
    n.size = size;
    n.mtime = rtc_unix();
    return afs_iput(ino, &n);
}

/* ---- Folders ----------------------------------------------------------- */

static int name_ok(const char *name) {
    size_t len = strnlen(name, NAME_MAX + 1);
    if (len == 0 || len > NAME_MAX) return 0;
    if (!strcmp(name, ".") || !strcmp(name, "..")) return 0;
    for (size_t i = 0; i < len; i++)
        if (name[i] == '/' || (unsigned char)name[i] < 32) return 0;
    return 1;
}

static int rec_matches(const struct drec *r, const char *name) {
    size_t len = strlen(name);
    return r->ino && r->len == len && !memcmp(r->name, name, len);
}

/* Index of the record named `name`, or -1. */
static int64_t find_rec(uint32_t dir, const char *name, struct drec *out) {
    struct afs_inode d;
    if (afs_iget(dir, &d) || d.mode != AFS_DIR) return -1;
    for (uint64_t i = 0; i < d.size / sizeof *out; i++) {
        if (afs_read(dir, i * sizeof *out, out, sizeof *out) != sizeof *out) return -1;
        if (rec_matches(out, name)) return (int64_t)i;
    }
    return -1;
}

int afs_lookup(uint32_t dir, const char *name, uint32_t *ino) {
    struct drec r;
    if (!name_ok(name)) return -E_INVAL;
    if (find_rec(dir, name, &r) < 0) return -E_NOENT;
    *ino = r.ino;
    return 0;
}

int afs_dir_add(uint32_t dir, const char *name, uint32_t ino, uint8_t type) {
    if (!name_ok(name)) return -E_INVAL;
    struct afs_inode d;
    int err = afs_iget(dir, &d);
    if (err) return err;
    if (d.mode != AFS_DIR) return -E_NOTDIR;
    struct drec r;
    uint64_t slot = d.size / sizeof r;
    for (uint64_t i = 0; i < d.size / sizeof r; i++) {
        if (afs_read(dir, i * sizeof r, &r, sizeof r) != sizeof r) return -E_IO;
        if (rec_matches(&r, name)) return -E_EXIST;
        if (!r.ino && slot == d.size / sizeof r) slot = i;
    }
    memset(&r, 0, sizeof r);
    r.ino = ino;
    r.type = type;
    r.len = (uint8_t)strlen(name);
    memcpy(r.name, name, r.len);
    int64_t n = afs_write(dir, slot * sizeof r, &r, sizeof r);
    return n == sizeof r ? 0 : (n < 0 ? (int)n : -E_IO);
}

int afs_dir_remove(uint32_t dir, const char *name) {
    struct drec r;
    int64_t i = find_rec(dir, name, &r);
    if (i < 0) return -E_NOENT;
    memset(&r, 0, sizeof r);
    return afs_write(dir, (uint64_t)i * sizeof r, &r, sizeof r) == sizeof r ? 0 : -E_IO;
}

int afs_dir_entry(uint32_t dir, uint32_t idx, char *name, uint32_t *ino, uint8_t *type) {
    struct afs_inode d;
    int err = afs_iget(dir, &d);
    if (err) return err;
    if (d.mode != AFS_DIR) return -E_NOTDIR;
    struct drec r;
    for (uint64_t i = 0; i < d.size / sizeof r; i++) {
        if (afs_read(dir, i * sizeof r, &r, sizeof r) != sizeof r) return -E_IO;
        if (!r.ino || idx--) continue;
        /* r.len is a byte off the disk; afs_dir_add only ever stores a
         * length name_ok() has capped at NAME_MAX, and the record was
         * authenticated on read, so a larger value cannot occur without
         * the volume key. Clamp anyway: callers' name buffers are
         * NAME_MAX+1, and an over-long len would otherwise over-read
         * r.name and overflow them. */
        if (r.len > NAME_MAX) r.len = NAME_MAX;
        memcpy(name, r.name, r.len);
        name[r.len] = 0;
        *ino = r.ino;
        *type = r.type;
        return 1;
    }
    return 0;
}

int afs_dir_empty(uint32_t dir) {
    char name[NAME_MAX + 1];
    uint32_t ino;
    uint8_t type;
    return afs_dir_entry(dir, 0, name, &ino, &type) == 0;
}

/* ---- Mounting ---------------------------------------------------------- */

int afs_probe(struct blkdev *d) {
    static uint8_t buf[AFS_BLOCK] __attribute__((aligned(16)));
    if (blk_read(d, 0, SECTORS_PER_BLOCK, buf)) return -E_IO;
    if (!memcmp(buf, magic, sizeof magic)) return AFS_PROBE_AFS;
    for (int i = 0; i < AFS_BLOCK; i++)
        if (buf[i]) return AFS_PROBE_OTHER;
    /* Also check where a GPT backup header or other data might sit at the start. */
    if (blk_read(d, SECTORS_PER_BLOCK, SECTORS_PER_BLOCK, buf)) return -E_IO;
    for (int i = 0; i < AFS_BLOCK; i++)
        if (buf[i]) return AFS_PROBE_OTHER;
    /* A wiped primary MBR/GPT is not a blank disk: the backup GPT at the end
     * still identifies it, and formatting would destroy the user's data. */
    static uint8_t last[512];
    if (d->sectors && !blk_read(d, d->sectors - 1, 1, last) && !memcmp(last, "EFI PART", 8))
        return AFS_PROBE_OTHER;
    return AFS_PROBE_BLANK;
}

static void derive_kek(const char *pw, size_t pwlen, uint8_t kek[32]) {
    pbkdf2_sha256(pw, pwlen, fs.sb.salt, sizeof fs.sb.salt, fs.sb.kdf_iters, kek, 32);
}

static void derive_inode_key(void) {
    hkdf_sha256(fs.sb.uuid, sizeof fs.sb.uuid, fs.vk, sizeof fs.vk, "aether-inodes", 13, fs.inode_key, 32);
}

static void free_tables(void) {
    if (fs.bitmap_phys) {
        pmm_free_contig(fs.bitmap_phys, ALIGN_UP((uint64_t)fs.sb.bitmap_blocks * AFS_BLOCK, PAGE_SIZE) / PAGE_SIZE);
        fs.bitmap_phys = 0;
        fs.bitmap = 0;
    }
    if (fs.inode_phys) {
        pmm_free_contig(fs.inode_phys, ALIGN_UP(fs.sb.ninodes, PAGE_SIZE) / PAGE_SIZE);
        fs.inode_phys = 0;
        fs.inode_used = 0;
    }
}

static int alloc_tables(void) {
    uint64_t bpages = ALIGN_UP((uint64_t)fs.sb.bitmap_blocks * AFS_BLOCK, PAGE_SIZE) / PAGE_SIZE;
    uint64_t ipages = ALIGN_UP(fs.sb.ninodes, PAGE_SIZE) / PAGE_SIZE;
    uint64_t b = pmm_alloc_contig(bpages), i = pmm_alloc_contig(ipages);
    if (!b || !i) {
        if (b) pmm_free_contig(b, bpages);
        if (i) pmm_free_contig(i, ipages);
        return -E_NOMEM;
    }
    fs.bitmap = P2V(b);
    fs.inode_used = P2V(i);
    fs.bitmap_phys = b;
    fs.inode_phys = i;
    cache_init();
    return 0;
}

int afs_format(struct blkdev *d, const char *pw, size_t pwlen) {
    memset(&fs, 0, sizeof fs);
    fs.dev = d;
    struct afs_super *sb = &fs.sb;
    uint64_t nb = d->sectors / SECTORS_PER_BLOCK;
    if (nb > 0xFFFFFFF0ull) nb = 0xFFFFFFF0ull;
    if (nb < 64) return -E_NOSPACE;
    memcpy(sb->magic, magic, sizeof magic);
    sb->version = AFS_VERSION;
    sb->block_size = AFS_BLOCK;
    sb->nblocks = (uint32_t)nb;
    uint32_t ninodes = (uint32_t)MIN(MAX(nb / 8, 64ull), (uint64_t)MAX_INODES);
    sb->ninodes = (ninodes + INODES_PER_BLOCK - 1) / INODES_PER_BLOCK * INODES_PER_BLOCK;
    sb->inode_blocks = sb->ninodes / INODES_PER_BLOCK;
    sb->bitmap_blocks = (uint32_t)((nb + AFS_BLOCK * 8 - 1) / (AFS_BLOCK * 8));
    sb->bitmap_start = 1;
    sb->inode_start = 1 + sb->bitmap_blocks;
    sb->data_start = sb->inode_start + sb->inode_blocks;
    sb->kdf_iters = kdf_iters;
    sb->created = rtc_unix();
    random_bytes(sb->uuid, sizeof sb->uuid);
    random_bytes(sb->salt, sizeof sb->salt);
    random_bytes(fs.vk, sizeof fs.vk);
    random_bytes(sb->wrap_nonce, sizeof sb->wrap_nonce);

    uint8_t kek[32];
    if (pw) derive_kek(pw, pwlen, kek);
    else random_bytes(kek, sizeof kek); /* temporary volume: nobody can ever unlock it again */
    aead_seal(kek, sb->wrap_nonce, sb, SUPER_AAD_LEN, fs.vk, 32, sb->wrapped_key, sb->wrap_tag);
    crypto_wipe(kek, sizeof kek);
    derive_inode_key();
    if (alloc_tables()) return -E_NOMEM;

    for (uint32_t b = 0; b < sb->data_start; b++) fs.bitmap[b / 8] |= (uint8_t)(1 << (b % 8));
    fs.free_blocks = sb->nblocks - sb->data_start;
    memset(raw, 0, sizeof raw);
    memcpy(raw, sb, sizeof *sb);
    if (raw_write(0, raw)) return -E_IO;
    for (uint32_t k = 0; k < sb->bitmap_blocks; k++)
        if (raw_write(sb->bitmap_start + k, fs.bitmap + (uint64_t)k * AFS_BLOCK)) return -E_IO;
    static uint8_t zero[AFS_PAYLOAD];
    for (uint32_t k = 0; k < sb->inode_blocks; k++)
        if (write_sealed(sb->inode_start + k, fs.inode_key, (struct aad){K_INODES, 0, 0, k}, zero)) return -E_IO;
    fs.inode_used[0] = 1;
    fs.free_inodes = sb->ninodes - 1;
    fs.mounted = 1;
    fs.temporary = !pw;
    int root = afs_ialloc(AFS_DIR, LABEL_PERSONAL, AFS_ROOT);
    if (root != AFS_ROOT) return root < 0 ? root : -E_IO;
    blk_flush(d);
    if (!testing)
        kprintf("[afs] formatted %s: %u blocks, %u inodes, %s\n", d->name, sb->nblocks, sb->ninodes,
                pw ? "encrypted with your passphrase" : "temporary (in memory)");
    return 0;
}

int afs_format_partition(struct blkdev *d, const char *pw, size_t pwlen) {
    struct afs_state live = fs;
    int err = afs_format(d, pw, pwlen);
    free_tables();
    fs = live;
    /* The target may have used the same block numbers and AADs as the live
     * volume. Drop every cache key so no decrypted block can be mistaken for
     * the other volume's data. */
    for (int i = 0; i < NCACHE; i++) {
        cache[i].blk = 0;
        cache[i].aad = (struct aad){0, 0, 0, 0};
        cache[i].stamp = 0;
    }
    return err;
}

int afs_unlock(struct blkdev *d, const char *pw, size_t pwlen) {
    memset(&fs, 0, sizeof fs);
    fs.dev = d;
    struct afs_super *sb = &fs.sb;
    if (blk_read(d, 0, SECTORS_PER_BLOCK, raw)) return -E_IO;
    memcpy(sb, raw, sizeof *sb);
    if (memcmp(sb->magic, magic, sizeof magic) || sb->version != AFS_VERSION || sb->block_size != AFS_BLOCK)
        return -E_INVAL;
    uint64_t nb = d->sectors / SECTORS_PER_BLOCK;
    /* Every structural sum is 64-bit: a hostile superblock can otherwise wrap
     * inode_start/data_start or make the inode scan walk past its table. */
    uint64_t ninodes = sb->ninodes, inode_blocks = sb->inode_blocks, bitmap_blocks = sb->bitmap_blocks;
    uint64_t inode_start = 1ull + bitmap_blocks;
    uint64_t data_start = inode_start + inode_blocks;
    if (sb->nblocks > nb || sb->nblocks < 64 || sb->kdf_iters < 1000 || sb->kdf_iters > KDF_ITERS_MAX ||
        sb->bitmap_start != 1 || bitmap_blocks == 0 || inode_blocks == 0 ||
        !ninodes || ninodes > MAX_INODES || ninodes % INODES_PER_BLOCK ||
        inode_blocks != ninodes / INODES_PER_BLOCK ||
        inode_start >= sb->nblocks || data_start > sb->nblocks || sb->data_start != data_start ||
        data_start >= sb->nblocks || (uint64_t)bitmap_blocks * AFS_BLOCK * 8 < sb->nblocks)
        return -E_INVAL;
    uint8_t kek[32];
    derive_kek(pw, pwlen, kek);
    int bad = aead_open(kek, sb->wrap_nonce, sb, SUPER_AAD_LEN, sb->wrapped_key, 32, fs.vk, sb->wrap_tag);
    crypto_wipe(kek, sizeof kek);
    if (bad) return -E_DENIED;
    derive_inode_key();
    if (alloc_tables()) return -E_NOMEM;
    for (uint32_t k = 0; k < sb->bitmap_blocks; k++)
        if (raw_read(sb->bitmap_start + k, fs.bitmap + (uint64_t)k * AFS_BLOCK)) {
            free_tables();
            return -E_IO;
        }
    for (uint32_t b = sb->data_start; b < sb->nblocks; b++) fs.free_blocks += !bit_get(b);
    fs.mounted = 1;
    fs.inode_used[0] = 1;
    for (uint32_t k = 0; k < sb->inode_blocks; k++) {
        if (read_sealed(sb->inode_start + k, fs.inode_key, (struct aad){K_INODES, 0, 0, k}, ibuf)) {
            fs.mounted = 0;
            free_tables();
            return -E_IO;
        }
        const struct afs_inode *in = (const struct afs_inode *)ibuf;
        for (uint32_t j = 0; j < INODES_PER_BLOCK; j++) {
            uint32_t ino = k * INODES_PER_BLOCK + j;
            if (ino == 0) continue;
            fs.inode_used[ino] = in[j].mode != AFS_FREE;
            fs.free_inodes += !fs.inode_used[ino];
        }
    }
    if (!testing) kprintf("[afs] unlocked %s: %u of %u blocks free\n", d->name, fs.free_blocks, sb->nblocks);
    return 0;
}

int afs_verify(const char *pw, size_t pwlen) {
    if (!fs.mounted || fs.temporary) return -E_LOCKED;
    uint8_t kek[32], vk[32];
    derive_kek(pw, pwlen, kek);
    int bad = aead_open(kek, fs.sb.wrap_nonce, &fs.sb, SUPER_AAD_LEN, fs.sb.wrapped_key, 32, vk, fs.sb.wrap_tag);
    crypto_wipe(kek, sizeof kek);
    crypto_wipe(vk, sizeof vk);
    return bad ? -E_DENIED : 0;
}

int afs_change_passphrase(const char *oldpw, size_t oldlen, const char *newpw, size_t newlen) {
    if (!fs.mounted || fs.temporary) return -E_LOCKED;
    if (!newpw || newlen < 8) return -E_INVAL;
    if (afs_verify(oldpw, oldlen)) return -E_DENIED;

    uint8_t kek[32];
    derive_kek(newpw, newlen, kek);
    struct afs_super sb = fs.sb;
    random_bytes(sb.wrap_nonce, sizeof sb.wrap_nonce);
    aead_seal(kek, sb.wrap_nonce, &sb, SUPER_AAD_LEN, fs.vk, sizeof fs.vk, sb.wrapped_key, sb.wrap_tag);
    crypto_wipe(kek, sizeof kek);
    memset(raw, 0, sizeof raw);
    memcpy(raw, &sb, sizeof sb);
    if (raw_write(0, raw)) return -E_IO;
    fs.sb = sb;
    return 0;
}

int afs_mounted(void) { return fs.mounted; }
int afs_temporary(void) { return fs.temporary; }

void afs_sync(void) {
    if (fs.mounted) blk_flush(fs.dev);
}

void afs_info(struct fs_info *out) {
    memset(out, 0, sizeof *out);
    if (!fs.mounted) return;
    out->kind = fs.temporary ? FS_TEMPORARY : FS_ENCRYPTED;
    out->block_size = AFS_PAYLOAD;
    out->total_blocks = fs.sb.nblocks - fs.sb.data_start;
    out->free_blocks = fs.free_blocks;
    out->total_inodes = fs.sb.ninodes - 1;
    out->free_inodes = fs.free_inodes;
    strlcpy(out->device, fs.dev->name, sizeof out->device);
}

/* ---- Self-test support (ktest.c) --------------------------------------- */

/* The unit tests format and unlock a scratch RAM disk through the real code
 * above, so the live volume's state is parked here meanwhile. The kernel
 * isn't preemptible, so nothing else can touch the file system in between.
 * The block cache is write-through, so emptying it loses nothing. */
static __typeof__(fs) parked;

static void cache_drop_all(void) {
    for (int i = 0; i < NCACHE; i++)
        if (cache[i].blk) cache_drop(cache[i].blk);
}

void afs_test_drop_cache(void) { cache_drop_all(); }

/* Frees what afs_format/afs_unlock allocated for the scratch volume. */
void afs_test_unmount(void) {
    if (!testing) return;
    if (fs.bitmap)
        pmm_free_contig((uint64_t)fs.bitmap - HHDM_BASE,
                        ALIGN_UP((uint64_t)fs.sb.bitmap_blocks * AFS_BLOCK, PAGE_SIZE) / PAGE_SIZE);
    if (fs.inode_used) pmm_free_contig((uint64_t)fs.inode_used - HHDM_BASE, ALIGN_UP(fs.sb.ninodes, PAGE_SIZE) / PAGE_SIZE);
    cache_drop_all();
    crypto_wipe(&fs, sizeof fs);
}

int afs_test_begin(void) {
    if (testing) return -E_LIMIT;
    cache_drop_all();
    parked = fs;
    memset(&fs, 0, sizeof fs);
    kdf_iters = 1000; /* the smallest afs_unlock accepts; keeps the tests quick */
    testing = 1;
    return 0;
}

void afs_test_end(void) {
    if (!testing) return;
    afs_test_unmount();
    fs = parked;
    crypto_wipe(&parked, sizeof parked);
    kdf_iters = KDF_ITERS;
    testing = 0;
}
