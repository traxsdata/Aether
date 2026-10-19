# æther build. Needs: clang, lld (ld.lld + lld-link), make, mtools, xorriso.
# Optional: qemu-system-x86_64 + OVMF for `make run` and `make test`.
#
#   make          build build/aether.iso (boot it in VMware, or write it to a USB stick)
#   make run      boot the ISO in QEMU with UEFI firmware, serial log on the terminal
#   make test     headless boot test in QEMU (screenshot + serial log)
#   make unittest run the kernel unit tests (the terminal's `test` command) headless in QEMU
#   build/disk.img (or $AETHER_DISK) is attached as a SATA disk when it exists
#   make clean

BUILD := build
CC    := clang
VERSION ?= 1.4.0
# Set SOURCE_DATE_EPOCH to make the build date (and therefore the image)
# reproducible. CI uses the tag's commit time.
SOURCE_DATE_EPOCH ?=
BUILD_DATE ?= $(if $(SOURCE_DATE_EPOCH),$(shell date -u -d '@$(SOURCE_DATE_EPOCH)' '+%Y-%m-%d %H:%M UTC'),$(shell date -u '+%Y-%m-%d %H:%M UTC'))
# Optional publisher key for app packages. CI sets AETHER_SIGNING_KEY to the
# offline release key; a local build without it produces unsigned packages
# (accepted for local installs, refused by the App Store's network path).
AETHER_APP_SIGNING_KEY ?= $(AETHER_SIGNING_KEY)
export AETHER_APP_SIGNING_KEY

COMMON_FLAGS := -ffreestanding -fno-builtin -std=c11 -O2 -g -Wall -Wextra -Wno-unused-parameter

BOOT_CFLAGS := -target x86_64-unknown-windows -ffreestanding -fshort-wchar -mno-red-zone \
               -fno-stack-protector -fno-builtin -std=c11 -O2 -Wall -Wextra
BOOT_LDFLAGS := -subsystem:efi_application -entry:efi_main -nodefaultlib

# Kernel: higher half, no SSE (so user FPU state is never clobbered), no red
# zone (interrupts run on the same stack), stack canaries on.
KERNEL_CFLAGS := -target x86_64-unknown-elf $(COMMON_FLAGS) -mcmodel=kernel -fno-pic -fno-pie \
                 -mno-red-zone -mgeneral-regs-only -fstack-protector-strong -fno-omit-frame-pointer
KERNEL_LDFLAGS := -nostdlib -static -no-pie -z max-page-size=4096 -T kernel/linker.ld

# Apps: ordinary user-mode code at 0x400000, SSE allowed, stack canaries on.
USER_CFLAGS := -target x86_64-unknown-elf $(COMMON_FLAGS) -fno-pic -fno-pie -fstack-protector-strong
USER_LDFLAGS := -nostdlib -static -no-pie -z max-page-size=4096 -T user/linker.ld

COMMON_SRCS := common/gfx.c common/string.c common/fmt.c
KERNEL_SRCS := kernel/entry.S kernel/main.c kernel/lib.c kernel/log.c kernel/serial.c kernel/gdt.c \
               kernel/idt.c kernel/isr.S kernel/pic.c kernel/input.c kernel/ps2.c kernel/rtc.c kernel/pmm.c kernel/vmm.c \
               kernel/heap.c kernel/proc.c kernel/switch.S kernel/syscall.c kernel/wm.c kernel/apps.c \
               kernel/clipboard.c \
               kernel/apps.S kernel/rng.c kernel/crypto.c kernel/afs.c kernel/vfs.c kernel/dlp.c kernel/umem.c kernel/storage.c kernel/pci.c kernel/blk.c kernel/ahci.c \
               kernel/elf.c kernel/linux.c kernel/ktest.c kernel/e1000.c kernel/net.c kernel/firewall.c \
               kernel/usb.c kernel/usb_hid.c kernel/usb_net.c kernel/usb_storage.c kernel/i2c_hid.c \
               kernel/appstore.c kernel/settings.c kernel/update.c kernel/power.c kernel/drivers.c common/gpt.c common/rsa_verify.c
ULIB_SRCS := user/lib/crt0.S user/lib/ui.c user/lib/runtime.c
# Native C apps, built into the kernel image. Settings and App Store are
# Rust packages built by tools/build-rust-apps.sh into build/apps.
APPS := terminal calculator notes probe explorer

