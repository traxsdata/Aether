/*
 * ktest.c - unit tests for the kernel's core subsystems, run inside the
 * running kernel by the terminal's `test` command:
 *
 *   lib      string and formatting routines shared with apps
 *   memory   physical pages, kernel heap, page tables and W^X
 *   crypto   known-answer vectors and AEAD tamper rejection
 *   files    the encrypted file system, on a scratch RAM disk
 *   dlp      labels, taint and egress checks
 *   sandbox  trust levels, system call filters, quotas, address layout
 *   syscall  pointer validation against the caller's page tables
 *   runner   program loading, app packages and made-on-touch memory (0.4)
 *
 * Tests run in the caller's system call with interrupts off (the kernel
 * isn't preemptible), so nothing else runs until they finish. Every test
 * cleans up after itself: pages, heap blocks and processes it creates are
 * freed, and the live file system is parked while `files` runs.
 */
#include "ktest.h"
#include "ktest_rsa.h"
#include "../common/rsa_verify.h"
#include "../include/abi.h"
#include "afs.h"
#include "apps.h"
#include "appstore.h"
#include "clipboard.h"
#include "blk.h"
#include "crypto.h"
#include "dlp.h"
#include "elf.h"
#include "kernel.h"
#include "net.h"
#include "mm.h"
#include "proc.h"
#include "rng.h"
#include "settings.h"
#include "storage.h"
#include "uaccess.h"
#include "usb_hid.h"
#include "wm.h"
#include "i2c_hid.h"

extern char __text_start[], __rodata_start[], __data_start[];

/* Defined in the sandbox section; used earlier by the runner tests. */
static struct proc *spawn_quiet(const char *name, int parent);
static void stop(struct proc *p);

/* ---- Runner ------------------------------------------------------------ */

static int failed_line;
static const char *failed_expr;

void ktest_check(int ok, const char *expr, int line) {
    if (!ok && !failed_expr) {
        failed_expr = expr;
        failed_line = line;
    }
}

/* Stop a test at its first failure when later steps depend on it. */
#define KT_REQUIRE(e)          \
    do {                       \
        KT_CHECK(e);           \
        if (failed_expr) return; \
    } while (0)

static int contains(const char *hay, size_t n, const char *needle) {
    size_t ln = strlen(needle);
    for (size_t i = 0; i + ln <= n; i++)
        if (!memcmp(hay + i, needle, ln)) return 1;
    return 0;
}

/* ---- lib --------------------------------------------------------------- */

static void t_lib_mem(void) {
    char a[16] = "abcdefgh", b[16];
    memmove(a + 2, a, 6); /* overlapping, forwards */
    KT_CHECK(!memcmp(a, "ababcdef", 8));
    memcpy(a, "abcdefgh", 9);
    memmove(a, a + 2, 6); /* overlapping, backwards */
    KT_CHECK(!memcmp(a, "cdefghgh", 8));
    memset(b, 0x5A, sizeof b);
    KT_CHECK(b[0] == 0x5A && b[15] == 0x5A);
    KT_CHECK(memcmp("abc", "abd", 3) < 0 && memcmp("abd", "abc", 3) > 0 && memcmp("abc", "abc", 3) == 0);
}

static void t_lib_str(void) {
    char buf[8];
    KT_CHECK(strlen("") == 0 && strlen("æther") == 6); /* bytes, not characters */
    KT_CHECK(strnlen("abcdef", 3) == 3);
    KT_CHECK(strcmp("a", "b") < 0 && strcmp("b", "a") > 0 && strcmp("same", "same") == 0);
    KT_CHECK(strncmp("abcX", "abcY", 3) == 0 && strncmp("abcX", "abcY", 4) != 0);
    KT_CHECK(strlcpy(buf, "overflowing", sizeof buf) == 11); /* returns the source length */
    KT_CHECK(!strcmp(buf, "overflo"));                         /* and always terminates */
}

static void t_lib_fmt(void) {
    char buf[64];
    snprintf(buf, sizeof buf, "%d %u %x %ld", -42, 42u, 0xBEEFu, -9000000000l);
    KT_CHECK(!strcmp(buf, "-42 42 beef -9000000000"));
    snprintf(buf, sizeof buf, "[%5d][%-5d][%05u][%lu]", 7, 7, 7u, 18446744073709551615ul);
    KT_CHECK(!strcmp(buf, "[    7][7    ][00007][18446744073709551615]"));
    snprintf(buf, sizeof buf, "[%4d][%-4d][%05d]", -7, -7, -7);
    KT_CHECK(!strcmp(buf, "[  -7][-7  ][-0007]"));
    snprintf(buf, sizeof buf, "[%s][%-4s][%4s][%c][%%]", "hi", "ab", "ab", 'z');
    KT_CHECK(!strcmp(buf, "[hi][ab  ][  ab][z][%]"));
    snprintf(buf, sizeof buf, "%p", (void *)0x1234);
    KT_CHECK(!strcmp(buf, "0x0000000000001234"));
}

static void t_lib_fmt_truncate(void) {
    char buf[6];
    memset(buf, 'X', sizeof buf);
    int n = snprintf(buf, sizeof buf, "%s-%d", "abcdef", 12345);
    KT_CHECK(n == 12);              /* the length it wanted */
    KT_CHECK(!strcmp(buf, "abcde")); /* what fit, terminated */
    KT_CHECK(snprintf(0, 0, "%d", 123) == 3);
}

/* ---- memory ------------------------------------------------------------ */

static void t_pmm_alloc(void) {
    uint64_t before = pmm_free_pages();
    uint64_t p = pmm_alloc();
    KT_REQUIRE(p != 0);
    KT_CHECK((p & 0xFFF) == 0);
    KT_CHECK(pmm_free_pages() == before - 1);
    uint8_t *v = P2V(p);
    int zero = 1;
    for (int i = 0; i < 4096; i++) zero &= v[i] == 0;
    KT_CHECK(zero);
    memset(v, 0xA5, 4096); /* dirty it: the next owner must still get zeroes */
    pmm_free(p);
    KT_CHECK(pmm_free_pages() == before);
    uint64_t q = pmm_alloc();
    KT_REQUIRE(q != 0);
    KT_CHECK(*(uint64_t *)P2V(q) == 0 && ((uint8_t *)P2V(q))[4095] == 0);
    pmm_free(q);
    KT_CHECK(pmm_free_pages() <= pmm_total_pages());
}

static void t_pmm_contig(void) {
    uint64_t before = pmm_free_pages();
    uint64_t a = pmm_alloc_contig(8), b = pmm_alloc_contig(8);
    KT_REQUIRE(a && b);
    KT_CHECK((a & 0xFFF) == 0 && (b & 0xFFF) == 0);
    KT_CHECK(a + 8 * PAGE_SIZE <= b || b + 8 * PAGE_SIZE <= a); /* no overlap */
    KT_CHECK(pmm_free_pages() == before - 16);
    pmm_free_contig(a, 8);
    pmm_free_contig(b, 8);
    KT_CHECK(pmm_free_pages() == before);
    KT_CHECK(pmm_alloc_contig(0) == 0);
    KT_CHECK(pmm_alloc_contig(pmm_free_pages() + 1) == 0); /* more than there is */
}

static void t_heap(void) {
    uint8_t *a = kmalloc(100), *b = kmalloc(100), *c = kmalloc(1);
    KT_REQUIRE(a && b && c);
    KT_CHECK(((uint64_t)a & 15) == 0 && ((uint64_t)b & 15) == 0 && ((uint64_t)c & 15) == 0);
    KT_CHECK(a + 100 <= b || b + 100 <= a);
    int zero = 1;
    for (int i = 0; i < 100; i++) zero &= a[i] == 0 && b[i] == 0;
    KT_CHECK(zero);
    memset(a, 0xFF, 100);
    memset(b, 0xEE, 100);
    KT_CHECK(c[0] == 0); /* neighbours untouched */
    kfree(b);
    uint8_t *d = kmalloc(100); /* reuses freed memory, zeroed again */
    KT_REQUIRE(d);
    KT_CHECK(d[0] == 0 && d[99] == 0);
    kfree(a);
    kfree(c);
    kfree(d);
    kfree(0); /* allowed, does nothing */
}

static void t_heap_large(void) {
    uint8_t *big = kmalloc(1 << 20);
    KT_REQUIRE(big);
    big[0] = 1;
    big[(1 << 20) - 1] = 2;
    KT_CHECK(big[0] == 1 && big[(1 << 20) - 1] == 2);
    kfree(big);
    KT_CHECK(kmalloc(1ull << 40) == 0); /* refused, not wrapped around */
}

static void t_vmm_map(void) {
    uint64_t before = pmm_free_pages();
    uint64_t as = vmm_new_address_space();
    KT_REQUIRE(as);
    uint64_t page = pmm_alloc(), flags = 0;
    KT_REQUIRE(page);
    const uint64_t va = 0x400000;
    KT_CHECK(vmm_translate(as, va, 0) == 0);
    KT_REQUIRE(vmm_map(as, va, page, PTE_US | PTE_RW | PTE_NX) == 0);
    KT_CHECK(vmm_translate(as, va + 0x123, &flags) == page + 0x123);
    KT_CHECK((flags & PTE_P) && (flags & PTE_US) && (flags & PTE_RW) && (flags & PTE_NX));
    KT_CHECK(vmm_translate(as, va + PAGE_SIZE, 0) == 0); /* only the one page */
    KT_CHECK(vmm_user_pages(as) == 1);
    vmm_unmap(as, va);
    KT_CHECK(vmm_translate(as, va, 0) == 0);
    KT_CHECK(vmm_map(as, va, page, PTE_US) == 0);
    vmm_destroy_address_space(as); /* frees the page and the tables */
    KT_CHECK(pmm_free_pages() == before);
}

static void t_vmm_kernel_half(void) {
    uint64_t as = vmm_new_address_space();
    KT_REQUIRE(as);
    uint64_t k = (uint64_t)&t_vmm_kernel_half, f1 = 0, f2 = 0;
    KT_CHECK(vmm_translate(as, k, &f1) != 0);
    KT_CHECK(vmm_translate(as, k, &f1) == vmm_translate(vmm_kernel_pml4(), k, &f2));
    KT_CHECK(f1 == f2);
    KT_CHECK(vmm_user_pages(as) == 0); /* nothing of the kernel counts as user memory */
    vmm_destroy_address_space(as);
}

