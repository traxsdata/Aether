/* uaccess.h - checked access to the calling process's memory (syscall.c). */
#pragma once
#include <stdint.h>

int user_range_ok(uint64_t addr, uint64_t len, int write);
int copy_in(void *dst, uint64_t usrc, uint64_t len);   /* 0 or -E_FAULT */
int copy_out(uint64_t udst, const void *src, uint64_t len);
/* Copies a path argument in and NUL-terminates it: 0, -E_FAULT or -E_INVAL. */
int copy_path(char *dst, uint64_t usrc, uint64_t len);

/* vfs.c */
int64_t sys_open(uint64_t upath, uint64_t len, uint64_t flags);
int64_t sys_close(uint64_t h);
int64_t sys_read(uint64_t h, uint64_t ubuf, uint64_t len);
int64_t sys_write(uint64_t h, uint64_t ubuf, uint64_t len);
int64_t sys_seek(uint64_t h, int64_t off, uint64_t whence);
int64_t sys_truncate(uint64_t h, uint64_t size);
int64_t sys_stat(uint64_t upath, uint64_t len, uint64_t uout);
int64_t sys_readdir(uint64_t h, uint64_t idx, uint64_t uout);
int64_t sys_mkdir(uint64_t upath, uint64_t len);
int64_t sys_remove(uint64_t upath, uint64_t len);
int64_t sys_rename(uint64_t ufrom, uint64_t flen, uint64_t uto, uint64_t tlen);
int64_t sys_set_label(uint64_t upath, uint64_t len, uint64_t label);
int64_t sys_pick_file(uint64_t ureq, uint64_t uout);
int64_t sys_fs_info(uint64_t uout);
int64_t sys_dlp_log(uint64_t ubuf, uint64_t len);

/* The same with kernel-side paths (linux.c). */
int64_t vfs_open(const char *path, uint64_t flags);
int64_t vfs_mkdir(const char *path);
int64_t vfs_remove(const char *path);
int64_t vfs_rename(const char *from, const char *to);
int vfs_lookup(const char *path, uint32_t *ino); /* -E_NOENT if it doesn't exist */
void vfs_meta_read(uint32_t ino);  /* untrusted apps: taints with the label of what its metadata shows */
int vfs_label_write(uint32_t ino); /* the caller's data is changing the file: raise its label */

/* umem.c */
int64_t sys_mem_map(uint64_t len, uint64_t prot);
int64_t sys_mem_unmap(uint64_t addr, uint64_t len);
int64_t sys_mem_protect(uint64_t addr, uint64_t len, uint64_t prot);

/* Reserved, made-on-first-touch memory (umem.c), for the Linux layer.
 * Lengths are page multiples; addresses page aligned. */
uint64_t umem_alloc_va(uint64_t len);                       /* 0 if out of address space */
void umem_claim_va(uint64_t addr, uint64_t len);            /* a fixed mapping: never hand it out again */
int umem_reserve(uint64_t addr, uint64_t len, uint64_t prot); /* range must be unmapped */
void umem_release(uint64_t addr, uint64_t len);             /* unmap and free */
int umem_protect(uint64_t addr, uint64_t len, uint64_t prot);
int umem_has_shared(uint64_t addr, uint64_t len);           /* window buffers: off limits */
int umem_range_free(uint64_t addr, uint64_t len);
int64_t umem_remap(uint64_t old, uint64_t oldlen, uint64_t newlen, int may_move);
/* A touch of reserved memory: 0 when it's now mapped, -1 not reserved for
 * that access, -2 out of memory (quota). */
int umem_fault(uint64_t addr, int write, int exec);
