/*
 * main.c - NewOS UEFI bootloader (BOOTX64.EFI).
 *
 * 1. Find the framebuffer through the Graphics Output Protocol.
 * 2. Read \kernel.elf from the same FAT partition this loader came from.
 * 3. Copy its PT_LOAD segments into one block of free physical memory.
 * 4. Grab the memory map and exit boot services.
 * 5. Build page tables (identity + higher-half direct map + kernel), load
 *    them, and jump to the kernel's higher-half entry point.
 *
 * After step 4 the firmware is gone: no more console, no more allocation.
 */
#include "efi.h"
#include "sha256.h"
#include "../include/bootinfo.h"
#include "../common/bootcontrol.h"
#include "../common/gpt.h"
#include "../common/rsa_verify.h"
#include "../common/release_key.h"
#include "../common/sysimage.h"

#define KERNEL_PATH u"\\kernel.elf"
#define NETBOOT_PATH u"\\netboot"

struct boot_disk {
    EFI_BLOCK_IO_PROTOCOL *bio;
    uint64_t sectors;
    uint8_t guid[16];
};

static int all_zero(const uint8_t *p, int n);

static EFI_SYSTEM_TABLE *ST;
static EFI_BOOT_SERVICES *BS;

/* The compiler may emit calls to these even in freestanding code. */
void *memset(void *dst, int c, size_t n) {
    uint8_t *d = dst;
    while (n--) *d++ = (uint8_t)c;
    return dst;
}

void *memcpy(void *dst, const void *src, size_t n) {
    uint8_t *d = dst;
    const uint8_t *s = src;
    while (n--) *d++ = *s++;
    return dst;
}

static int guid_eq(const EFI_GUID *a, const EFI_GUID *b) {
    const uint8_t *x = (const uint8_t *)a, *y = (const uint8_t *)b;
    for (int i = 0; i < 16; i++)
        if (x[i] != y[i]) return 0;
    return 1;
}

static void print(const CHAR16 *s) { ST->ConOut->OutputString(ST->ConOut, s); }

static void print_ascii(const char *s) {
    CHAR16 buf[2] = {0, 0};
    for (; *s; s++) {
        buf[0] = (CHAR16)(*s >= 32 && *s < 127 ? *s : '?');
        print(buf);
    }
}

static void print_hex(uint64_t v) {
    CHAR16 buf[19] = u"0x";
    for (int i = 0; i < 16; i++) {
        int nib = (v >> ((15 - i) * 4)) & 0xF;
        buf[2 + i] = (CHAR16)(nib < 10 ? u'0' + nib : u'A' + nib - 10);
    }
    buf[18] = 0;
    print(buf);
}

/* Print an error and stay on screen so the message can be read. */
static EFI_STATUS fail(const CHAR16 *msg, EFI_STATUS status) {
    print(u"\r\n[boot] ERROR: ");
    print(msg);
    print(u" (status ");
    print_hex(status);
    print(u")\r\n");
    for (;;) BS->Stall(1000000);
    return status;
}

/* ---- ELF64 ------------------------------------------------------------- */

typedef struct {
    uint8_t e_ident[16];
    uint16_t e_type, e_machine;
    uint32_t e_version;
    uint64_t e_entry, e_phoff, e_shoff;
    uint32_t e_flags;
    uint16_t e_ehsize, e_phentsize, e_phnum, e_shentsize, e_shnum, e_shstrndx;
} Elf64_Ehdr;

typedef struct {
    uint32_t p_type, p_flags;
    uint64_t p_offset, p_vaddr, p_paddr, p_filesz, p_memsz, p_align;
} Elf64_Phdr;

#define PT_LOAD 1
#define EM_X86_64 62

static EFI_STATUS read_kernel(EFI_HANDLE image, void **out, UINTN *out_size) {
    EFI_GUID lip_guid = EFI_LOADED_IMAGE_PROTOCOL_GUID;
    EFI_GUID sfs_guid = EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID;
    EFI_LOADED_IMAGE_PROTOCOL *lip;
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL *sfs;
    EFI_FILE_PROTOCOL *root, *file;
    EFI_STATUS s;

    if (EFI_ERROR(s = BS->HandleProtocol(image, &lip_guid, (void **)&lip)))
        return fail(u"no LoadedImage protocol", s);
    if (EFI_ERROR(s = BS->HandleProtocol(lip->DeviceHandle, &sfs_guid, (void **)&sfs)))
        return fail(u"boot device has no file system", s);
    if (EFI_ERROR(s = sfs->OpenVolume(sfs, &root)))
        return fail(u"cannot open boot volume", s);
    if (EFI_ERROR(s = root->Open(root, &file, KERNEL_PATH, EFI_FILE_MODE_READ, 0)))
        return fail(u"cannot open \\kernel.elf", s);

    /* Seeking to the end is the simplest way to learn the size. */
    uint64_t size;
    file->SetPosition(file, ~0ull);
    file->GetPosition(file, &size);
    file->SetPosition(file, 0);

    void *buf;
    if (EFI_ERROR(s = BS->AllocatePool(EfiLoaderData, size, &buf)))
        return fail(u"out of memory reading kernel", s);
    UINTN n = size;
    if (EFI_ERROR(s = file->Read(file, &n, buf)) || n != size)
        return fail(u"short read of kernel", s);
    file->Close(file);
    root->Close(root);

    *out = buf;
    *out_size = size;
    return EFI_SUCCESS;
}

/* EFI_FILE_INFO, as returned when reading a directory. */
struct file_info {
    uint64_t size, file_size, physical_size;
    uint8_t times[48];
    uint64_t attribute;
    CHAR16 name[256];
};
#define EFI_FILE_DIRECTORY 0x10

/* Read every \apps\*.app package into memory for the kernel. A missing
 * folder just means no extra apps. */