static void t_vmm_wx(void) {
    uint64_t pml4 = vmm_kernel_pml4(), f;
    static uint64_t some_data;
    KT_REQUIRE(vmm_translate(pml4, (uint64_t)__text_start, &f));
    KT_CHECK(!(f & PTE_RW) && !(f & PTE_NX) && !(f & PTE_US)); /* code: execute, never write */
    KT_REQUIRE(vmm_translate(pml4, (uint64_t)__rodata_start, &f));
    KT_CHECK(!(f & PTE_RW) && (f & PTE_NX) && !(f & PTE_US)); /* constants: read only */
    KT_REQUIRE(vmm_translate(pml4, (uint64_t)&some_data, &f));
    KT_CHECK((f & PTE_RW) && (f & PTE_NX) && !(f & PTE_US)); /* data: never execute */
    uint64_t page = pmm_alloc();
    KT_REQUIRE(page);
    KT_CHECK(vmm_translate(pml4, (uint64_t)P2V(page), &f) == page);
    KT_CHECK((f & PTE_NX) && !(f & PTE_US)); /* direct map of RAM: never execute */
    pmm_free(page);
    KT_CHECK(vmm_translate(pml4, 0, 0) == 0); /* no identity map: null is unmapped */
}

/* ---- crypto ------------------------------------------------------------ */

static void t_crypto_vectors(void) { KT_CHECK(crypto_selftest() == 0); /* SHA-256, HMAC, PBKDF2, HKDF, ChaCha20, Poly1305 */ }

static void t_crypto_aead(void) {
    static uint8_t key[32], key2[32], nonce[12], msg[200], ct[200], pt[200];
    uint8_t tag[16];
    for (int i = 0; i < 32; i++) key[i] = (uint8_t)i, key2[i] = (uint8_t)i;
    key2[31] ^= 1;
    for (int i = 0; i < 200; i++) msg[i] = (uint8_t)(i * 7);
    aead_seal(key, nonce, "aad", 3, msg, sizeof msg, ct, tag);
    KT_CHECK(memcmp(ct, msg, sizeof msg) != 0);
    KT_CHECK(aead_open(key, nonce, "aad", 3, ct, sizeof ct, pt, tag) == 0 && !memcmp(pt, msg, sizeof msg));
    KT_CHECK(aead_open(key2, nonce, "aad", 3, ct, sizeof ct, pt, tag) == -1); /* wrong key */
    KT_CHECK(aead_open(key, nonce, "aaD", 3, ct, sizeof ct, pt, tag) == -1); /* wrong context */
    nonce[0] = 1;
    KT_CHECK(aead_open(key, nonce, "aad", 3, ct, sizeof ct, pt, tag) == -1); /* wrong nonce */
    nonce[0] = 0;
    tag[15] ^= 0x80;
    KT_CHECK(aead_open(key, nonce, "aad", 3, ct, sizeof ct, pt, tag) == -1); /* forged tag */
    int zero = 1;
    for (int i = 0; i < 200; i++) zero &= pt[i] == 0;
    KT_CHECK(zero); /* nothing of a rejected message is released */
}

static void t_crypto_helpers(void) {
    uint8_t a[32], b[32];
    memset(a, 7, sizeof a);
    memcpy(b, a, sizeof b);
    KT_CHECK(crypto_memeq(a, b, 32) == 1);
    b[31] = 8;
    KT_CHECK(crypto_memeq(a, b, 32) == 0);
    crypto_wipe(a, sizeof a);
    KT_CHECK(a[0] == 0 && a[31] == 0);
}

static void t_rng(void) {
    uint64_t a[4], b[4];
    random_bytes(a, sizeof a);
    random_bytes(b, sizeof b);
    KT_CHECK(memcmp(a, b, sizeof a) != 0);
    KT_CHECK((a[0] | a[1] | a[2] | a[3]) != 0);
    KT_CHECK(random_u64() != random_u64());
}

/* ---- files ------------------------------------------------------------- */

#define SCRATCH_SECTORS 8192 /* 4 MiB: 1024 blocks */
static struct blkdev *scratch;
static const char PW[] = "kernel self-test";
static uint8_t fbuf[3 * AFS_PAYLOAD], fbuf2[3 * AFS_PAYLOAD], sect[4096];

static int scratch_has(const char *needle) {
    for (uint64_t lba = 0; lba < SCRATCH_SECTORS; lba += 8) {
        if (blk_read(scratch, lba, 8, sect)) return -1;
        if (contains((char *)sect, sizeof sect, needle)) return 1;
    }
    return 0;
}

static void corrupt_block(uint32_t blk, int byte) {
    blk_read(scratch, (uint64_t)blk * 8, 8, sect);
    sect[byte] ^= 0x01;
    blk_write(scratch, (uint64_t)blk * 8, 8, sect);
    afs_test_drop_cache();
}

static void poke_u32(uint32_t blk, int off, uint32_t value) {
    blk_read(scratch, (uint64_t)blk * 8, 8, sect);
    memcpy(sect + off, &value, 4);
    blk_write(scratch, (uint64_t)blk * 8, 8, sect);
    afs_test_drop_cache();
}

static void fill_pattern(uint8_t *p, size_t n, uint32_t seed) {
    for (size_t i = 0; i < n; i++) p[i] = (uint8_t)((i * 31 + seed) ^ (i >> 8));
    memcpy(p + 100, "MARKER-PLAINTEXT", 16);
}

/* Each files test runs on a freshly formatted scratch volume. */
static void with_scratch(void (*body)(void)) {
    if (!scratch) scratch = ramdisk_create(SCRATCH_SECTORS);
    KT_REQUIRE(scratch);
    KT_REQUIRE(afs_test_begin() == 0);
    memset(sect, 0, sizeof sect);
    for (uint64_t lba = 0; lba < SCRATCH_SECTORS; lba += 8) blk_write(scratch, lba, 8, sect);
    KT_CHECK(afs_probe(scratch) == AFS_PROBE_BLANK);
    if (afs_format(scratch, PW, sizeof PW - 1) == 0) body();
    else KT_CHECK(!"afs_format failed");
    afs_test_end();
}

static void files_format(void) {
    KT_CHECK(afs_mounted());
    KT_CHECK(afs_probe(scratch) == AFS_PROBE_AFS);
    struct fs_info fi;
    afs_info(&fi);
    KT_CHECK(fi.kind == FS_ENCRYPTED && fi.free_blocks > 0 && fi.free_blocks <= fi.total_blocks);
    struct afs_inode root;
    KT_CHECK(afs_iget(AFS_ROOT, &root) == 0 && root.mode == AFS_DIR);
    KT_CHECK(afs_dir_empty(AFS_ROOT));
}

static void files_rw(void) {
    int ino = afs_ialloc(AFS_FILE, LABEL_PERSONAL, AFS_ROOT);
    KT_REQUIRE(ino > AFS_ROOT);
    fill_pattern(fbuf, sizeof fbuf, 1);
    KT_CHECK(afs_write((uint32_t)ino, 10, fbuf, sizeof fbuf) == (int64_t)sizeof fbuf); /* spans 4 blocks */
    memset(fbuf2, 0, sizeof fbuf2);
    KT_CHECK(afs_read((uint32_t)ino, 10, fbuf2, sizeof fbuf2) == (int64_t)sizeof fbuf2);
    KT_CHECK(!memcmp(fbuf, fbuf2, sizeof fbuf));
    KT_CHECK(afs_read((uint32_t)ino, 0, fbuf2, 10) == 10 && fbuf2[0] == 0 && fbuf2[9] == 0); /* hole reads as 0 */
    KT_CHECK(afs_read((uint32_t)ino, 10 + sizeof fbuf, fbuf2, 1) == 0);                    /* end of file */
    /* Far past the direct blocks: goes through the indirect block. */
    uint64_t far = (uint64_t)AFS_NDIRECT * AFS_PAYLOAD + 5000;
    KT_CHECK(afs_write((uint32_t)ino, far, "indirect", 8) == 8);
    KT_CHECK(afs_read((uint32_t)ino, far, fbuf2, 8) == 8 && !memcmp(fbuf2, "indirect", 8));
    struct afs_inode n;
    KT_CHECK(afs_iget((uint32_t)ino, &n) == 0 && n.size == far + 8 && n.label == LABEL_PERSONAL);
    afs_test_drop_cache(); /* read it back from the disk, decrypting again */
    KT_CHECK(afs_read((uint32_t)ino, 10, fbuf2, sizeof fbuf2) == (int64_t)sizeof fbuf2 && !memcmp(fbuf, fbuf2, sizeof fbuf));
}

static void files_dirs(void) {
    int dir = afs_ialloc(AFS_DIR, LABEL_PERSONAL, AFS_ROOT), f = afs_ialloc(AFS_FILE, LABEL_SECRET, AFS_ROOT);
    KT_REQUIRE(dir > 0 && f > 0);
    KT_CHECK(afs_dir_add(AFS_ROOT, "work", (uint32_t)dir, AFS_DIR) == 0);
    KT_CHECK(afs_dir_add((uint32_t)dir, "salary.txt", (uint32_t)f, AFS_FILE) == 0);
    KT_CHECK(afs_dir_add((uint32_t)dir, "salary.txt", (uint32_t)f, AFS_FILE) == -E_EXIST);
    KT_CHECK(afs_dir_add((uint32_t)dir, "", (uint32_t)f, AFS_FILE) < 0);   /* empty name */
    KT_CHECK(afs_dir_add((uint32_t)dir, "a/b", (uint32_t)f, AFS_FILE) < 0); /* no separators in names */
    uint32_t got = 0;
    KT_CHECK(afs_lookup((uint32_t)dir, "salary.txt", &got) == 0 && got == (uint32_t)f);
    KT_CHECK(afs_lookup((uint32_t)dir, "nope", &got) == -E_NOENT);
    char name[NAME_MAX + 2];
    uint32_t ino;
    uint8_t type;
    KT_CHECK(afs_dir_entry((uint32_t)dir, 0, name, &ino, &type) == 1 && !strcmp(name, "salary.txt") && type == AFS_FILE);
    KT_CHECK(afs_dir_entry((uint32_t)dir, 1, name, &ino, &type) == 0);
    /* A maximum-length (NAME_MAX) name must survive afs_dir_entry intact:
     * the over-read clamp there must not shorten a legitimate name. */
    static const char maxname[] = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzABCDEF"; /* 58 */
    KT_CHECK(strlen(maxname) == NAME_MAX);
    KT_CHECK(afs_dir_add((uint32_t)dir, maxname, (uint32_t)f, AFS_FILE) == 0);
    int seen = 0;
    for (uint32_t i = 0; afs_dir_entry((uint32_t)dir, i, name, &ino, &type) == 1; i++)
        if (!strcmp(name, maxname)) seen = 1;
    KT_CHECK(seen);
    KT_CHECK(afs_dir_remove((uint32_t)dir, maxname) == 0);
    KT_CHECK(!afs_dir_empty((uint32_t)dir));
    KT_CHECK(afs_dir_remove((uint32_t)dir, "salary.txt") == 0);
    KT_CHECK(afs_dir_empty((uint32_t)dir));
    struct afs_inode n;
    KT_CHECK(afs_iget((uint32_t)f, &n) == 0 && n.label == LABEL_SECRET); /* labels are stored */
}

