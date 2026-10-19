/*
 * main.c - kernel entry: bring the machine up in a safe order, then hand
 * the CPU to the window manager loop.
 */
#include "../include/abi.h"
#include "../include/bootinfo.h"
#include "ahci.h"
#include "drivers.h"
#include "apps.h"
#include "cpu.h"
#include "gdt.h"
#include "idt.h"
#include "input.h"
#include "kernel.h"
#include "mm.h"
#include "pci.h"
#include "rng.h"
#include "storage.h"
#include "update.h"
#include "net.h"
#include "usb.h"
#include "i2c_hid.h"
#include "pic.h"
#include "proc.h"
#include "power.h"
#include "wm.h"

uint32_t cpu_features;
uint32_t boot_flags, boot_slot = BOOT_NO_SLOT;
uint8_t boot_disk_guid[16];
char cpu_vendor[16], cpu_brand[48];
struct framebuffer boot_fb;
extern uint64_t __stack_chk_guard;

static void detect_cpu(void) {
    uint32_t a, b, c, d, max;
    cpuid(0, 0, &max, &b, &c, &d);
    memcpy(cpu_vendor, &b, 4);
    memcpy(cpu_vendor + 4, &d, 4);
    memcpy(cpu_vendor + 8, &c, 4);
    cpuid(1, 0, &a, &b, &c, &d);
    if (c & (1u << 30)) cpu_features |= CPU_FEAT_RDRAND;
    if (max >= 7) {
        cpuid(7, 0, &a, &b, &c, &d);
        if (b & (1u << 7)) cpu_features |= CPU_FEAT_SMEP;
        if (b & (1u << 20)) cpu_features |= CPU_FEAT_SMAP;
        if (c & (1u << 2)) cpu_features |= CPU_FEAT_UMIP;
    }
    cpuid(0x80000000, 0, &max, &b, &c, &d);
    if (max >= 0x80000001) {
        cpuid(0x80000001, 0, &a, &b, &c, &d);
        if (d & (1u << 20)) cpu_features |= CPU_FEAT_NX;
    }
    if (max >= 0x80000004) {
        for (uint32_t i = 0; i < 3; i++) {
            cpuid(0x80000002 + i, 0, &a, &b, &c, &d);
            memcpy(cpu_brand + i * 16, &a, 4);
            memcpy(cpu_brand + i * 16 + 4, &b, 4);
            memcpy(cpu_brand + i * 16 + 8, &c, 4);
            memcpy(cpu_brand + i * 16 + 12, &d, 4);
        }
        cpu_brand[47] = 0;
    }
}

static void enable_protections(void) {
    if (!(cpu_features & CPU_FEAT_NX)) panic("this CPU has no NX bit; refusing to run without W^X");
    uint64_t cr4 = read_cr4();
    if (cpu_features & CPU_FEAT_SMEP) cr4 |= CR4_SMEP; /* kernel may never execute user pages */
    if (cpu_features & CPU_FEAT_SMAP) cr4 |= CR4_SMAP; /* ...or touch them outside copy_in/out */
    if (cpu_features & CPU_FEAT_UMIP) cr4 |= CR4_UMIP; /* user mode can't read GDT/IDT addresses */
    write_cr4(cr4);
    kprintf("[sec] NX on, SMEP %s, SMAP %s, UMIP %s, kernel W^X, stack canaries on\n",
            (cpu_features & CPU_FEAT_SMEP) ? "on" : "unavailable", (cpu_features & CPU_FEAT_SMAP) ? "on" : "unavailable",
            (cpu_features & CPU_FEAT_UMIP) ? "on" : "unavailable");
}

static void start_desktop(void) {
    const char *app = (boot_flags & BOOT_FLAG_NETBOOT) ? "installer" : "terminal";
    const struct program *p = program_find(app);
    if (!p) p = program_find("terminal");
    if (p) proc_spawn(p->name, p->caps, 0, 0);
}

__attribute__((no_stack_protector, noreturn)) void kmain(uint64_t bootinfo_phys) {
    serial_init();
    struct bootinfo *bi = P2V(bootinfo_phys);
    if (bi->magic != BOOTINFO_MAGIC) halt_forever(); /* wrong loader: nothing safe to draw on */
    boot_fb = bi->fb;
    boot_flags = bi->boot_flags;
    boot_slot = bi->boot_slot;
    memcpy(boot_disk_guid, bi->boot_disk_guid, sizeof boot_disk_guid);

    detect_cpu();
    __stack_chk_guard = random_u64();

    console_init(&boot_fb);
    kprintf("\xC3\xA6ther " AETHER_VERSION " booting on %s\n", cpu_brand[0] ? cpu_brand : cpu_vendor);
    kprintf("[boot] framebuffer %ux%u, kernel at phys %lx-%lx\n", boot_fb.width, boot_fb.height, bi->kernel_start,
            bi->kernel_end);

    gdt_init();
    idt_init();
    pmm_init(bi);
    kprintf("[mm] %lu MiB usable\n", pmm_total_pages() / 256);
    vmm_init(bi);
    power_init(bi->acpi_rsdp);
    enable_protections();
    heap_init();
    programs_install(bi);

    pic_init();
    lapic_quiesce();
    pit_init(TICK_HZ);
    pic_unmask(0);
    ps2_init();
    proc_init();
    syscall_init();

    /* Storage. After pit_init: the AHCI driver's timeouts count PIT periods
     * while interrupts are still off. */
    pci_init();
    ahci_init();
    ahci_selftest(); /* only touches disks whose model says QEMU */
    drivers_init(); /* Rust drivers: NVMe registers as nvme0 */
    usb_init();     /* xHCI, USB HID and USB Ethernet */
    i2c_hid_init(); /* Yoga touchpad/pen over LPSS I2C */

    wm_init(&boot_fb);
    kprintf("[boot] desktop ready\n");
    sti();
    rng_init(); /* needs the timer ticking */
    net_init(); /* needs random numbers: DHCP ids, ports, sequence numbers */
    storage_init(start_desktop);
    /* The kernel and its storage path are up: confirm this A/B slot before
     * asking for a passphrase, so an unattended lock screen is not counted
     * as a failed boot. */
    update_boot_good();
    wm_run();
}