static void read_modules(EFI_HANDLE image, struct bootinfo *bi) {
    EFI_GUID lip_guid = EFI_LOADED_IMAGE_PROTOCOL_GUID;
    EFI_GUID sfs_guid = EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID;
    EFI_LOADED_IMAGE_PROTOCOL *lip;
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL *sfs;
    EFI_FILE_PROTOCOL *root, *dir, *file;
    if (EFI_ERROR(BS->HandleProtocol(image, &lip_guid, (void **)&lip)) ||
        EFI_ERROR(BS->HandleProtocol(lip->DeviceHandle, &sfs_guid, (void **)&sfs)) ||
        EFI_ERROR(sfs->OpenVolume(sfs, &root)))
        return;
    if (EFI_ERROR(root->Open(root, &dir, u"\\apps", EFI_FILE_MODE_READ, 0))) return;
    static struct file_info fi;
    for (;;) {
        UINTN n = sizeof fi;
        if (EFI_ERROR(dir->Read(dir, &n, &fi)) || n == 0) break;
        if (fi.attribute & EFI_FILE_DIRECTORY || bi->nmodules >= BOOT_MODULES_MAX) continue;
        /* name.app -> "name" */
        int len = 0;
        while (fi.name[len]) len++;
        if (len < 5 || len - 4 >= 32 || fi.name[len - 4] != '.' || fi.name[len - 3] != 'a' ||
            fi.name[len - 2] != 'p' || fi.name[len - 1] != 'p')
            continue;
        if (EFI_ERROR(dir->Open(dir, &file, fi.name, EFI_FILE_MODE_READ, 0))) continue;
        EFI_PHYSICAL_ADDRESS buf;
        UINTN size = fi.file_size;
        if (EFI_ERROR(BS->AllocatePages(AllocateAnyPages, EfiLoaderData, (size + 4095) / 4096, &buf)) ||
            EFI_ERROR(file->Read(file, &size, (void *)buf)) || size != fi.file_size) {
            file->Close(file);
            continue;
        }
        file->Close(file);
        struct boot_module *m = &bi->modules[bi->nmodules++];
        for (int i = 0; i < len - 4; i++) m->name[i] = (char)fi.name[i];
        m->phys = buf;
        m->size = size;
        print(u"[boot] app ");
        print(fi.name);
        print(u"\r\n");
    }
    dir->Close(dir);
    root->Close(root);
}

/*
 * Apps on the CD itself. A UEFI CD boot only sees the small El Torito boot
 * image (at most 32 MiB), so big app packages live in the ISO 9660 file
 * system, in /apps, and are read straight from the disk's sectors. This
 * works the same when the ISO is written to a USB stick.
 */
static int memcmp(const void *a, const void *b, UINTN n) {
    const uint8_t *x = a, *y = b;
    for (UINTN i = 0; i < n; i++)
        if (x[i] != y[i]) return x[i] - y[i];
    return 0;
}

static EFI_BLOCK_IO_PROTOCOL *iso_dev;
static uint8_t iso_sector[2048];

/* Read len bytes at byte offset off (both multiples of 2048). */
static int iso_read(uint64_t off, uint64_t len, void *buf) {
    uint32_t bs = iso_dev->Media->BlockSize;
    while (len) {
        UINTN n = len > (1u << 20) ? (1u << 20) : len; /* 1 MiB at a time */
        UINTN rd = (n + bs - 1) / bs * bs;
        if (rd != n) return -1;
        if (EFI_ERROR(iso_dev->ReadBlocks(iso_dev, iso_dev->Media->MediaId, off / bs, rd, buf))) return -1;
        off += n;
        len -= n;
        buf = (uint8_t *)buf + n;
    }
    return 0;
}

static uint32_t le32(const uint8_t *p) { return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24; }

/* Find the whole-device block I/O handle carrying our ISO 9660 volume. */
static int iso_find(void) {
    EFI_GUID bio_guid = EFI_BLOCK_IO_PROTOCOL_GUID;
    EFI_HANDLE *handles;
    UINTN count;
    if (EFI_ERROR(BS->LocateHandleBuffer(EFI_LOCATE_BY_PROTOCOL, &bio_guid, 0, &count, &handles))) return 0;
    for (UINTN i = 0; i < count; i++) {
        EFI_BLOCK_IO_PROTOCOL *bio;
        if (EFI_ERROR(BS->HandleProtocol(handles[i], &bio_guid, (void **)&bio))) continue;
        if (!bio->Media->MediaPresent || bio->Media->LogicalPartition || !bio->Media->BlockSize ||
            2048 % bio->Media->BlockSize)
            continue;
        iso_dev = bio;
        /* Primary volume descriptor, volume id "AETHER". */
        if (!iso_read(16 * 2048, 2048, iso_sector) && iso_sector[0] == 1 && !memcmp(iso_sector + 1, "CD001", 5) &&
            !memcmp(iso_sector + 40, "AETHER ", 7))
            break;
        iso_dev = 0;
    }
    BS->FreePool(handles);
    return iso_dev != 0;
}