static void files_free(void) {
    struct fs_info a, b;
    afs_info(&a);
    int ino = afs_ialloc(AFS_FILE, LABEL_PUBLIC, AFS_ROOT);
    KT_REQUIRE(ino > 0);
    fill_pattern(fbuf, sizeof fbuf, 2);
    KT_CHECK(afs_write((uint32_t)ino, 0, fbuf, sizeof fbuf) == (int64_t)sizeof fbuf);
    afs_info(&b);
    KT_CHECK(b.free_blocks < a.free_blocks && b.free_inodes == a.free_inodes - 1);
    KT_CHECK(afs_truncate((uint32_t)ino, 100) == 0);
    KT_CHECK(afs_read((uint32_t)ino, 0, fbuf2, sizeof fbuf2) == 100);
    KT_CHECK(afs_ifree((uint32_t)ino) == 0);
    afs_info(&b);
    KT_CHECK(b.free_blocks == a.free_blocks && b.free_inodes == a.free_inodes); /* nothing leaked */
}

static void files_ciphertext(void) {
    int ino = afs_ialloc(AFS_FILE, LABEL_PERSONAL, AFS_ROOT);
    KT_REQUIRE(ino > 0);
    KT_CHECK(afs_dir_add(AFS_ROOT, "DIRNAME-PLAINTEXT", (uint32_t)ino, AFS_FILE) == 0);
    fill_pattern(fbuf, sizeof fbuf, 3);
    KT_CHECK(afs_write((uint32_t)ino, 0, fbuf, sizeof fbuf) == (int64_t)sizeof fbuf);
    afs_sync();
    KT_CHECK(scratch_has("MARKER-PLAINTEXT") == 0);  /* contents are encrypted on disk */
    KT_CHECK(scratch_has("DIRNAME-PLAINTEXT") == 0); /* and so are file names */
    KT_CHECK(scratch_has(PW) == 0);                  /* the passphrase is never stored */
}

static void files_tamper(void) {
    int ino = afs_ialloc(AFS_FILE, LABEL_PERSONAL, AFS_ROOT);
    KT_REQUIRE(ino > 0);
    fill_pattern(fbuf, sizeof fbuf, 4);
    KT_REQUIRE(afs_write((uint32_t)ino, 0, fbuf, 2 * AFS_PAYLOAD) == 2 * AFS_PAYLOAD);
    struct afs_inode n;
    KT_REQUIRE(afs_iget((uint32_t)ino, &n) == 0 && n.direct[0] && n.direct[1]);
    corrupt_block(n.direct[0], 200); /* one flipped bit in the ciphertext */
    KT_CHECK(afs_read((uint32_t)ino, 0, fbuf2, 16) == -E_IO);
    KT_CHECK(afs_read((uint32_t)ino, AFS_PAYLOAD, fbuf2, 16) == 16); /* other blocks still fine */
    corrupt_block(n.direct[0], 200);                                 /* undo */
    KT_CHECK(afs_read((uint32_t)ino, 0, fbuf2, 16) == 16 && !memcmp(fbuf, fbuf2, 16));
}

static void files_swap(void) {
    int ino = afs_ialloc(AFS_FILE, LABEL_PERSONAL, AFS_ROOT);
    KT_REQUIRE(ino > 0);
    fill_pattern(fbuf, sizeof fbuf, 5);
    KT_REQUIRE(afs_write((uint32_t)ino, 0, fbuf, 2 * AFS_PAYLOAD) == 2 * AFS_PAYLOAD);
    struct afs_inode n;
    KT_REQUIRE(afs_iget((uint32_t)ino, &n) == 0 && n.direct[0] && n.direct[1]);
    /* Swap two intact, validly sealed blocks: each is bound to its position. */
    static uint8_t b0[4096], b1[4096];
    blk_read(scratch, (uint64_t)n.direct[0] * 8, 8, b0);
    blk_read(scratch, (uint64_t)n.direct[1] * 8, 8, b1);
    blk_write(scratch, (uint64_t)n.direct[0] * 8, 8, b1);
    blk_write(scratch, (uint64_t)n.direct[1] * 8, 8, b0);
    afs_test_drop_cache();
    KT_CHECK(afs_read((uint32_t)ino, 0, fbuf2, 16) == -E_IO);
    KT_CHECK(afs_read((uint32_t)ino, AFS_PAYLOAD, fbuf2, 16) == -E_IO);
}

static void files_shared_cache(void) {
    int a = afs_ialloc(AFS_FILE, LABEL_PERSONAL, AFS_ROOT);
    int b = afs_ialloc(AFS_FILE, LABEL_PERSONAL, AFS_ROOT);
    KT_REQUIRE(a > 0 && b > 0);
    fill_pattern(fbuf, sizeof fbuf, 6);
    fill_pattern(fbuf2, sizeof fbuf2, 7);
    KT_REQUIRE(afs_write((uint32_t)a, 0, fbuf, AFS_PAYLOAD) == AFS_PAYLOAD);
    KT_REQUIRE(afs_write((uint32_t)b, 0, fbuf2, AFS_PAYLOAD) == AFS_PAYLOAD);

    struct afs_inode na, nb;
    KT_REQUIRE(afs_iget((uint32_t)a, &na) == 0 && afs_iget((uint32_t)b, &nb) == 0);
    KT_REQUIRE(na.direct[0] && nb.direct[0] && na.direct[0] != nb.direct[0]);

    /* A hostile disk can make two inodes share one physical data block. The
     * block is sealed for A, so B must not get A's plaintext from the cache;
     * the AEAD check under B's file key/AAD has to run and fail. */
    nb.direct[0] = na.direct[0];
    KT_REQUIRE(afs_iput((uint32_t)b, &nb) == 0);
    afs_test_drop_cache();
    KT_REQUIRE(afs_read((uint32_t)a, 0, sect, AFS_PAYLOAD) == AFS_PAYLOAD && !memcmp(sect, fbuf, AFS_PAYLOAD));
    KT_CHECK(afs_read((uint32_t)b, 0, sect, AFS_PAYLOAD) == -E_IO);

    /* Re-sealing the shared block for B must drop A's cached plaintext too. */
    KT_REQUIRE(afs_write((uint32_t)b, 0, fbuf2, AFS_PAYLOAD) == AFS_PAYLOAD);
    memset(sect, 0, sizeof sect);
    KT_CHECK(afs_read((uint32_t)a, 0, sect, AFS_PAYLOAD) == -E_IO);
    KT_CHECK(afs_read((uint32_t)b, 0, sect, AFS_PAYLOAD) == AFS_PAYLOAD && !memcmp(sect, fbuf2, AFS_PAYLOAD));
}

static void files_unlock(void) {
    int ino = afs_ialloc(AFS_FILE, LABEL_CONFIDENTIAL, AFS_ROOT);
    KT_REQUIRE(ino > 0);
    KT_REQUIRE(afs_dir_add(AFS_ROOT, "kept.txt", (uint32_t)ino, AFS_FILE) == 0);
    KT_REQUIRE(afs_write((uint32_t)ino, 0, "survives a remount", 18) == 18);
    afs_sync();
    afs_test_unmount();
    KT_CHECK(afs_unlock(scratch, "wrong passphrase", 16) == -E_DENIED);
    KT_CHECK(!afs_mounted());
    afs_test_unmount();
    KT_REQUIRE(afs_unlock(scratch, PW, sizeof PW - 1) == 0);
    uint32_t got;
    KT_CHECK(afs_lookup(AFS_ROOT, "kept.txt", &got) == 0 && got == (uint32_t)ino);
    KT_CHECK(afs_read((uint32_t)ino, 0, fbuf2, 18) == 18 && !memcmp(fbuf2, "survives a remount", 18));
    struct afs_inode n;
    KT_CHECK(afs_iget((uint32_t)ino, &n) == 0 && n.label == LABEL_CONFIDENTIAL);

    /* Re-wrapping the volume key must require the old passphrase and survive
     * a remount with the new one. */
    KT_CHECK(afs_change_passphrase("wrong", 5, "new passphrase", 14) == -E_DENIED);
    KT_CHECK(afs_change_passphrase(PW, sizeof PW - 1, "short", 5) == -E_INVAL);
    KT_CHECK(afs_change_passphrase(PW, sizeof PW - 1, "new passphrase", 14) == 0);
    KT_CHECK(afs_verify(PW, sizeof PW - 1) == -E_DENIED);
    KT_CHECK(afs_verify("new passphrase", 14) == 0);
    afs_sync();
    afs_test_unmount();
    KT_REQUIRE(afs_unlock(scratch, "new passphrase", 14) == 0);
    KT_CHECK(afs_lookup(AFS_ROOT, "kept.txt", &got) == 0 && got == (uint32_t)ino);
    KT_CHECK(afs_read((uint32_t)ino, 0, fbuf2, 18) == 18 && !memcmp(fbuf2, "survives a remount", 18));
}

static void files_superblock(void) {
    afs_sync();
    afs_test_unmount();
    /* Byte 104 is the creation time: not checked on its own, but covered by
     * the key wrap's tag, so any edit to the header makes unlocking fail. */
    corrupt_block(0, 104);
    KT_CHECK(afs_unlock(scratch, PW, sizeof PW - 1) == -E_DENIED);
    afs_test_unmount();
    corrupt_block(0, 104);
    KT_CHECK(afs_unlock(scratch, PW, sizeof PW - 1) == 0);

    /* Structural arithmetic must not wrap in 32 bits. A huge inode_blocks
     * (offset 32 in the superblock) used to make the unlock scan walk past
     * the inode table and write beyond its bitmap. */
    afs_sync();
    afs_test_unmount();
    blk_read(scratch, 0, 8, sect);
    static uint8_t saved[4096];
    memcpy(saved, sect, sizeof saved);
    poke_u32(0, 32, 0x10000001u);
    KT_CHECK(afs_unlock(scratch, PW, sizeof PW - 1) == -E_INVAL);
    blk_write(scratch, 0, 8, saved);
    afs_test_drop_cache();
    KT_CHECK(afs_unlock(scratch, PW, sizeof PW - 1) == 0);
}