KERNEL_OBJS := $(patsubst %,$(BUILD)/k/%.o,$(KERNEL_SRCS) $(COMMON_SRCS))
ULIB_OBJS := $(patsubst %,$(BUILD)/u/%.o,$(ULIB_SRCS) $(COMMON_SRCS))
APP_ELFS := $(patsubst %,$(BUILD)/user/%.elf,$(APPS))
HEADERS := $(wildcard include/*.h common/*.h kernel/*.h user/lib/*.h)
# Installed app packages (tools/mkapp.py, tools/craft-build.sh) go in /apps on the ISO.
RUST_APPS ?= 1
RUST_APP_NAMES := settings installer
RUST_APP_PKGS := $(patsubst %,$(BUILD)/apps/%.app,$(RUST_APP_NAMES))
RUST_SRCS := $(shell find runner/apps runner/eframe runner/aether-host runner/webnet -type f 2>/dev/null) \
             runner/Cargo.toml runner/Cargo.lock tools/build-rust-apps.sh
ifeq ($(RUST_APPS),1)
PACKAGES := $(sort $(wildcard $(BUILD)/apps/*.app) $(RUST_APP_PKGS))
else
PACKAGES := $(filter-out $(RUST_APP_PKGS),$(wildcard $(BUILD)/apps/*.app))
endif

OVMF_CODE ?= $(firstword $(wildcard /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd \
                                    /usr/share/ovmf/OVMF.fd /usr/share/edk2/x64/OVMF_CODE.fd))

.PHONY: all run test unittest test-lock test-power test-nvme test-usb clean rust-apps craft-apps images boot-image system-image netboot FORCE
all: $(BUILD)/aether.iso

# The pieces a release publishes. The netboot ISO and the boot image change
# only when the loader/installer do; the system image changes with the OS.
images: $(BUILD)/boot.img $(BUILD)/system.img $(BUILD)/aether-netboot.iso
boot-image: $(BUILD)/boot.img
system-image: $(BUILD)/system.img
netboot: $(BUILD)/aether-netboot.iso

$(BUILD)/boot.img: $(BUILD)/BOOTX64.EFI tools/mkboot.sh
	tools/mkboot.sh installed $@

$(BUILD)/system.img: $(BUILD)/kernel.elf $(BUILD)/boot.img $(RUST_APP_PKGS) tools/mksysimage.py
	tools/mksysimage.py --output $@ --version $(VERSION) --build '$(BUILD_DATE)' \
	    --key "$(AETHER_SIGNING_KEY)" --boot $(BUILD)/boot.img --kernel $(BUILD)/kernel.elf $(RUST_APP_PKGS)

$(BUILD)/netboot-esp.img: $(BUILD)/BOOTX64.EFI $(BUILD)/kernel.elf $(BUILD)/apps/installer.app tools/mkboot.sh
	tools/mkboot.sh netboot $@

$(BUILD)/aether-netboot.iso: $(BUILD)/netboot-esp.img
	rm -rf $(BUILD)/netboot-iso && mkdir -p $(BUILD)/netboot-iso
	printf 'æther netboot installer\n' > $(BUILD)/netboot-iso/README.TXT
	xorriso -as mkisofs -quiet -R -J -V AETHERNET \
	    -append_partition 2 0xef $< \
	    -e --interval:appended_partition_2:all:: -no-emul-boot \
	    -o $@ $(BUILD)/netboot-iso

BOOT_OBJS := $(BUILD)/boot/main.o $(BUILD)/boot/sha256.o $(BUILD)/boot/gpt.o $(BUILD)/boot/rsa_verify.o

$(BUILD)/boot/%.o: boot/%.c boot/efi.h include/bootinfo.h common/gpt.h common/sysimage.h common/bootcontrol.h common/release_key.h common/rsa_verify.h
	@mkdir -p $(dir $@)
	$(CC) $(BOOT_CFLAGS) -c $< -o $@

$(BUILD)/boot/gpt.o: common/gpt.c common/gpt.h
	@mkdir -p $(dir $@)
	$(CC) $(BOOT_CFLAGS) -c $< -o $@

$(BUILD)/boot/rsa_verify.o: common/rsa_verify.c common/rsa_verify.h
	@mkdir -p $(dir $@)
	$(CC) $(BOOT_CFLAGS) -c $< -o $@

$(BUILD)/BOOTX64.EFI: $(BOOT_OBJS)
	lld-link $(BOOT_LDFLAGS) -out:$@ $(BOOT_OBJS)

$(BUILD)/u/%.o: % $(HEADERS)
	@mkdir -p $(dir $@)
	$(CC) $(USER_CFLAGS) -c $< -o $@

$(BUILD)/user/%.elf: $(BUILD)/u/user/%.c.o $(ULIB_OBJS) user/linker.ld
	@mkdir -p $(dir $@)
	ld.lld $(USER_LDFLAGS) -o $@ $< $(ULIB_OBJS)

# apps.S embeds the app binaries, so it depends on them.
$(BUILD)/k/kernel/apps.S.o: kernel/apps.S $(APP_ELFS)
	@mkdir -p $(dir $@)
	$(CC) $(KERNEL_CFLAGS) -c $< -o $@

$(BUILD)/k/%.o: % $(HEADERS)
	@mkdir -p $(dir $@)
	$(CC) $(KERNEL_CFLAGS) -c $< -o $@

# The no_std Rust kernel drivers. Built separately with Cargo and linked in
# as a static library; they use only the kernel's C allocation/MMIO helpers.
RUST_DRIVER_SRCS := $(shell find drivers -type f -not -path '*/target/*' 2>/dev/null) tools/build-kernel-drivers.sh
$(BUILD)/rust-kernel/libaether_nvme.a: $(RUST_DRIVER_SRCS)
	tools/build-kernel-drivers.sh

$(BUILD)/kernel.elf: $(KERNEL_OBJS) $(BUILD)/rust-kernel/libaether_nvme.a kernel/linker.ld
	ld.lld $(KERNEL_LDFLAGS) -o $@ $(KERNEL_OBJS) $(BUILD)/rust-kernel/libaether_nvme.a

# FAT EFI System Partition image: \EFI\BOOT\BOOTX64.EFI is the path UEFI
# firmware boots by default from removable media; the kernel sits beside it.
$(BUILD)/esp.img: $(BUILD)/BOOTX64.EFI $(BUILD)/kernel.elf
	rm -f $@
	dd if=/dev/zero of=$@ bs=1M count=16 status=none
	mformat -i $@ -v AETHER ::
	mmd -i $@ ::/EFI ::/EFI/BOOT
	mcopy -i $@ $(BUILD)/BOOTX64.EFI ::/EFI/BOOT/BOOTX64.EFI
	mcopy -i $@ $(BUILD)/kernel.elf ::/kernel.elf

# Hybrid ISO: boots as a CD in VMware, and as a GPT disk when written to USB.
# App packages go in /apps of the ISO 9660 file system (the boot partition is
# capped at 32 MiB by El Torito); the loader reads them from the raw sectors.
# Rebuild the ISO when the set of packages changes, not only their contents.
# The Rust apps are ordinary eframe programs packaged by tools/mkapp.py.
# The stamp keeps `make` from invoking Cargo when nothing changed.
ifeq ($(RUST_APPS),1)
rust-apps: $(BUILD)/rust-apps.stamp
$(BUILD)/rust-apps.stamp: $(RUST_SRCS)
	tools/build-rust-apps.sh
	@touch $@
$(RUST_APP_PKGS): $(BUILD)/rust-apps.stamp ;
endif

# Prebuilt Craft app packages from github.com/traxsdata/Aether-CraftApps,
# checked against their SHA-256 and saved as build/apps/<name>.app. Run
# `make craft-apps` (CRAFT_APPS="gridcraft" for a subset), then `make`.
craft-apps:
	tools/fetch-craft-apps.py $(CRAFT_APPS)

$(BUILD)/packages.list: FORCE
	@mkdir -p $(BUILD)
	@echo "$(PACKAGES)" | cmp -s - $@ || echo "$(PACKAGES)" > $@
FORCE:

$(BUILD)/aether.iso: $(BUILD)/esp.img $(PACKAGES) $(BUILD)/packages.list
	rm -rf $(BUILD)/iso && mkdir -p $(BUILD)/iso/apps
	for p in $(PACKAGES); do cp $$p $(BUILD)/iso/apps/; done
	xorriso -as mkisofs -quiet -R -J -V AETHER \
	    -append_partition 2 0xef $< \
	    -e --interval:appended_partition_2:all:: -no-emul-boot \
	    -o $@ $(BUILD)/iso

# Blank 64 MiB test disk; QEMU's q35 ICH9 controller exposes it over AHCI.
$(BUILD)/disk.img:
	@mkdir -p $(dir $@)
	dd if=/dev/zero of=$@ bs=1M count=64 status=none

# The disk: $AETHER_DISK if set, else build/disk.img if it exists.
run: $(BUILD)/aether.iso
	@test -n "$(OVMF_CODE)" || { echo "OVMF firmware not found; set OVMF_CODE=/path/to/OVMF_CODE.fd"; exit 1; }
	disk="$${AETHER_DISK:-}"; [ -n "$$disk" ] || [ ! -f $(BUILD)/disk.img ] || disk=$(BUILD)/disk.img; \
	qemu-system-x86_64 -machine q35 -cpu $${QEMU_CPU:-max} -m 1G \
	    -drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
	    -cdrom $< -serial stdio -no-reboot \
	    $${disk:+-drive file=$$disk,format=raw,if=none,id=d0 -device ide-hd,drive=d0,bus=ide.0} $(QEMU_FLAGS)

test: $(BUILD)/aether.iso $(BUILD)/disk.img
	tools/test-qemu.sh

unittest: $(BUILD)/aether.iso
	tools/test-unit.sh

test-lock: $(BUILD)/aether.iso
	tools/test-lock.sh

test-usb-storage: $(BUILD)/aether.iso
	tools/test-usb-storage.sh

test-power: $(BUILD)/aether.iso
	tools/test-power.sh

test-nvme:
	tools/test-nvme.sh

test-usb:
	tools/test-usb.sh

clean:
	rm -rf $(BUILD)