static void read_iso_apps(struct bootinfo *bi) {
    if (!iso_find()) return;
    /* The root directory record is at byte 156 of the volume descriptor. */
    uint64_t dir_lba = le32(iso_sector + 156 + 2), dir_size = le32(iso_sector + 156 + 10);
    uint64_t apps_lba = 0, apps_size = 0;
    static uint8_t dirbuf[2048];
    /* Find /APPS in the root, then load every package in it. */
    for (int pass = 0; pass < 2; pass++) {
        for (uint64_t o = 0; o < dir_size; o += 2048) {
            if (iso_read((dir_lba * 2048) + o, 2048, dirbuf)) return;
            for (unsigned r = 0; r < 2048 && dirbuf[r];) {
                const uint8_t *rec = dirbuf + r;
                uint32_t lba = le32(rec + 2), size = le32(rec + 10);
                int is_dir = rec[25] & 2, nlen = rec[32];
                const char *name = (const char *)rec + 33;
                if (pass == 0 && is_dir && nlen == 4 && (name[0] | 32) == 'a' && (name[1] | 32) == 'p' &&
                    (name[2] | 32) == 'p' && (name[3] | 32) == 's') {
                    apps_lba = lba;
                    apps_size = size;
                } else if (pass == 1 && !is_dir && size > 512 && bi->nmodules < BOOT_MODULES_MAX) {
                    /* A package is recognised by its header, not its name. */
                    EFI_PHYSICAL_ADDRESS buf;
                    uint64_t padded = (size + 2047) / 2048 * 2048;
                    if (EFI_ERROR(BS->AllocatePages(AllocateAnyPages, EfiLoaderData, (padded + 4095) / 4096, &buf)))
                        return;
                    uint8_t *p = (uint8_t *)buf;
                    if (iso_read((uint64_t)lba * 2048, padded, p) || memcmp(p, "AETHAPP1", 8)) {
                        BS->FreePages(buf, (padded + 4095) / 4096);
                    } else {
                        struct boot_module *m = &bi->modules[bi->nmodules++];
                        for (int i = 0; i < 31 && p[16 + i]; i++) m->name[i] = (char)p[16 + i];
                        m->phys = buf;
                        m->size = size;
                        print(u"[boot] app from CD: ");
                        CHAR16 wname[32];
                        int i = 0;
                        for (; i < 31 && m->name[i]; i++) wname[i] = (CHAR16)m->name[i];
                        wname[i] = 0;
                        print(wname);
                        print(u"\r\n");
                    }
                }
                r += rec[0];
            }
        }
        if (pass == 0) {
            if (!apps_lba) return;
            dir_lba = apps_lba;
            dir_size = apps_size;
        }
    }
}

static Elf64_Phdr *kernel_phdrs;
static int kernel_phnum;

static EFI_STATUS elf_problem(const CHAR16 *msg) {
    print(u"\r\n[boot] ");
    print(msg);
    print(u"\r\n");
    return EFI_LOAD_ERROR;
}

static EFI_STATUS load_elf(void *image, UINTN size, struct bootinfo *bi, uint64_t *entry) {
    Elf64_Ehdr *eh = image;
    if (size < sizeof *eh || eh->e_ident[0] != 0x7F || eh->e_ident[1] != 'E' ||
        eh->e_ident[2] != 'L' || eh->e_ident[3] != 'F' || eh->e_ident[4] != 2 ||
        eh->e_machine != EM_X86_64)
        return elf_problem(u"the kernel is not an x86_64 ELF64 file");

    if (eh->e_phentsize != sizeof(Elf64_Phdr) || eh->e_phnum == 0 ||
        eh->e_phoff > size || (uint64_t)eh->e_phnum * sizeof(Elf64_Phdr) > size - eh->e_phoff)
        return elf_problem(u"the kernel has a bad program header table");
    kernel_phdrs = (Elf64_Phdr *)((uint8_t *)image + eh->e_phoff);
    kernel_phnum = eh->e_phnum;

    /* The kernel's virtual span, as linked. */
    uint64_t vlo = ~0ull, vhi = 0;
    for (int i = 0; i < kernel_phnum; i++) {
        Elf64_Phdr *ph = &kernel_phdrs[i];
        if (ph->p_type != PT_LOAD || ph->p_memsz == 0) continue;
        if (ph->p_filesz > ph->p_memsz || ph->p_offset > size || ph->p_filesz > size - ph->p_offset ||
            ph->p_vaddr + ph->p_memsz < ph->p_vaddr)
            return elf_problem(u"the kernel has a bad loadable segment");
        if ((ph->p_vaddr & ~0xFFFull) < vlo) vlo = ph->p_vaddr & ~0xFFFull;
        if (ph->p_vaddr + ph->p_memsz > vhi) vhi = ph->p_vaddr + ph->p_memsz;
    }
    vhi = (vhi + 0xFFF) & ~0xFFFull;
    if (vlo >= vhi) return elf_problem(u"kernel.elf has nothing to load");

    /* Put it wherever the firmware has room: the page tables decide where it
     * appears, so the physical address doesn't matter. */
    EFI_PHYSICAL_ADDRESS base;
    EFI_STATUS s = BS->AllocatePages(AllocateAnyPages, EfiLoaderData, (vhi - vlo) / 4096, &base);
    if (EFI_ERROR(s)) return elf_problem(u"not enough memory for the kernel");
    memset((void *)base, 0, vhi - vlo);
    for (int i = 0; i < kernel_phnum; i++) {
        Elf64_Phdr *ph = &kernel_phdrs[i];
        if (ph->p_type != PT_LOAD || ph->p_memsz == 0) continue;
        memcpy((void *)(base + (ph->p_vaddr - vlo)), (uint8_t *)image + ph->p_offset, ph->p_filesz);
    }
    bi->kernel_start = base;
    bi->kernel_end = base + (vhi - vlo);
    bi->kernel_virt = vlo;
    *entry = eh->e_entry;
    return EFI_SUCCESS;
}


/* ---- Installed A/B system partitions ----------------------------------- */

static int block_read(void *ctx, uint64_t lba, uint32_t count, void *buf) {
    struct boot_disk *d = ctx;
    if (!count) return 0;
    if (lba > d->sectors || count > d->sectors - lba) return -1;
    if (EFI_ERROR(d->bio->ReadBlocks(d->bio, d->bio->Media->MediaId, lba, (UINTN)count * 512, buf))) return -1;
    return 0;
}

static int block_write(void *ctx, uint64_t lba, uint32_t count, const void *buf) {
    struct boot_disk *d = ctx;
    if (!count) return 0;
    if (!d->bio->WriteBlocks || lba > d->sectors || count > d->sectors - lba) return -1;
    if (EFI_ERROR(d->bio->WriteBlocks(d->bio, d->bio->Media->MediaId, lba, (UINTN)count * 512, buf))) return -1;
    return d->bio->FlushBlocks && EFI_ERROR(d->bio->FlushBlocks(d->bio)) ? -1 : 0;
}