/*
 * The rules the file explorer (user/explorer.c) leans on, through the same
 * path calls apps make, as the caller (the terminal, which holds CAP_FILES):
 * folders can't move into themselves, non-empty folders aren't removed
 * behind your back, names don't collide, the root stays, ".." is refused.
 */
static void files_tree(void) {
    KT_REQUIRE(current->caps & CAP_FILES);
    /* The live volume's /home and /apps (made one after the other at setup)
     * are protected by inode number; step past those numbers here so only
     * the rules under test decide. */
    char pad[16];
    uint32_t ino = 0;
    for (int i = 0; i < 16 && ino <= vfs_home() + 1; i++) {
        snprintf(pad, sizeof pad, "/pad%d", i);
        KT_REQUIRE(vfs_mkdir(pad) == 0 && vfs_lookup(pad, &ino) == 0);
    }
    KT_REQUIRE(ino > vfs_home() + 1);
    KT_REQUIRE(vfs_mkdir("/t") == 0 && vfs_mkdir("/t/sub") == 0);
    int64_t h = vfs_open("/t/f.txt", O_WRITE | O_CREATE);
    KT_REQUIRE(h >= 0);
    sys_close((uint64_t)h);
    KT_CHECK(vfs_mkdir("/t") == -E_EXIST);
    KT_CHECK(vfs_rename("/t", "/t/sub/t2") == -E_INVAL); /* into itself */
    KT_CHECK(vfs_rename("/t", "/t/t2") == -E_INVAL);
    KT_CHECK(vfs_remove("/t") == -E_NOTEMPTY);
    KT_CHECK(vfs_rename("/t/f.txt", "/t/sub") == -E_EXIST); /* never replaces */
    KT_CHECK(vfs_rename("/t/f.txt", "/t/sub/f.txt") == 0);  /* a move: cut and paste */
    KT_CHECK(vfs_lookup("/t/f.txt", &ino) == -E_NOENT && vfs_lookup("/t/sub/f.txt", &ino) == 0);
    KT_CHECK(vfs_mkdir("/t/../x") == -E_PERM && vfs_lookup("/x", &ino) == -E_NOENT);
    KT_CHECK(vfs_remove("/") == -E_PERM && vfs_rename("/", "/y") == -E_PERM);
    KT_CHECK(vfs_remove("/t/sub/f.txt") == 0 && vfs_remove("/t/sub") == 0 && vfs_remove("/t") == 0);
    KT_CHECK(vfs_lookup("/t", &ino) == -E_NOENT);
}

static void t_files_format(void) { with_scratch(files_format); }
static void t_files_tree(void) { with_scratch(files_tree); }
static void t_files_rw(void) { with_scratch(files_rw); }
static void t_files_dirs(void) { with_scratch(files_dirs); }
static void t_files_free(void) { with_scratch(files_free); }
static void t_files_ciphertext(void) { with_scratch(files_ciphertext); }
static void t_files_tamper(void) { with_scratch(files_tamper); }
static void t_files_swap(void) { with_scratch(files_swap); }
static void t_files_shared_cache(void) { with_scratch(files_shared_cache); }
static void t_files_unlock(void) { with_scratch(files_unlock); }
static void t_files_superblock(void) { with_scratch(files_superblock); }

static void t_files_foreign(void) {
    if (!scratch) scratch = ramdisk_create(SCRATCH_SECTORS);
    KT_REQUIRE(scratch);
    memset(sect, 0, sizeof sect);
    memcpy(sect + 510, "\x55\xAA", 2); /* looks like someone's partition table */
    blk_write(scratch, 0, 8, sect);
    KT_CHECK(afs_probe(scratch) == AFS_PROBE_OTHER); /* storage never formats this */
}

/* ---- dlp --------------------------------------------------------------- */

static struct proc *fake_proc(void) {
    struct proc *p = kmalloc(sizeof *p);
    if (p) strlcpy(p->name, "selftest", sizeof p->name);
    return p;
}

static void t_dlp_names(void) {
    KT_CHECK(!strcmp(dlp_label_name(LABEL_PUBLIC), "Public") && !strcmp(dlp_label_name(LABEL_SECRET), "Secret"));
    KT_CHECK(!strcmp(dlp_label_name(-1), "?") && !strcmp(dlp_label_name(LABEL_COUNT), "?"));
    KT_CHECK(LABEL_PUBLIC < LABEL_PERSONAL && LABEL_PERSONAL < LABEL_CONFIDENTIAL && LABEL_CONFIDENTIAL < LABEL_SECRET);
}

static void t_dlp_taint(void) {
    struct proc *p = fake_proc();
    KT_REQUIRE(p);
    KT_CHECK(p->taint == LABEL_PUBLIC);
    dlp_taint(p, LABEL_PERSONAL, "a self-test file");
    KT_CHECK(p->taint == LABEL_PERSONAL);
    dlp_taint(p, LABEL_PUBLIC, "a self-test file");
    KT_CHECK(p->taint == LABEL_PERSONAL); /* reading lower data never lowers it */
    dlp_taint(p, LABEL_SECRET, "a self-test file");
    KT_CHECK(p->taint == LABEL_SECRET);
    dlp_taint(p, LABEL_CONFIDENTIAL, "a self-test file");
    KT_CHECK(p->taint == LABEL_SECRET);
    kfree(p);
}

static void t_dlp_egress(void) {
    struct proc *p = fake_proc();
    KT_REQUIRE(p);
    KT_CHECK(dlp_egress(p, DLP_EGRESS_LOG) == 0);
    p->taint = LABEL_PERSONAL;
    KT_CHECK(dlp_egress(p, DLP_EGRESS_LOG) == 0);
    p->taint = LABEL_CONFIDENTIAL;
    KT_CHECK(dlp_egress(p, DLP_EGRESS_LOG) == -E_DENIED);
    KT_CHECK(p->dlp_warned); /* audited once, not once per attempt */
    p->taint = LABEL_SECRET;
    KT_CHECK(dlp_egress(p, DLP_EGRESS_LOG) == -E_DENIED);
    kfree(p);
}

static void t_dlp_audit(void) {
    static char log[8192];
    struct proc *p = fake_proc();
    KT_REQUIRE(p);
    p->pid = 9999;
    dlp_taint(p, LABEL_CONFIDENTIAL, "an audit check");
    size_t n = dlp_log_read(log, sizeof log);
    KT_CHECK(n > 0 && n <= sizeof log);
    KT_CHECK(contains(log, n, "selftest (pid 9999) now holds Confidential data"));
    kfree(p);
}

/* ---- sandbox ----------------------------------------------------------- */

static const uint64_t never_untrusted[] = {SYS_LOG,  SYS_SYSINFO,  SYS_PROC_LIST, SYS_SPAWN,
                                           SYS_KILL, SYS_KLOG_READ, SYS_WAIT,      SYS_DLP_LOG};

static struct proc *spawn_quiet(const char *name, int parent) {
    const struct program *prog = program_find(name);
    int pid = prog ? proc_spawn(name, prog->caps, 0, parent) : -E_NOENT;
    return pid > 0 ? proc_find(pid) : 0;
}

/* Test processes are never scheduled: they're killed before this system call returns. */
static void stop(struct proc *p) {
    if (p) proc_kill(p, EXIT_KILLED_BY_WM, 0);
}

static void t_sandbox_untrusted(void) {
    struct proc *p = spawn_quiet("probe", current->pid);
    KT_REQUIRE(p);
    KT_CHECK(p->trust == TRUST_UNTRUSTED);
    KT_CHECK(p->caps == CAP_STORAGE); /* its manifest: a private folder, nothing else */
    int blocked = 1;
    for (unsigned i = 0; i < ARRAY_LEN(never_untrusted); i++) blocked &= !(p->syscall_allow & (1ull << never_untrusted[i]));
    KT_CHECK(blocked);
    KT_CHECK(p->syscall_allow & (1ull << SYS_EXIT));
    KT_CHECK(SYS_COUNT <= 64);                 /* the untrusted filter is one word */

    KT_CHECK(p->mem_quota_pages == 8 * 256);    /* 8 MiB */
    stop(p);
}

static void t_sandbox_inherit(void) {
    struct proc *probe = spawn_quiet("probe", current->pid);
    KT_REQUIRE(probe);
    probe->taint = LABEL_CONFIDENTIAL;
    /* The calculator is trusted, but started by an untrusted, tainted parent... */
    struct proc *child = spawn_quiet("calculator", probe->pid);
    KT_CHECK(child != 0);
    if (child) {
        KT_CHECK(child->trust == TRUST_UNTRUSTED); /* ...it never gains trust */
        KT_CHECK(child->taint == LABEL_CONFIDENTIAL); /* ...and the data's label follows */
        KT_CHECK(!(child->syscall_allow & (1ull << SYS_SPAWN)));
    }
    stop(child);
    stop(probe);
}

static void t_sandbox_layout(void) {
    struct proc *a = spawn_quiet("probe", current->pid), *b = spawn_quiet("probe", current->pid);
    KT_CHECK(a && b);
    if (a && b) {
        uint64_t f;
        KT_CHECK(a->pml4 != b->pml4 && a->pml4 != vmm_kernel_pml4()); /* separate address spaces */
        KT_CHECK(vmm_translate(a->pml4, 0x400000, &f) && (f & PTE_US) && !(f & PTE_RW) && !(f & PTE_NX)); /* code */
        KT_CHECK(vmm_translate(a->pml4, USER_STACK_TOP - 8, &f) && (f & PTE_US) && (f & PTE_RW) && (f & PTE_NX)); /* stack */
        KT_CHECK(vmm_translate(a->pml4, 0x400000, 0) != vmm_translate(b->pml4, 0x400000, 0)); /* private copies */
        KT_CHECK(vmm_translate(a->pml4, 0, 0) == 0); /* null is never mapped */
        KT_CHECK(a->mmap_next >= USER_MMAP_BASE && a->mmap_next < USER_MMAP_TOP && !(a->mmap_next & 0xFFF));
        KT_CHECK(a->mmap_next != b->mmap_next); /* randomized heap base */
    }
    stop(a);
    stop(b);
}

/* Files, the explorer: trusted, a window and paths, nothing else. */
static void t_sandbox_explorer(void) {
    const struct program *prog = program_find("explorer");
    KT_REQUIRE(prog);
    KT_CHECK(prog->caps == (CAP_WINDOW | CAP_FILES | CAP_CLIPBOARD) && prog->trust == TRUST_TRUSTED &&
             !strcmp(prog->title, "Files"));
    struct proc *p = spawn_quiet("explorer", current->pid);
    KT_REQUIRE(p);
    KT_CHECK(p->trust == TRUST_TRUSTED && p->caps == (CAP_WINDOW | CAP_FILES | CAP_CLIPBOARD)); /* no spawn, kill or system info */
    stop(p);
}

