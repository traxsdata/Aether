/*
 * afs.h - the æther file system: an encrypted, authenticated on-disk format.
 *
 * Every block except the superblock and the free-space bitmap is sealed
 * with ChaCha20-Poly1305: 12-byte random nonce, 16-byte tag, 4068 bytes of
 * payload. The tag covers which file and which position the block belongs
 * to, so blocks can't be swapped, moved or altered without detection.
 * Keys: passphrase -> PBKDF2 -> key-encryption key, which unwraps the random
 * volume key; per-file keys are derived from that with HKDF.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "../include/abi.h"

struct blkdev;

#define AFS_BLOCK 4096
#define AFS_PAYLOAD 4068 /* AFS_BLOCK - nonce - tag */
#define AFS_ROOT 1       /* inode number of "/" */
#define AFS_NDIRECT 24

enum { AFS_FREE = 0, AFS_FILE = FT_FILE, AFS_DIR = FT_DIR };

struct afs_inode {
    uint16_t mode; /* AFS_FREE / AFS_FILE / AFS_DIR */
    uint8_t label; /* LABEL_* */
    uint8_t flags;
    uint32_t nlink;
    uint64_t size;
    uint64_t mtime, ctime;
    uint32_t gen; /* bumped each time the inode number is reused: new file key */
    uint32_t parent;
    uint32_t direct[AFS_NDIRECT];
    uint32_t indirect, dindirect;
    uint8_t pad[112];
};
_Static_assert(sizeof(struct afs_inode) == 256, "inode size");

enum { AFS_PROBE_BLANK = 0, AFS_PROBE_AFS = 1, AFS_PROBE_OTHER = 2 };

int afs_probe(struct blkdev *d);  /* AFS_PROBE_* or -E_IO */
/* Format and mount. pw == 0 makes a temporary volume whose key is never stored. */
int afs_format(struct blkdev *d, const char *pw, size_t pwlen);
/* Format a *different* disk while a volume is mounted. The live volume and
 * its cache are put back afterwards; used by the installer. */
int afs_format_partition(struct blkdev *d, const char *pw, size_t pwlen);
int afs_unlock(struct blkdev *d, const char *pw, size_t pwlen); /* -E_DENIED: wrong passphrase */
/* Check a passphrase against the mounted volume without changing any state:
 * used by the session lock, which must not reinitialize the file system. */
int afs_verify(const char *pw, size_t pwlen); /* 0, -E_DENIED or -E_LOCKED */
/* Re-wrap the mounted volume key under a new passphrase. The old one is
 * checked first; a failed write leaves the old wrap intact. */
int afs_change_passphrase(const char *oldpw, size_t oldlen, const char *newpw, size_t newlen);
int afs_mounted(void);
int afs_temporary(void);
void afs_info(struct fs_info *out);
void afs_sync(void);

int afs_iget(uint32_t ino, struct afs_inode *out);
int afs_iput(uint32_t ino, const struct afs_inode *in);
int afs_ialloc(uint16_t mode, uint8_t label, uint32_t parent); /* inode number or -E_* */
int afs_ifree(uint32_t ino);                                    /* frees its blocks too */

int64_t afs_read(uint32_t ino, uint64_t off, void *buf, uint64_t len);
int64_t afs_write(uint32_t ino, uint64_t off, const void *buf, uint64_t len);
int afs_truncate(uint32_t ino, uint64_t size);

/* Folders: records of {inode, type, name}. Names are 1..NAME_MAX bytes. */
int afs_lookup(uint32_t dir, const char *name, uint32_t *ino);
int afs_dir_add(uint32_t dir, const char *name, uint32_t ino, uint8_t type);
int afs_dir_remove(uint32_t dir, const char *name);
/* The idx-th entry: 1 and fills name/ino/type, 0 past the end. */
int afs_dir_entry(uint32_t dir, uint32_t idx, char *name, uint32_t *ino, uint8_t *type);
int afs_dir_empty(uint32_t dir);

/* Self-test support (ktest.c): park the live volume so a scratch disk can be
 * formatted and mounted, then put it back. */
int afs_test_begin(void);
void afs_test_unmount(void); /* free the scratch volume's tables */
void afs_test_drop_cache(void);
void afs_test_end(void);