/* Whether a file exists on the volume this loader came from. */
static int file_exists(EFI_HANDLE image, const CHAR16 *path) {
    EFI_GUID lip_guid = EFI_LOADED_IMAGE_PROTOCOL_GUID;
    EFI_GUID sfs_guid = EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID;
    EFI_LOADED_IMAGE_PROTOCOL *lip;
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL *sfs;
    EFI_FILE_PROTOCOL *root, *file;
    if (EFI_ERROR(BS->HandleProtocol(image, &lip_guid, (void **)&lip)) ||
        EFI_ERROR(BS->HandleProtocol(lip->DeviceHandle, &sfs_guid, (void **)&sfs)) ||
        EFI_ERROR(sfs->OpenVolume(sfs, &root)))
        return 0;
    EFI_STATUS s = root->Open(root, &file, path, EFI_FILE_MODE_READ, 0);
    if (!EFI_ERROR(s)) file->Close(file);
    root->Close(root);
    return !EFI_ERROR(s);
}

/* The partition this loader itself came from, from its device path. */
static int loader_partition_guid(EFI_HANDLE image, uint8_t sig[16]) {
    EFI_GUID lip_guid = EFI_LOADED_IMAGE_PROTOCOL_GUID;
    EFI_GUID dp_guid = EFI_DEVICE_PATH_PROTOCOL_GUID;
    EFI_LOADED_IMAGE_PROTOCOL *lip;
    uint8_t *dp;
    if (EFI_ERROR(BS->HandleProtocol(image, &lip_guid, (void **)&lip)) ||
        EFI_ERROR(BS->HandleProtocol(lip->DeviceHandle, &dp_guid, (void **)&dp)))
        return 0;
    for (int off = 0; off < 4096;) {
        uint8_t type = dp[off], subtype = dp[off + 1];
        uint16_t len = (uint16_t)(dp[off + 2] | dp[off + 3] << 8);
        if (len < 4 || off + len > (1 << 20)) return 0;
        if (type == 4 && subtype == 1 && len >= 42) {
            for (int i = 0; i < 16; i++) sig[i] = dp[off + 24 + i];
            return 1;
        }
        if (type == 0x7F && subtype == 0xFF) break;
        off += len;
    }
    return 0;
}

static int parts_have_guid(const struct gpt_partition *parts, int n, const uint8_t guid[16]) {
    int any = 0;
    for (int i = 0; i < 16; i++) any |= guid[i];
    if (!any) return 0;
    for (int i = 0; i < n; i++)
        if (!memcmp(parts[i].guid, guid, 16)) return 1;
    return 0;
}

/* Find the whole disk carrying a valid GPT for this install. Fixed slot
 * indexes A=0/B=1 with presence flags; when the loader's own partition start
 * is known, only the disk containing it is accepted. */
static int find_install_disk(struct boot_disk *out, uint8_t out_guid[16], struct gpt_partition *bc,
                             struct gpt_partition sys[2], int have[2], const uint8_t loader_sig[16]) {
    EFI_GUID bio_guid = EFI_BLOCK_IO_PROTOCOL_GUID;
    EFI_HANDLE *handles;
    UINTN count;
    if (EFI_ERROR(BS->LocateHandleBuffer(EFI_LOCATE_BY_PROTOCOL, &bio_guid, 0, &count, &handles))) return 0;
    int found = 0;
    int want_match = !all_zero(loader_sig, 16);
    if (!want_match) {
        BS->FreePool(handles);
        return 0;
    }
    for (UINTN i = 0; i < count; i++) {
        EFI_BLOCK_IO_PROTOCOL *bio;
        if (EFI_ERROR(BS->HandleProtocol(handles[i], &bio_guid, (void **)&bio))) continue;
        if (!bio->Media->MediaPresent || bio->Media->LogicalPartition || bio->Media->BlockSize != 512) continue;
        struct boot_disk d = {bio, bio->Media->LastBlock + 1, {0}};
        struct gpt_io io = {&d, d.sectors, block_read, block_write, {0}, {0}};
        static struct gpt_partition parts[GPT_ENTRIES];
        int n = gpt_load(&io, parts, GPT_ENTRIES);
        if (n <= 0) continue;
        struct gpt_partition b, a, bslot;
        if (!gpt_find_type(parts, n, gpt_type_aether_boot, 0, &b)) continue;
        if (b.last_lba - b.first_lba + 1 < BOOTCONTROL_COPIES) continue;
        int ha = gpt_find_type(parts, n, gpt_type_aether_system_a, 0, &a) ? 1 : 0;
        int hb = gpt_find_type(parts, n, gpt_type_aether_system_b, 0, &bslot) ? 1 : 0;
        if (!ha && !hb) continue;
        int match = parts_have_guid(parts, n, loader_sig);
        for (int k = 0; k < 16; k++) d.guid[k] = io.loaded_disk_guid[k];
        if (match) {
            *out = d;
            for (int k = 0; k < 16; k++) out_guid[k] = io.loaded_disk_guid[k];
            *bc = b;
            memset(sys, 0, sizeof sys[0] * 2);
            have[0] = have[1] = 0;
            if (ha) sys[0] = a, have[0] = 1;
            if (hb) sys[1] = bslot, have[1] = 1;
            found = 1;
            break;
        }
    }
    BS->FreePool(handles);
    return found;
}

static int bootcontrol_load(struct boot_disk *d, const struct gpt_partition *part, struct bootcontrol *out) {
    uint8_t raw[BOOTCONTROL_COPIES * 512];
    if (block_read(d, part->first_lba, BOOTCONTROL_COPIES, raw)) return -1;
    int best = -1;
    for (int i = 0; i < BOOTCONTROL_COPIES; i++) {
        struct bootcontrol *c = (struct bootcontrol *)(raw + i * 512);
        if (!bootcontrol_valid(c)) continue;
        if (best < 0 || c->generation > ((struct bootcontrol *)(raw + best * 512))->generation) best = i;
    }
    if (best < 0) return -1;
    *out = *(struct bootcontrol *)(raw + best * 512);
    return 0;
}