static void t_sandbox_system(void) {
    struct proc *k = proc_find(0);
    KT_REQUIRE(k);
    KT_CHECK(k == proc_find(0) && k->pid == 0);
    KT_CHECK(current->state == PROC_READY);
    KT_CHECK(proc_find(-1) == 0 && proc_find(1 << 30) == 0);
    struct proc_info list[MAX_PROCS];
    int n = proc_list(list, MAX_PROCS), found = 0;
    for (int i = 0; i < n; i++) found |= (int)list[i].pid == current->pid;
    KT_CHECK(n == proc_count() && found);
    KT_CHECK(proc_spawn("no-such-app", ~0u, 0, current->pid) == -E_NOENT);
}

/* ---- syscall ----------------------------------------------------------- */

static void t_syscall_pointers(void) {
    KT_CHECK(user_range_ok(0x1000, 0, 0));                     /* empty ranges are fine */
    KT_CHECK(!user_range_ok(0, 8, 0));                         /* null page */
    KT_CHECK(!user_range_ok(KERNEL_VIRT_BASE + 0x100000, 8, 0)); /* the kernel */
    KT_CHECK(!user_range_ok(HHDM_BASE, 8, 0));                 /* the direct map of RAM */
    KT_CHECK(!user_range_ok(USER_TOP - 8, 16, 0));             /* straddles the top */
    KT_CHECK(!user_range_ok(0x400000, ~0ull, 0));              /* length wraps around */
    uint64_t k = 0;
    KT_CHECK(copy_in(&k, KERNEL_VIRT_BASE + 0x100000, sizeof k) == -E_FAULT && k == 0);
    KT_CHECK(copy_out(KERNEL_VIRT_BASE + 0x100000, &k, sizeof k) == -E_FAULT);
}

static void t_syscall_caller(void) {
    /* Checked against the caller's own page tables (the terminal's). */
    KT_REQUIRE(current->pid != 0);
    KT_CHECK(user_range_ok(0x400000, 16, 0));  /* its code is readable */
    KT_CHECK(!user_range_ok(0x400000, 16, 1)); /* but not writable */
    KT_CHECK(user_range_ok(USER_STACK_TOP - 64, 64, 1));
    KT_CHECK(!user_range_ok(USER_STACK_TOP - 64, 128, 1)); /* runs past the stack's top */
    KT_CHECK(!user_range_ok(USER_MMAP_TOP, 8, 0));         /* unmapped */
}

static void t_syscall_path(void) {
    char path[PATH_MAX + 1];
    KT_CHECK(copy_path(path, 0x400000, 0) == -E_INVAL);
    KT_CHECK(copy_path(path, 0x400000, PATH_MAX + 1) == -E_INVAL);
    KT_CHECK(copy_path(path, KERNEL_VIRT_BASE + 0x100000, 4) == -E_FAULT);
}

/* ---- runner ------------------------------------------------------------ */

/* A one-segment ELF image: type, segment flags, and optionally an interpreter. */
static uint8_t *tiny_elf(uint16_t type, uint32_t flags, int interp, uint64_t *size) {
    enum { EH = 64, PH = 56 };
    uint8_t *img = kmalloc(8192);
    if (!img) return 0;
    memcpy(img, "\x7F" "ELF\x02\x01\x01", 7);
    *(uint16_t *)&img[16] = type;
    *(uint16_t *)&img[18] = 62;                      /* x86-64 */
    *(uint64_t *)&img[24] = type == 3 ? 0x1000 : 0x401000; /* entry */
    *(uint64_t *)&img[32] = EH;                       /* phoff */
    *(uint16_t *)&img[54] = PH;
    *(uint16_t *)&img[56] = (uint16_t)(interp ? 2 : 1);
    uint8_t *ph = img + EH;
    *(uint32_t *)&ph[0] = 1; /* PT_LOAD */
    *(uint32_t *)&ph[4] = flags;
    *(uint64_t *)&ph[8] = 0;                                  /* offset */
    *(uint64_t *)&ph[16] = type == 3 ? 0 : 0x400000;          /* vaddr */
    *(uint64_t *)&ph[32] = *(uint64_t *)&ph[40] = 8192;      /* filesz, memsz */
    if (interp) *(uint32_t *)&ph[PH] = 3; /* PT_INTERP */
    img[4096] = 0xC3;
    *size = 8192;
    return img;
}

static int try_load(uint8_t *img, uint64_t size, int allow_dyn, struct elf_info *ei) {
    uint64_t pml4 = vmm_new_address_space();
    if (!pml4) return -E_NOMEM;
    int r = elf_load(pml4, img, size, allow_dyn, ei);
    vmm_destroy_address_space(pml4);
    return r;
}

static void t_runner_elf(void) {
    uint64_t size;
    struct elf_info ei;
    uint8_t *img = tiny_elf(2, 5, 0, &size); /* r-x executable */
    KT_REQUIRE(img);
    KT_CHECK(try_load(img, size, 0, &ei) == 0 && ei.entry == 0x401000 && ei.base == 0);
    *(uint32_t *)&img[64 + 4] = 7; /* writable and executable */
    KT_CHECK(try_load(img, size, 0, &ei) == -E_INVAL);
    *(uint32_t *)&img[64 + 4] = 4; /* readable, but the entry is not executable */
    KT_CHECK(try_load(img, size, 0, &ei) == -E_INVAL);
    kfree(img);
    img = tiny_elf(2, 5, 1, &size); /* needs a dynamic loader */
    KT_CHECK(img && try_load(img, size, 1, &ei) == -E_INVAL);
    kfree(img);
    img = tiny_elf(2, 5, 0, &size);
    KT_CHECK(img && try_load(img, size - 4096, 0, &ei) == -E_INVAL); /* segment runs past the file */
    *(uint64_t *)&img[32] = ~0ull - 55; /* e_phoff + one header wraps round to 0 */
    KT_CHECK(try_load(img, size, 0, &ei) == -E_INVAL);
    kfree(img);
}

static void t_runner_pie(void) {
    uint64_t size;
    struct elf_info ei;
    uint8_t *img = tiny_elf(3, 5, 0, &size);
    KT_REQUIRE(img);
    KT_CHECK(try_load(img, size, 0, &ei) == -E_INVAL); /* only where the Linux layer asked for it */
    uint64_t bases[4];
    int ok = 1, differ = 0;
    for (int i = 0; i < 4; i++) {
        ok &= try_load(img, size, 1, &ei) == 0;
        bases[i] = ei.base;
        ok &= ei.base >= (1ull << 32) && !(ei.base & 0x1FFFFF) && ei.entry == ei.base + 0x1000 && ei.end < USER_MMAP_BASE;
        if (i) differ |= bases[i] != bases[0];
    }
    KT_CHECK(ok);
    KT_CHECK(differ); /* a random base each time */
    kfree(img);
}

static void t_runner_signature(void) {
    /* The verifier is the same RSA-2048 PKCS#1 v1.5 code the loader and the
     * App Store trust anchor use; this is a known-good test vector. */
    uint8_t digest[32];
    sha256((const uint8_t *)ktest_rsa_message, sizeof ktest_rsa_message - 1, digest);
    KT_CHECK(rsa_verify_sha256(digest, ktest_rsa_sig, ktest_rsa_pub));
    digest[0] ^= 1;
    KT_CHECK(!rsa_verify_sha256(digest, ktest_rsa_sig, ktest_rsa_pub)); /* changed message */
    digest[0] ^= 1;
    uint8_t sig[256];
    memcpy(sig, ktest_rsa_sig, sizeof sig);
    sig[255] ^= 1;
    KT_CHECK(!rsa_verify_sha256(digest, sig, ktest_rsa_pub)); /* changed signature */
}

static void t_runner_package(void) {
    uint64_t esize;
    uint8_t *elf = tiny_elf(3, 5, 0, &esize);
    uint8_t *pkg = kmalloc(sizeof(struct app_header) + 8192);
    KT_REQUIRE(elf && pkg);
    struct app_header *h = (struct app_header *)pkg;
    memset(h, 0, sizeof *h);
    memcpy(h->magic, APP_MAGIC, 8);
    h->header_size = sizeof *h;
    h->format = 1;
    strlcpy(h->name, "kt-app", sizeof h->name);
    h->abi = ABI_LINUX;
    h->elf_size = esize;
    memcpy(pkg + sizeof *h, elf, esize);
    sha256(pkg + sizeof *h, esize, h->elf_sha256);
    uint64_t size = sizeof *h + esize;
    KT_CHECK(app_check(h, size, 1) == 0);
    /* An optional icon follows the ELF; its bounds and hash are checked. */
    uint64_t icon_off = size;
    uint8_t *icon = pkg + icon_off;
    for (int i = 0; i < 16; i++) icon[i] = (uint8_t)(i * 17 + 1);
    h->icon_format = APP_ICON_RGBA;
    h->icon_width = 2;
    h->icon_height = 2;
    h->icon_offset = (uint32_t)icon_off;
    h->icon_size = 16;
    sha256(icon, 16, h->icon_sha256);
    size += 16;
    KT_CHECK(app_check(h, size, 1) == 0);
    icon[0] ^= 1; /* one bit of the icon changed */
    KT_CHECK(app_check(h, size, 1) != 0);
    icon[0] ^= 1;
    h->icon_offset = 0;
    KT_CHECK(app_check(h, size, 1) != 0); /* icon overlaps the header */
    h->icon_offset = (uint32_t)icon_off;
    KT_CHECK(app_check(h, size - 1, 1) != 0); /* truncated icon */
    h->icon_format = APP_ICON_NONE;
    h->icon_width = h->icon_height = h->icon_offset = h->icon_size = 0;
    size -= 16;
    KT_CHECK(app_check(h, size, 1) == 0);
    pkg[sizeof *h + 4096] ^= 1; /* one bit of the program changed */
    KT_CHECK(app_check(h, size, 1) != 0);
    pkg[sizeof *h + 4096] ^= 1;
    KT_CHECK(app_check(h, size - 1, 1) != 0); /* truncated */
    strlcpy(h->name, "terminal", sizeof h->name);
    KT_CHECK(app_check(h, size, 1) != 0); /* can't replace a system program */
    strlcpy(h->name, "../x", sizeof h->name);
    KT_CHECK(app_check(h, size, 1) != 0);
    /* A native-ABI package must be a fixed-address ET_EXEC, not a PIE. */
    strlcpy(h->name, "kt-app", sizeof h->name);
    h->abi = ABI_AETHER;
    KT_CHECK(app_check(h, size, 1) != 0);
    h->abi = ABI_LINUX;
    /* Signature fields: absent means all zero; a present signature must be a
     * 256-byte trailer after the icon and nothing may follow it. */
    KT_CHECK(app_check(h, size, 1) == 0);
    h->sig_algorithm = APP_SIG_RSA2048_SHA256;
    h->sig_size = APP_SIG_SIZE;
    h->sig_offset = (uint32_t)size;
    h->sig_publisher = APP_PUBLISHER_AETHER;
    KT_CHECK(app_check_signature_shape(h, size) != 0); /* no room for a trailer */
    h->sig_offset = (uint32_t)size - 8;
    KT_CHECK(app_check_signature_shape(h, size) != 0);
    h->sig_algorithm = APP_SIG_NONE;
    h->sig_size = h->sig_offset = h->sig_publisher = 0;
    KT_CHECK(app_check_signature_shape(h, size) == 0);
    kfree(elf);
    kfree(pkg);
}