static int bootcontrol_store(struct boot_disk *d, const struct gpt_partition *part, struct bootcontrol *bc) {
    uint8_t raw[BOOTCONTROL_COPIES * 512];
    bc->generation++;
    bootcontrol_finish(bc);
    for (int i = 0; i < BOOTCONTROL_COPIES; i++) memcpy(raw + i * 512, bc, sizeof *bc);
    return block_write(d, part->first_lba, BOOTCONTROL_COPIES, raw);
}

static int image_range_ok(const struct gpt_partition *p, uint64_t offset, uint64_t size, uint64_t *part_bytes) {
    if (p->last_lba < p->first_lba) return 0;
    uint64_t bytes = (p->last_lba - p->first_lba + 1) * 512;
    if (part_bytes) *part_bytes = bytes;
    if (offset > bytes || size > bytes - offset || offset % 512) return 0;
    return 1;
}

/* Read a range of a system partition into loader pages. */
static int read_part(struct boot_disk *d, const struct gpt_partition *p, uint64_t offset, uint64_t size,
                     void **out_phys) {
    uint64_t part_bytes;
    if (!image_range_ok(p, offset, size, &part_bytes) || !size || size > (1ull << 30)) return -1;
    uint64_t pages = (size + 4095) / 4096;
    EFI_PHYSICAL_ADDRESS phys;
    if (EFI_ERROR(BS->AllocatePages(AllocateAnyPages, EfiLoaderData, pages, &phys))) return -1;
    uint64_t sectors = (size + 511) / 512;
    if (offset / 512 + sectors > (p->last_lba - p->first_lba + 1) ||
        block_read(d, p->first_lba + offset / 512, (uint32_t)sectors, (void *)phys)) {
        BS->FreePages(phys, pages);
        return -1;
    }
    *out_phys = (void *)phys;
    return 0;
}

/* The canonical digest covers the header (with image_sha256 and signature
 * zeroed) and then the payload. The same digest is signed and recorded in
 * boot control. */
static void image_digest(const uint8_t *image, const struct sysimage_header *h, uint8_t out[32]) {
    static uint8_t hdr[SYSIMAGE_HEADER_SIZE];
    memcpy(hdr, image, sizeof hdr);
    memset(hdr + offsetof(struct sysimage_header, image_sha256), 0, 32);
    memset(hdr + offsetof(struct sysimage_header, signature), 0, SYSIMAGE_SIGNATURE_SIZE);
    struct boot_sha256 c;
    boot_sha256_init(&c);
    boot_sha256_update(&c, hdr, sizeof hdr);
    boot_sha256_update(&c, image + SYSIMAGE_HEADER_SIZE, h->image_size - SYSIMAGE_HEADER_SIZE);
    boot_sha256_final(&c, out);
}

static int all_zero(const uint8_t *p, int n) {
    for (int i = 0; i < n; i++)
        if (p[i]) return 0;
    return 1;
}

static int boot_system_part(struct boot_disk *d, const struct gpt_partition *p, const uint8_t expected[32],
                            uint32_t min_version, struct bootinfo *bi, uint64_t *entry) {
    static struct sysimage_header h;
    if (all_zero(expected, 32)) {
        print(u"[boot] slot has no recorded image hash\r\n");
        return -1;
    }
    if (block_read(d, p->first_lba, SYSIMAGE_HEADER_SIZE / 512, &h)) return -1;
    const char *why = sysimage_check(&h, (p->last_lba - p->first_lba + 1) * 512);
    if (why) {
        print(u"[boot] system image refused: ");
        print_ascii(why);
        print(u"\r\n");
        return -1;
    }
    /* Read the whole image once and use that same buffer for hashing and
     * loading. Re-reading after the check would be a TOCTOU hole. */
    void *image_phys;
    if (read_part(d, p, 0, h.image_size, &image_phys)) return -1;
    const uint8_t *image = image_phys;
    const struct sysimage_header *hi = (const struct sysimage_header *)image;
    why = sysimage_check(hi, (p->last_lba - p->first_lba + 1) * 512);
    if (why) return -1;
    if (hi->image_size != h.image_size) {
        print(u"[boot] system image size changed between reads\r\n");
        return -1;
    }
    if (hi->version_code < min_version) {
        print(u"[boot] system image is older than the boot-control minimum\r\n");
        return -1;
    }
    uint8_t digest[32];
    image_digest(image, hi, digest);
    for (int i = 0; i < 32; i++)
        if (digest[i] != hi->image_sha256[i]) {
            print(u"[boot] system image digest mismatch\r\n");
            return -1;
        }
    if (!rsa_verify_sha256(digest, hi->signature, aether_release_modulus)) {
        print(u"[boot] system image signature is not from the release key\r\n");
        return -1;
    }
    for (int i = 0; i < 32; i++)
        if (digest[i] != expected[i]) {
            print(u"[boot] system image does not match boot control\r\n");
            return -1;
        }
    if (EFI_ERROR(load_elf((void *)(image + hi->kernel_offset), hi->kernel_size, bi, entry))) return -1;
    for (uint32_t i = 0; i < hi->app_count && i < BOOT_MODULES_MAX; i++) {
        struct boot_module *m = &bi->modules[bi->nmodules++];
        for (int k = 0; k < SYSIMAGE_NAME_MAX && hi->apps[i].name[k]; k++) m->name[k] = hi->apps[i].name[k];
        m->phys = (uint64_t)image_phys + hi->apps[i].offset;
        m->size = hi->apps[i].size;
        print(u"[boot] system app ");
        print_ascii(m->name);
        print(u"\r\n");
    }
    return 0;
}

/* Try the installed OS. Returns 1 when it booted, 0 when no install exists,
 * -1 when an install exists but neither slot could be loaded. */
static int boot_installed(EFI_HANDLE image, struct bootinfo *bi, uint64_t *entry) {
    struct boot_disk d;
    struct gpt_partition bcpart, sys[2];
    int have[2] = {0, 0};
    uint8_t guid[16];
    uint8_t loader_sig[16] = {0};
    if (!loader_partition_guid(image, loader_sig)) {
        print(u"[boot] the loader's ESP device path has no GPT partition GUID\r\n");
        return -1;
    }
    if (!find_install_disk(&d, guid, &bcpart, sys, have, loader_sig)) {
        print(u"[boot] no installed disk matches the loader's ESP partition GUID\r\n");
        return -1;
    }
    struct bootcontrol bc;
    memset(&bc, 0, sizeof bc);
    if (bootcontrol_load(&d, &bcpart, &bc)) {
        print(u"[boot] boot control is unreadable; refusing to guess a slot\r\n");
        return -1;
    }
    if (bc.pending != BOOTCONTROL_NONE) {
        if (bc.attempts + 1 > BOOTCONTROL_MAX_ATTEMPTS) {
            print(u"[boot] the pending system did not start twice; falling back\r\n");
            bc.active = bc.good;
            bc.pending = BOOTCONTROL_NONE;
            bc.attempts = 0;
        } else {
            bc.attempts++;
        }
        if (bootcontrol_store(&d, &bcpart, &bc)) {
            print(u"[boot] couldn't record the boot attempt; booting the last good slot\r\n");
            bc.active = bc.good;
            bc.pending = BOOTCONTROL_NONE;
            bc.attempts = 0;
        }
    }
    int order[2] = {(int)bc.active, 1 - (int)bc.active};
    for (int k = 0; k < 2; k++) {
        int slot = order[k];
        if (slot < 0 || slot > 1 || !have[slot]) continue;
        bi->nmodules = 0;
        if (!boot_system_part(&d, &sys[slot], bc.image_sha256[slot], bc.min_version, bi, entry)) {
            bi->boot_flags |= BOOT_FLAG_INSTALLED;
            bi->boot_slot = (uint32_t)slot;
            for (int i = 0; i < 16; i++) bi->boot_disk_guid[i] = guid[i];
            return 1;
        }
    }
    print(u"[boot] no installed system image could be loaded\r\n");
    return -1;
}

/* ---- Firmware boot entry ------------------------------------------------- */

static int device_path_size(const uint8_t *p, UINTN *out) {
    UINTN n = 0;
    for (int i = 0; i < 4096; i++) { /* a device path node is at most 64 KiB; cap the walk */
        uint8_t type = p[n], subtype = p[n + 1];
        uint16_t len = (uint16_t)(p[n + 2] | p[n + 3] << 8);
        if (len < 4 || n + len > (1u << 20)) return 0;
        n += len;
        if (type == 0x7F && subtype == 0xFF) {
            *out = n;
            return 1;
        }
    }
    return 0;
}

/* Make the installed disk appear in the firmware's boot menu. Firmware often
 * falls back to \EFI\BOOT\BOOTX64.EFI on a new disk, but not every laptop
 * does. The entry points at the loader's own device path, is written once,
 * and is harmless if the firmware already has one. */