static void t_runner_appstore(void) {
    uint64_t esize;
    uint8_t *elf = tiny_elf(3, 5, 0, &esize);
    uint8_t *pkg = kmalloc(sizeof(struct app_header) + esize + 16);
    KT_REQUIRE(elf && pkg);
    struct app_header *h = (struct app_header *)pkg;
    memset(h, 0, sizeof *h);
    memcpy(h->magic, APP_MAGIC, 8);
    h->header_size = sizeof *h;
    h->format = 1;
    strlcpy(h->name, "kt-store", sizeof h->name);
    strlcpy(h->title, "Store test", sizeof h->title);
    strlcpy(h->version, "1.0.0", sizeof h->version);
    h->abi = ABI_LINUX;
    h->elf_size = esize;
    memcpy(pkg + sizeof *h, elf, esize);
    sha256(pkg + sizeof *h, esize, h->elf_sha256);
    uint64_t size = sizeof *h + esize;
    /* A 2x2 icon travels in the package and is exposed through appstore_icon. */
    uint8_t *icon = pkg + size;
    for (int i = 0; i < 16; i++) icon[i] = (uint8_t)(0x11 + i);
    h->icon_format = APP_ICON_RGBA;
    h->icon_width = h->icon_height = 2;
    h->icon_offset = (uint32_t)size;
    h->icon_size = 16;
    sha256(icon, 16, h->icon_sha256);
    size += 16;
    uint64_t pages = ALIGN_UP(size, PAGE_SIZE) / PAGE_SIZE;
    uint64_t phys = pmm_alloc_contig(pages);
    KT_REQUIRE(phys);
    memcpy(P2V(phys), pkg, size);
    int err = appstore_install(phys, size, 0);
    KT_CHECK(err == 0);
    if (err) {
        pmm_free_contig(phys, pages);
    } else {
        const struct program *prog = program_find("kt-store");
        KT_CHECK(prog && prog->installed && !prog->system && prog->abi == ABI_LINUX);
        const uint8_t *idata = 0;
        uint32_t iw = 0, ih = 0;
        KT_CHECK(appstore_icon("kt-store", &idata, &iw, &ih) == 1);
        KT_CHECK(idata && iw == 2 && ih == 2 && idata[0] == 0x11 && idata[15] == 0x20);
        struct proc *live = spawn_quiet("kt-store", current->pid);
        KT_CHECK(live && proc_named_running("kt-store"));
        KT_CHECK(appstore_remove("kt-store") == -E_BUSY); /* can't remove a running app */
        /* Install a newer version while the old copy runs: it must be marked
         * pending, not restarted or deleted. */
        strlcpy(h->version, "2.0.0", sizeof h->version);
        uint64_t phys2 = pmm_alloc_contig(pages);
        KT_REQUIRE(phys2);
        memcpy(P2V(phys2), pkg, size);
        err = appstore_install(phys2, size, APP_INSTALL_REPLACE);
        KT_CHECK(err == 0);
        if (err) {
            pmm_free_contig(phys2, pages);
        } else {
            const struct program *p2 = program_find("kt-store");
            KT_CHECK(p2 && !strcmp(p2->version, "2.0.0"));
            KT_CHECK(proc_update_pending("kt-store"));
        }
        stop(live);
        KT_CHECK(appstore_remove("kt-store") == 0);
        KT_CHECK(program_find("kt-store") == 0);
    }
    kfree(elf);
    kfree(pkg);
}

static void t_runner_lazy(void) {
    struct proc *p = current;
    uint64_t pages0 = p->mem_pages, res0 = p->reserved_pages, va = umem_alloc_va(4 * PAGE_SIZE);
    KT_REQUIRE(va);
    KT_REQUIRE(umem_reserve(va, 4 * PAGE_SIZE, PROT_READ | PROT_WRITE) == 0);
    KT_CHECK(p->reserved_pages == res0 + 4 && p->mem_pages == pages0); /* nothing made yet */
    KT_CHECK(vmm_translate(p->pml4, va, 0) == 0);
    KT_CHECK(umem_fault(va + 10, 1, 0) == 0); /* first touch */
    uint64_t f, phys = vmm_translate(p->pml4, va, &f);
    KT_CHECK(phys && (f & PTE_US) && (f & PTE_RW) && (f & PTE_NX) && p->mem_pages == pages0 + 1);
    KT_CHECK(umem_fault(va + PAGE_SIZE, 0, 1) == -1);    /* never executable */
    KT_CHECK(umem_protect(va, 4 * PAGE_SIZE, 0) == 0);    /* PROT_NONE */
    KT_CHECK(vmm_translate(p->pml4, va, 0) == 0 && umem_fault(va, 0, 0) == -1);
    KT_CHECK(umem_protect(va, 4 * PAGE_SIZE, PROT_READ) == 0);
    KT_CHECK(vmm_translate(p->pml4, va, &f) == phys && !(f & PTE_RW)); /* same page, kept */
    KT_CHECK(umem_reserve(va, PAGE_SIZE, PROT_READ) == -E_EXIST);      /* no double booking */
    umem_release(va, 4 * PAGE_SIZE);
    KT_CHECK(p->mem_pages == pages0 && p->reserved_pages == res0);
    KT_CHECK(umem_range_free(va, 4 * PAGE_SIZE));
}

static void t_runner_quota(void) {
    struct proc *p = current;
    uint64_t quota = p->mem_quota_pages, va = umem_alloc_va(PAGE_SIZE);
    KT_REQUIRE(va && umem_reserve(va, PAGE_SIZE, PROT_READ | PROT_WRITE) == 0);
    p->mem_quota_pages = p->mem_pages; /* full */
    KT_CHECK(umem_fault(va, 1, 0) == -2);
    KT_CHECK(!user_range_ok(va, 8, 1)); /* the kernel can't be used to get round it */
    p->mem_quota_pages = quota;
    KT_CHECK(user_range_ok(va, 8, 1));
    umem_release(va, PAGE_SIZE);
}

static void t_runner_tables(void) {
    struct proc *p = current;
    uint64_t quota = p->mem_quota_pages, pt0 = p->pt_pages;
    /* One page in each of two fresh 1 GiB stretches: each needs new page tables. */
    uint64_t a = umem_alloc_va(1ull << 30);
    KT_REQUIRE(a && umem_alloc_va(1ull << 30) && umem_alloc_va(1ull << 30)); /* 3 GiB of room */
    a = ALIGN_UP(a, 1ull << 30);
    KT_REQUIRE(umem_reserve(a, PAGE_SIZE, PROT_READ | PROT_WRITE) == 0);
    KT_CHECK(p->pt_pages > pt0); /* the tables are charged, though no page was made */
    p->mem_quota_pages = p->mem_pages + p->pt_pages; /* full */
    KT_CHECK(umem_reserve(a + (1ull << 30), PAGE_SIZE, PROT_READ | PROT_WRITE) == -E_NOMEM);
    KT_CHECK(umem_range_free(a + (1ull << 30), PAGE_SIZE));
    p->mem_quota_pages = quota;
    umem_release(a, PAGE_SIZE);
    KT_CHECK(p->pt_pages < pt0 + 3); /* the last-level table is given back */
}

static void t_runner_remap(void) {
    struct proc *p = current;
    uint64_t pages0 = p->mem_pages, va = umem_alloc_va(2 * PAGE_SIZE), other = umem_alloc_va(PAGE_SIZE);
    KT_REQUIRE(va && other && umem_reserve(va, 2 * PAGE_SIZE, PROT_READ | PROT_WRITE) == 0);
    KT_REQUIRE(umem_reserve(other, PAGE_SIZE, PROT_READ | PROT_WRITE) == 0);
    KT_REQUIRE(umem_fault(va, 1, 0) == 0);
    uint64_t phys = vmm_translate(p->pml4, va, 0);
    int64_t moved = umem_remap(va, 2 * PAGE_SIZE, 64 * PAGE_SIZE, 1); /* can't grow in place: moves */
    KT_CHECK(moved > 0 && (uint64_t)moved != va && vmm_translate(p->pml4, (uint64_t)moved, 0) == phys);
    KT_CHECK(vmm_translate(p->pml4, va, 0) == 0 && umem_range_free(va, 2 * PAGE_SIZE));
    KT_CHECK(umem_remap(other, PAGE_SIZE, 2 * PAGE_SIZE, 0) == -E_NOMEM); /* the next mapping is in the way */
    /* Newly handed-out space never overlaps what was just grown. */
    uint64_t next = umem_alloc_va(PAGE_SIZE);
    KT_CHECK(moved <= 0 || next >= (uint64_t)moved + 64 * PAGE_SIZE);
    if (moved > 0) umem_release((uint64_t)moved, 64 * PAGE_SIZE);
    umem_release(other, 2 * PAGE_SIZE);
    KT_CHECK(p->mem_pages == pages0);
}

/* ---- Network ------------------------------------------------------------ */

static void t_net_checksum(void) {
    /* RFC 1071's example: the one's-complement sum of these is 0xDDF2. */
    uint8_t d[10] = {0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7, 0, 0};
    uint16_t c = net_checksum(d, 8, 0);
    KT_CHECK(c == (uint16_t)(0x0D22)); /* ~0xDDF2 = 0x220D, stored big-endian */
    memcpy(d + 8, &c, 2);
    KT_CHECK(net_checksum(d, 10, 0) == 0); /* a packet with its checksum sums to zero */
    uint8_t odd[3] = {0x12, 0x34, 0x56};
    uint8_t padded[4] = {0x12, 0x34, 0x56, 0};
    KT_CHECK(net_checksum(odd, 3, 0) == net_checksum(padded, 4, 0));
}

static void t_net_names(void) {
    KT_CHECK(net_host_ok("example.com") && net_host_ok("a-b.c0") && net_host_ok("x") && net_host_ok("93.184.216.34"));
    KT_CHECK(!net_host_ok("") && !net_host_ok("-a.com") && !net_host_ok("a-.com") && !net_host_ok("a..b"));
    KT_CHECK(!net_host_ok("a b.com") && !net_host_ok("exa_mple.com") && !net_host_ok("\xC3\xA9.com") && !net_host_ok(".a"));
    KT_CHECK(!net_host_ok("1.2.3") && !net_host_ok("256.1.1.1") && !net_host_ok("a/b") && !net_host_ok("a:80"));
    char longlabel[80];
    memset(longlabel, 'a', 64);
    strlcpy(longlabel + 64, ".com", 8);
    KT_CHECK(!net_host_ok(longlabel)); /* labels are at most 63 bytes */
    uint32_t ip;
    KT_CHECK(net_parse_ipv4("10.0.2.15", &ip) == 0 && ((uint8_t *)&ip)[0] == 10 && ((uint8_t *)&ip)[3] == 15);
    KT_CHECK(net_parse_ipv4("1.2.3.4.5", &ip) && net_parse_ipv4("01234.1.1.1", &ip) && net_parse_ipv4("1.2.3.", &ip));
}

static uint32_t ip4(int a, int b, int c, int d) {
    uint8_t x[4] = {(uint8_t)a, (uint8_t)b, (uint8_t)c, (uint8_t)d};
    uint32_t v;
    memcpy(&v, x, 4);
    return v;
}

static void t_net_addrs(void) {
    KT_CHECK(net_addr_allowed(ip4(93, 184, 216, 34), 0));
    KT_CHECK(!net_addr_allowed(ip4(127, 0, 0, 1), 1) && !net_addr_allowed(ip4(0, 0, 0, 0), 1));
    KT_CHECK(!net_addr_allowed(ip4(169, 254, 169, 254), 1)); /* cloud metadata services live here */
    KT_CHECK(!net_addr_allowed(ip4(224, 0, 0, 251), 1) && !net_addr_allowed(ip4(255, 255, 255, 255), 1));
    /* LAN addresses: fine when typed, refused when a name resolves to one (DNS rebinding). */
    KT_CHECK(net_addr_allowed(ip4(192, 168, 1, 1), 1) && !net_addr_allowed(ip4(192, 168, 1, 1), 0));
    KT_CHECK(!net_addr_allowed(ip4(10, 1, 2, 3), 0) && !net_addr_allowed(ip4(172, 20, 0, 1), 0));
    KT_CHECK(net_addr_allowed(ip4(172, 32, 0, 1), 0)); /* just outside 172.16/12 */
    /* Real special-use ranges need a typed literal; TEST-NET is allowed by
     * name too because it is only ever a lab. */
    KT_CHECK(net_addr_allowed(ip4(192, 0, 0, 192), 1) && !net_addr_allowed(ip4(192, 0, 0, 192), 0));
    KT_CHECK(net_addr_allowed(ip4(198, 18, 0, 1), 1) && !net_addr_allowed(ip4(198, 18, 0, 1), 0));
    KT_CHECK(net_addr_allowed(ip4(192, 0, 2, 1), 1) && net_addr_allowed(ip4(192, 0, 2, 1), 0));
    KT_CHECK(net_addr_allowed(ip4(198, 51, 100, 1), 1) && net_addr_allowed(ip4(198, 51, 100, 1), 0));
    KT_CHECK(net_addr_allowed(ip4(203, 0, 113, 2), 1) && net_addr_allowed(ip4(203, 0, 113, 2), 0));
}

/* A reply to `q` (a query from dns_build_query) with answers appended. */
static size_t dns_reply(uint8_t *r, const uint8_t *q, int qlen, uint8_t rcode, int ancount, const uint8_t *ans, size_t alen) {
    memcpy(r, q, (size_t)qlen);
    r[2] = 0x81;
    r[3] = (uint8_t)(0x80 | rcode);
    r[7] = (uint8_t)ancount;
    memcpy(r + qlen, ans, alen);
    return (size_t)qlen + alen;
}

static void t_net_dns(void) {
    uint8_t q[128], r[256];
    int n = dns_build_query(q, sizeof q, "www.Example.com", 0xBEEF);
    KT_REQUIRE(n == 12 + 17 + 4 && q[0] == 0xBE && q[1] == 0xEF && q[12] == 3 && q[16] == 7);
    static const uint8_t a_rec[] = {0xC0, 12, 0, 1, 0, 1, 0, 0, 0x0E, 0x10, 0, 4, 93, 184, 216, 34};
    uint32_t ip = 0, ttl = 0;
    size_t len = dns_reply(r, q, n, 0, 1, a_rec, sizeof a_rec);
    KT_CHECK(dns_parse_reply(r, len, 0xBEEF, "www.example.com", &ip, &ttl) == 0 && ip == ip4(93, 184, 216, 34) && ttl == 3600);
    KT_CHECK(dns_parse_reply(r, len, 0xBEEE, "www.example.com", &ip, &ttl) == -1); /* wrong id */
    KT_CHECK(dns_parse_reply(r, len, 0xBEEF, "www.example.org", &ip, &ttl) == -1); /* not our question */
    for (size_t cut = 0; cut < len; cut++) KT_CHECK(dns_parse_reply(r, cut, 0xBEEF, "www.example.com", &ip, &ttl) == -1);
    len = dns_reply(r, q, n, 3, 0, 0, 0);
    KT_CHECK(dns_parse_reply(r, len, 0xBEEF, "www.example.com", &ip, &ttl) == -2); /* no such name */
    /* A CNAME, then the A record for the alias. */
    static const uint8_t cname[] = {0xC0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 6, 3, 'c', 'd', 'n', 0xC0, 16,
                                    0xC0, 45, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 1, 2, 3, 4}; /* 45: the alias, cdn.example.com */
    len = dns_reply(r, q, n, 0, 2, cname, sizeof cname);
    KT_CHECK(dns_parse_reply(r, len, 0xBEEF, "www.example.com", &ip, &ttl) == 0 && ip == ip4(1, 2, 3, 4));
    /* Hostile replies: an answer cut short, a length running off the end, a question that loops. */
    static const uint8_t cut_short[] = {0xC0, 12, 0, 1, 0, 1};
    len = dns_reply(r, q, n, 0, 1, cut_short, sizeof cut_short);
    KT_CHECK(dns_parse_reply(r, len, 0xBEEF, "www.example.com", &ip, &ttl) == -1);
    static const uint8_t longrd[] = {0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0xFF, 0xFF, 1, 2, 3, 4};
    len = dns_reply(r, q, n, 0, 1, longrd, sizeof longrd);
    KT_CHECK(dns_parse_reply(r, len, 0xBEEF, "www.example.com", &ip, &ttl) == -1);
    uint8_t qloop[64];
    memcpy(qloop, q, 12);
    qloop[5] = 1, qloop[2] = 0x81, qloop[3] = 0x80, qloop[12] = 0xC0, qloop[13] = 12; /* the question points at itself */
    KT_CHECK(dns_parse_reply(qloop, 20, 0xBEEF, "www.example.com", &ip, &ttl) == -1);
    KT_CHECK(dns_build_query(q, 20, "www.example.com", 1) < 0); /* doesn't fit */
}

static void t_net_firewall(void) {
    KT_CHECK(fw_host_match("*", "anything.example"));
    KT_CHECK(fw_host_match("example.com", "EXAMPLE.com") && !fw_host_match("example.com", "www.example.com"));
    KT_CHECK(fw_host_match("*.example.com", "www.example.com") && fw_host_match("*.example.com", "a.b.example.com"));
    KT_CHECK(!fw_host_match("*.example.com", "example.com") && !fw_host_match("*.example.com", "badexample.com"));
    KT_CHECK(!fw_host_match("*.example.com", "example.com.evil.net"));
    /* The built-in web policy: the browser on 443 and 80, nothing else. */
    KT_CHECK(fw_builtin("browser", 443) >= 0 && fw_builtin("browser", 80) >= 0);
    KT_CHECK(fw_builtin("browser", 22) < 0 && fw_builtin("browser", 8080) < 0);
    KT_CHECK(fw_builtin("notes", 443) < 0 && fw_builtin("terminal", 80) < 0 && fw_builtin("browse", 443) < 0);
    KT_CHECK(fw_builtin("*", 443) < 0);
    struct fw_rule r;
    KT_CHECK(!fw_builtin_get(0, &r) && r.action == FW_ALLOW && !strcmp(r.host, "*"));
    KT_CHECK(fw_builtin_get(99, &r) == -E_NOENT);
}

static void t_settings(void) {
    char buf[64];
    KT_CHECK(settings_set("test.key", "hello") == 0);
    KT_CHECK(settings_get("test.key", buf, sizeof buf) == 5 && !strcmp(buf, "hello"));
    KT_CHECK(settings_get("no.such.key", buf, sizeof buf) == -E_NOENT);
    KT_CHECK(settings_set("Bad Key", "x") == -E_INVAL);
    char longval[SETTINGS_VALUE_MAX + 2];
    memset(longval, 'x', sizeof longval - 1);
    longval[sizeof longval - 1] = 0;
    KT_CHECK(settings_set("test.key", longval) == -E_INVAL);
    KT_CHECK(settings_set("test.key", "a\nnet.dns=1.2.3.4") == -E_INVAL);
}

static void t_net_config(void) {
    struct net_config old, c, got;
    net_get_config(&old);
    memset(&c, 0, sizeof c);
    c.mode = NET_MODE_STATIC;
    c.ip = 0x0A01A8C0;      /* 192.168.1.10 in memory byte order */
    c.netmask = 0x00FFFFFF; /* 255.255.255.0 */
    c.gateway = 0x0101A8C0; /* 192.168.1.1 */
    c.dns = 0x0201A8C0;     /* 192.168.1.2 */
    int r = net_apply_config(&c);
    if (r == -E_NOTSUP) {
        KT_CHECK(1); /* no network card in this test machine */
        return;
    }
    KT_CHECK(r == 0);
    net_get_config(&got);
    KT_CHECK(got.mode == NET_MODE_STATIC && got.ip == c.ip && got.netmask == c.netmask && got.gateway == c.gateway &&
             got.dns == c.dns);
    c.mode = NET_MODE_DHCP;
    KT_CHECK(net_apply_config(&c) == 0);
    net_get_config(&got);
    KT_CHECK(got.mode == NET_MODE_DHCP);
    /* Put the machine back the way the test found it. */
    KT_CHECK(net_apply_config(&old) == 0);
}