static void install_boot_entry(EFI_HANDLE image) {
    EFI_RUNTIME_SERVICES *rt = (EFI_RUNTIME_SERVICES *)ST->RuntimeServices;
    if (!rt || !rt->GetVariable || !rt->SetVariable) return;
    EFI_GUID lip_guid = EFI_LOADED_IMAGE_PROTOCOL_GUID;
    EFI_GUID dp_guid = EFI_DEVICE_PATH_PROTOCOL_GUID;
    EFI_GUID global = EFI_GLOBAL_VARIABLE_GUID;
    EFI_LOADED_IMAGE_PROTOCOL *lip;
    if (EFI_ERROR(BS->HandleProtocol(image, &lip_guid, (void **)&lip))) return;
    uint8_t *path;
    if (EFI_ERROR(BS->HandleProtocol(lip->DeviceHandle, &dp_guid, (void **)&path))) return;
    UINTN path_size;
    if (!device_path_size(path, &path_size) || path_size > 0xFFFF) return;

    CHAR16 name[9] = u"Boot0000";
    UINTN best = ~0ull;
    for (unsigned i = 0; i < 0x100; i++) {
        name[4] = (CHAR16)(u'0' + (i >> 12 & 0xF));
        name[5] = (CHAR16)(u'0' + (i >> 8 & 0xF));
        name[6] = (CHAR16)(u'0' + (i >> 4 & 0xF));
        name[7] = (CHAR16)(u'0' + (i & 0xF));
        UINTN size = 0;
        EFI_STATUS st = rt->GetVariable(name, &global, 0, &size, 0);
        if (st == EFI_BUFFER_TOO_SMALL && size >= 6) {
            void *data;
            if (EFI_ERROR(BS->AllocatePool(EfiLoaderData, size, &data))) return;
            st = rt->GetVariable(name, &global, 0, &size, data);
            if (!EFI_ERROR(st)) {
                uint16_t flen = (uint16_t)(((uint8_t *)data)[4] | ((uint8_t *)data)[5] << 8);
                const uint8_t *p = (const uint8_t *)data + 6;
                UINTN dlen = size >= 6 ? (size - 6) : 0;
                /* Description length is unknown here; compare the path at the end. */
                if (dlen >= flen && flen == path_size && !memcmp(p + dlen - flen, path, flen)) {
                    BS->FreePool(data);
                    return; /* already ours */
                }
            }
            BS->FreePool(data);
        } else if (st == EFI_NOT_FOUND && best == ~0ull) {
            best = i;
        }
    }
    if (best == ~0ull) return;
    name[4] = (CHAR16)(u'0' + (best >> 12 & 0xF));
    name[5] = (CHAR16)(u'0' + (best >> 8 & 0xF));
    name[6] = (CHAR16)(u'0' + (best >> 4 & 0xF));
    name[7] = (CHAR16)(u'0' + (best & 0xF));

    static const CHAR16 desc[] = u"aether";
    UINTN desc_size = (sizeof desc); /* includes the final NUL */
    UINTN data_size = 6 + desc_size + path_size;
    uint8_t *data;
    if (EFI_ERROR(BS->AllocatePool(EfiLoaderData, data_size, (void **)&data))) return;
    data[0] = 1, data[1] = 0, data[2] = 0, data[3] = 0; /* LoadOptionActive */
    data[4] = (uint8_t)path_size, data[5] = (uint8_t)(path_size >> 8);
    memcpy(data + 6, desc, desc_size);
    memcpy(data + 6 + desc_size, path, path_size);
    EFI_STATUS st = rt->SetVariable(name, &global, EFI_VARIABLE_NON_VOLATILE | EFI_VARIABLE_BOOTSERVICE_ACCESS |
                                              EFI_VARIABLE_RUNTIME_ACCESS,
                                    data_size, data);
    BS->FreePool(data);
    if (EFI_ERROR(st)) return;

    /* Append it to BootOrder if it isn't there already. */
    CHAR16 order_name[10] = u"BootOrder";
    UINTN order_size = 0;
    st = rt->GetVariable(order_name, &global, 0, &order_size, 0);
    uint8_t *order = 0;
    if (st == EFI_BUFFER_TOO_SMALL && order_size) {
        if (EFI_ERROR(BS->AllocatePool(EfiLoaderData, order_size + 2, (void **)&order))) return;
        UINTN got = order_size;
        if (EFI_ERROR(rt->GetVariable(order_name, &global, 0, &got, order))) {
            BS->FreePool(order);
            return;
        }
        order_size = got;
    } else {
        if (EFI_ERROR(BS->AllocatePool(EfiLoaderData, 2, (void **)&order))) return;
        order_size = 0;
    }
    for (UINTN i = 0; i + 1 < order_size; i += 2)
        if (order[i] == (uint8_t)best && order[i + 1] == 0) {
            BS->FreePool(order);
            return;
        }
    order[order_size] = (uint8_t)best;
    order[order_size + 1] = 0;
    rt->SetVariable(order_name, &global, EFI_VARIABLE_NON_VOLATILE | EFI_VARIABLE_BOOTSERVICE_ACCESS |
                                              EFI_VARIABLE_RUNTIME_ACCESS,
                    order_size + 2, order);
    BS->FreePool(order);
}

static EFI_STATUS setup_framebuffer(struct bootinfo *bi) {
    EFI_GUID gop_guid = EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID;
    EFI_GRAPHICS_OUTPUT_PROTOCOL *gop;
    EFI_STATUS s = BS->LocateProtocol(&gop_guid, 0, (void **)&gop);
    if (EFI_ERROR(s)) return fail(u"no Graphics Output Protocol (framebuffer)", s);

    EFI_GRAPHICS_OUTPUT_MODE_INFORMATION *info = gop->Mode->Info;
    if (info->PixelFormat != PixelRedGreenBlueReserved8BitPerColor &&
        info->PixelFormat != PixelBlueGreenRedReserved8BitPerColor)
        return fail(u"unsupported framebuffer pixel format", EFI_UNSUPPORTED);

    bi->fb.base = gop->Mode->FrameBufferBase;
    bi->fb.size = gop->Mode->FrameBufferSize;
    bi->fb.width = info->HorizontalResolution;
    bi->fb.height = info->VerticalResolution;
    bi->fb.pitch = info->PixelsPerScanLine;
    bi->fb.format = info->PixelFormat == PixelRedGreenBlueReserved8BitPerColor ? FB_FORMAT_RGBX
                                                                                 : FB_FORMAT_BGRX;
    return EFI_SUCCESS;
}

static void find_acpi(struct bootinfo *bi) {
    EFI_GUID acpi20 = EFI_ACPI_20_TABLE_GUID;
    for (UINTN i = 0; i < ST->NumberOfTableEntries; i++)
        if (guid_eq(&ST->ConfigurationTable[i].VendorGuid, &acpi20))
            bi->acpi_rsdp = (uint64_t)ST->ConfigurationTable[i].VendorTable;
}

/*
 * Get the final memory map and leave boot services. ExitBootServices fails
 * if the map changed since we read it, so retry with a fresh map. No
 * allocations are allowed between the two calls, hence the oversized buffer.
 */
static EFI_STATUS exit_boot_services(EFI_HANDLE image, struct bootinfo *bi) {
    UINTN map_size = 0, map_key, desc_size;
    uint32_t desc_ver;
    EFI_MEMORY_DESCRIPTOR *map = 0;
    EFI_STATUS s;

    BS->GetMemoryMap(&map_size, map, &map_key, &desc_size, &desc_ver);
    map_size += 16 * desc_size;
    UINTN capacity = map_size;
    if (EFI_ERROR(s = BS->AllocatePool(EfiLoaderData, capacity, (void **)&map)))
        return fail(u"cannot allocate memory map", s);

    for (int attempt = 0; attempt < 8; attempt++) {
        map_size = capacity;
        if (EFI_ERROR(s = BS->GetMemoryMap(&map_size, map, &map_key, &desc_size, &desc_ver)))
            return fail(u"GetMemoryMap failed", s);
        bi->mmap = (uint64_t)map;
        bi->mmap_size = map_size;
        bi->mmap_desc_size = desc_size;
        if (!EFI_ERROR(BS->ExitBootServices(image, map_key))) return EFI_SUCCESS;
    }
    return fail(u"ExitBootServices kept failing", EFI_LOAD_ERROR);
}