/* ---- USB HID ----------------------------------------------------------- */

static void t_clipboard_labels(void) {
    uint32_t before = current->taint;
    size_t n = 0;
    int label = LABEL_PUBLIC;
    clipboard_clear();
    KT_CHECK(clipboard_store(current, "confidential text", 18, LABEL_CONFIDENTIAL) == 0);
    KT_CHECK(current->taint >= LABEL_CONFIDENTIAL); /* writing taints the writer */
    char out[32];
    KT_CHECK(clipboard_read(current, out, sizeof out, &n, &label) == 0 && n == 18 && label == LABEL_CONFIDENTIAL);
    KT_CHECK(!memcmp(out, "confidential text", 18));
    /* An app that already holds Confidential data cannot label a later copy
     * Public; labels only go up. */
    KT_CHECK(clipboard_store(current, "x", 1, LABEL_PUBLIC) == 0);
    KT_CHECK(!clipboard_info(&n, &label) && n == 1 && label == LABEL_CONFIDENTIAL);
    KT_CHECK(clipboard_store(current, 0, CLIP_MAX + 1, LABEL_PUBLIC) == -E_INVAL);
    clipboard_clear();
    KT_CHECK(!clipboard_info(&n, &label) && n == 0 && label == LABEL_PUBLIC);
    current->taint = before; /* the test raised it deliberately */
}

static void t_notify_rate(void) {
    wm_notifications_clear();
    for (int i = 0; i < 4; i++)
        KT_CHECK(wm_notify_from("ktest", "test", "one") == 0);
    KT_CHECK(wm_notify_from("ktest", "test", "one") == -E_LIMIT); /* burst limit */
    KT_CHECK(wm_notifications_visible() == 4);
    wm_notifications_clear();
    KT_CHECK(wm_notifications_visible() == 0);
}

static void t_usb_hid_keyboard(void) {
    struct usb_hid_kbd st;
    struct input_event ev[6];
    uint8_t r[8] = {0};
    memset(&st, 0, sizeof st);
    r[2] = 0x04; /* 'a' */
    KT_CHECK(usb_hid_keyboard_report(&st, r, ev, ARRAY_LEN(ev)) == 1);
    KT_CHECK(ev[0].type == IN_KEY && ev[0].key == 'a' && ev[0].mods == 0);
    KT_CHECK(usb_hid_keyboard_report(&st, r, ev, ARRAY_LEN(ev)) == 0); /* held, not repeated */
    memset(r, 0, sizeof r);
    usb_hid_keyboard_report(&st, r, ev, ARRAY_LEN(ev)); /* release */
    r[0] = 0x02; /* left shift */
    r[2] = 0x04;
    KT_CHECK(usb_hid_keyboard_report(&st, r, ev, ARRAY_LEN(ev)) == 1);
    KT_CHECK(ev[0].key == 'A' && ev[0].mods == MOD_SHIFT);
    memset(r, 0, sizeof r);
    usb_hid_keyboard_report(&st, r, ev, ARRAY_LEN(ev));
    r[2] = 0x4F; /* Right Arrow */
    KT_CHECK(usb_hid_keyboard_report(&st, r, ev, ARRAY_LEN(ev)) == 1);
    KT_CHECK(ev[0].key == KEY_RIGHT);
    /* Caps Lock toggles letters without generating a key event. */
    memset(&st, 0, sizeof st);
    memset(r, 0, sizeof r);
    r[2] = 0x39;
    KT_CHECK(usb_hid_keyboard_report(&st, r, ev, ARRAY_LEN(ev)) == 0);
    memset(r, 0, sizeof r);
    r[2] = 0x04;
    KT_CHECK(usb_hid_keyboard_report(&st, r, ev, ARRAY_LEN(ev)) == 1 && ev[0].key == 'A');
}

static void t_i2c_hid_parser(void) { KT_CHECK(i2c_hid_selftest() == 0); }

static void t_usb_hid_mouse(void) {
    struct usb_hid_mouse st;
    struct input_event ev;
    uint8_t r[4] = {1, 5, 0xFD, 0xFF}; /* left, +5 x, -3 y, wheel -1 */
    memset(&st, 0, sizeof st);
    KT_CHECK(usb_hid_mouse_report(&st, r, sizeof r, &ev) == 1);
    KT_CHECK(ev.type == IN_MOUSE && ev.buttons == 1 && ev.dx == 5 && ev.dy == -3 && ev.dz == -1);
    uint8_t same[3] = {1, 0, 0};
    KT_CHECK(usb_hid_mouse_report(&st, same, sizeof same, &ev) == 0);
    uint8_t release[3] = {0, 0, 0};
    KT_CHECK(usb_hid_mouse_report(&st, release, sizeof release, &ev) == 1 && ev.buttons == 0);
}

/* ---- Table ------------------------------------------------------------- */

static const struct ktest {
    const char *suite, *name;
    void (*fn)(void);
} tests[] = {
    {"lib", "memmove handles overlap both ways", t_lib_mem},
    {"lib", "string length, compare, bounded copy", t_lib_str},
    {"lib", "snprintf formats numbers and strings", t_lib_fmt},
    {"lib", "snprintf truncates safely", t_lib_fmt_truncate},
    {"memory", "page allocator hands out zeroed pages", t_pmm_alloc},
    {"memory", "contiguous page runs don't overlap", t_pmm_contig},
    {"memory", "heap: aligned, zeroed, separate blocks", t_heap},
    {"memory", "heap: large and impossible requests", t_heap_large},
    {"memory", "page tables map, translate, unmap, free", t_vmm_map},
    {"memory", "every address space shares the kernel", t_vmm_kernel_half},
    {"memory", "kernel is W^X, RAM map is no-execute", t_vmm_wx},
    {"crypto", "known-answer vectors (RFCs)", t_crypto_vectors},
    {"crypto", "AEAD rejects wrong key/nonce/data/tag", t_crypto_aead},
    {"crypto", "constant-time compare and wipe", t_crypto_helpers},
    {"crypto", "random numbers differ every time", t_rng},
    {"files", "format a blank disk", t_files_format},
    {"files", "write and read back across blocks", t_files_rw},
    {"files", "folders: add, find, list, remove", t_files_dirs},
    {"files", "freeing a file returns its space", t_files_free},
    {"files", "contents and names are encrypted", t_files_ciphertext},
    {"files", "a flipped bit on disk is detected", t_files_tamper},
    {"files", "swapped blocks are detected", t_files_swap},
    {"files", "shared blocks don't reuse cached plaintext", t_files_shared_cache},
    {"files", "wrong passphrase refused, data kept", t_files_unlock},
    {"files", "tampered superblock refuses to unlock", t_files_superblock},
    {"files", "a disk with other data is left alone", t_files_foreign},
    {"files", "moves, removes and names keep the tree", t_files_tree},
    {"dlp", "label names and order", t_dlp_names},
    {"dlp", "taint only ever goes up", t_dlp_taint},
    {"dlp", "confidential data can't reach the log", t_dlp_egress},
    {"dlp", "label changes are audited", t_dlp_audit},
    {"sandbox", "untrusted apps get a narrow filter", t_sandbox_untrusted},
    {"sandbox", "children inherit distrust and taint", t_sandbox_inherit},
    {"sandbox", "private, W^X, randomized memory", t_sandbox_layout},
    {"sandbox", "Files gets a window and paths only", t_sandbox_explorer},
    {"sandbox", "process table is consistent", t_sandbox_system},
    {"syscall", "kernel and wild pointers rejected", t_syscall_pointers},
    {"syscall", "checked against the caller's pages", t_syscall_caller},
    {"syscall", "path arguments are bounded", t_syscall_path},
    {"runner", "unsafe or incomplete programs refused", t_runner_elf},
    {"runner", "Linux programs load at a random base", t_runner_pie},
    {"runner", "app packages: checksum, name, size", t_runner_package},
    {"runner", "publisher signatures verify and reject changes", t_runner_signature},
    {"runner", "the app store installs and removes a package", t_runner_appstore},
    {"runner", "memory made on first touch, W^X kept", t_runner_lazy},
    {"runner", "the memory limit holds on first touch", t_runner_quota},
    {"runner", "growing a mapping moves it safely", t_runner_remap},
    {"runner", "page tables count against the limit", t_runner_tables},
    {"net", "internet checksums", t_net_checksum},
    {"net", "host names checked before use", t_net_names},
    {"net", "private addresses refused; special-use DNS policy", t_net_addrs},
    {"net", "DNS replies parsed strictly", t_net_dns},
    {"net", "firewall host patterns and web policy", t_net_firewall},
    {"net", "static address applied and DHCP restored", t_net_config},
    {"usb", "HID boot keyboard maps keys and modifiers", t_usb_hid_keyboard},
    {"usb", "HID boot mouse maps buttons and movement", t_usb_hid_mouse},
    {"usb", "I2C-HID report descriptor parser", t_i2c_hid_parser},
    {"settings", "settings persist and validate keys", t_settings},
    {"wm", "notifications keep history and rate-limit bursts", t_notify_rate},
    {"dlp", "clipboard raises taint and never lowers labels", t_clipboard_labels},
};

int ktest_run(const char *suite, char *out, size_t len, struct ktest_result *res) {
    size_t used = 0;
    res->passed = res->failed = 0;
    if (len) out[0] = 0;
    int any = 0;
    for (unsigned i = 0; i < ARRAY_LEN(tests); i++)
        any |= !suite[0] || !strcmp(suite, tests[i].suite);
    if (!any) return -E_NOENT;
    for (unsigned i = 0; i < ARRAY_LEN(tests); i++) {
        const struct ktest *t = &tests[i];
        if (suite[0] && strcmp(suite, t->suite)) continue;
        failed_expr = 0;
        t->fn();
        char line[200];
        if (!failed_expr) {
            res->passed++;
            snprintf(line, sizeof line, "ok   %-8s %s\n", t->suite, t->name);
        } else {
            res->failed++;
            snprintf(line, sizeof line, "FAIL %-8s %s: %s (ktest.c:%d)\n", t->suite, t->name, failed_expr, failed_line);
            kprintf("[test] %s", line);
        }
        if (len && used < len - 1) used += strlcpy(out + used, line, len - used);
        if (used > len - 1) used = len - 1;
    }
    kprintf("[test] kernel: %u passed, %u failed\n", res->passed, res->failed);
    return 0;
}