/* ---- Page tables ------------------------------------------------------- */

#define PT_POOL_PAGES 512 /* 2 MiB of page tables: enough for ~250 GiB of RAM */
#define PTE_P 0x1ull
#define PTE_RW 0x2ull
#define PTE_PS 0x80ull
#define PTE_ADDR 0x000FFFFFFFFFF000ull

static uint64_t *pt_pool;
static UINTN pt_used;

static uint64_t *pt_alloc(void) {
    if (pt_used == PT_POOL_PAGES)
        for (;;) __asm__ volatile("cli; hlt"); /* boot services are gone; nothing to report to */
    uint64_t *t = pt_pool + pt_used++ * 512;
    memset(t, 0, 4096);
    return t;
}

static uint64_t *pt_next(uint64_t *table, int idx) {
    if (!(table[idx] & PTE_P)) table[idx] = (uint64_t)pt_alloc() | PTE_P | PTE_RW;
    return (uint64_t *)(table[idx] & PTE_ADDR);
}

static void map_4k(uint64_t *pml4, uint64_t virt, uint64_t phys) {
    uint64_t *pdpt = pt_next(pml4, (virt >> 39) & 511);
    uint64_t *pd = pt_next(pdpt, (virt >> 30) & 511);
    uint64_t *pt = pt_next(pd, (virt >> 21) & 511);
    pt[(virt >> 12) & 511] = phys | PTE_P | PTE_RW;
}

static void map_2m_range(uint64_t *pml4, uint64_t virt_off, uint64_t start, uint64_t end) {
    for (uint64_t pa = start & ~0x1FFFFFull; pa < end; pa += 0x200000) {
        uint64_t va = pa + virt_off;
        uint64_t *pdpt = pt_next(pml4, (va >> 39) & 511);
        uint64_t *pd = pt_next(pdpt, (va >> 30) & 511);
        pd[(va >> 21) & 511] = pa | PTE_P | PTE_RW | PTE_PS;
    }
}

static void map_phys_range(uint64_t *pml4, uint64_t start, uint64_t end) {
    map_2m_range(pml4, 0, start, end);         /* identity, so this code keeps running */
    map_2m_range(pml4, HHDM_BASE, start, end); /* direct map for the kernel */
}

static uint64_t *build_page_tables(struct bootinfo *bi) {
    uint64_t *pml4 = pt_alloc();
    for (uint64_t off = 0; off < bi->mmap_size; off += bi->mmap_desc_size) {
        struct mem_desc *d = (struct mem_desc *)(bi->mmap + off);
        if (mem_type_is_mapped(d->type))
            map_phys_range(pml4, d->phys_start, d->phys_start + d->num_pages * 4096);
    }
    map_phys_range(pml4, bi->fb.base, bi->fb.base + bi->fb.size);
    for (uint64_t o = 0; o < bi->kernel_end - bi->kernel_start; o += 4096)
        map_4k(pml4, bi->kernel_virt + o, bi->kernel_start + o);
    return pml4;
}

EFI_STATUS EFIAPI efi_main(EFI_HANDLE image, EFI_SYSTEM_TABLE *systab) {
    ST = systab;
    BS = systab->BootServices;
    BS->SetWatchdogTimer(0, 0, 0, 0); /* firmware would otherwise reset us after 5 minutes */

    ST->ConOut->ClearScreen(ST->ConOut);
    print(u"\u00e6ther bootloader\r\n");

    struct bootinfo *bi;
    EFI_STATUS s = BS->AllocatePool(EfiLoaderData, sizeof *bi, (void **)&bi);
    if (EFI_ERROR(s)) return fail(u"out of memory", s);
    memset(bi, 0, sizeof *bi);
    bi->magic = BOOTINFO_MAGIC;
    bi->boot_slot = BOOT_NO_SLOT;

    if (EFI_ERROR(s = setup_framebuffer(bi))) return s;
    find_acpi(bi);

    uint64_t entry = 0;
    int netboot = file_exists(image, NETBOOT_PATH);
    if (netboot) {
        bi->boot_flags |= BOOT_FLAG_NETBOOT;
        print(u"[boot] netboot: live installer mode\r\n");
    }
    /* The netboot medium always runs its live kernel. Otherwise prefer an
     * installed A/B system, and fall back to a live kernel when neither slot
     * can be loaded (a USB stick with a kernel still boots). */
    int installed = netboot ? 0 : boot_installed(image, bi, &entry);
    if (installed == 1) install_boot_entry(image);
    if (installed != 1) {
        bi->nmodules = 0;
        bi->boot_flags &= ~BOOT_FLAG_INSTALLED;
        bi->boot_slot = BOOT_NO_SLOT;
        void *elf;
        UINTN elf_size;
        if (EFI_ERROR(s = read_kernel(image, &elf, &elf_size))) return s;
        if (EFI_ERROR(s = load_elf(elf, elf_size, bi, &entry))) return s;
        read_modules(image, bi);
        read_iso_apps(bi);
    }

    EFI_PHYSICAL_ADDRESS pool;
    if (EFI_ERROR(s = BS->AllocatePages(AllocateAnyPages, EfiLoaderData, PT_POOL_PAGES, &pool)))
        return fail(u"cannot allocate page tables", s);
    pt_pool = (uint64_t *)pool;

    print(u"[boot] kernel entry ");
    print_hex(entry);
    print(u", starting...\r\n");

    if (EFI_ERROR(s = exit_boot_services(image, bi))) return s;

    uint64_t *pml4 = build_page_tables(bi);
    __asm__ volatile("mov %0, %%cr3" : : "r"(pml4) : "memory");

    /* The kernel uses the System V calling convention, not Microsoft's. */
    void (__attribute__((sysv_abi)) *kernel_entry)(struct bootinfo *) = (void *)entry;
    kernel_entry(bi);
    for (;;) __asm__ volatile("cli; hlt");
}
