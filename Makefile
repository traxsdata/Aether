# SPDX-License-Identifier: Apache-2.0

SHELL := /bin/sh

CARGO ?= cargo
AS ?= as
LD ?= ld
PYTHON ?= python3

BUILD := build
KERNEL_ELF := $(BUILD)/aether-kernel.elf
USER_ELF := $(BUILD)/aether-init.elf
LINUXD_ELF := $(BUILD)/aether-linuxd.elf
LOADER_EFI := $(BUILD)/BOOTX64.EFI
LINUX_ELF := $(BUILD)/linux-hello.elf
LINUX_DYN_ELF := $(BUILD)/linux-dynamic.elf
LINUX_INTERP_ELF := $(BUILD)/linux-interp.elf
LINUX_EXEC_ELF := $(BUILD)/linux-exec.elf
LINUX_DYN_EXEC_ELF := $(BUILD)/linux-dynamic-exec.elf
LINUX_APP_ELF := $(BUILD)/linux-appimage-app.elf
GATE4_APP_ELF := $(BUILD)/linux-gate4-portal.elf
GATE4_APPIMAGE := $(BUILD)/linux-gate4-portal.AppImage
GATE4_APPIMAGE_RECORD := $(BUILD)/linux-gate4-portal.import
GATE4_GUI_APP_ELF := $(BUILD)/linux-gate4-gui-portal.elf
GATE4_GUI_APPIMAGE := $(BUILD)/linux-gate4-gui-portal.AppImage
GATE4_GUI_APPIMAGE_RECORD := $(BUILD)/linux-gate4-gui-portal.import
GATE4_GUI_NAMESPACE := /run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx
LINUX_RUNTIME_DIR := $(BUILD)/linux-runtime-pinned-root
LINUX_ROOTFS := $(BUILD)/linux-runtime.sqfs
LINUX_ROOTFS_RECORD := $(BUILD)/linux-runtime.import
LINUX_RUNTIME_PIN := $(BUILD)/linux-runtime-pinned.pin
LINUX_RUNTIME_MANIFEST := $(BUILD)/linux-runtime-pinned.manifest
LINUX_APPIMAGE := $(BUILD)/linux-user.AppImage
LINUX_APPIMAGE_RECORD := $(BUILD)/linux-user.import
# Artifacts copied onto the boot volume by the image target. The staged
# AppImage is not embedded in the kernel; the loader reads these paths.
STAGED_APPIMAGE ?= $(LINUX_APPIMAGE)
STAGED_APPIMAGE_RECORD ?= $(LINUX_APPIMAGE_RECORD)
# The runtime rootfs is staged too. Gate 3 stages the same image it used to
# embed; Gate 4 overrides these with the Mesa/lavapipe rootfs.
STAGED_RUNTIME_ROOTFS ?= $(LINUX_ROOTFS)
STAGED_RUNTIME_RECORD ?= $(LINUX_ROOTFS_RECORD)
TAMPERED_APPIMAGE := $(BUILD)/linux-user-tampered.AppImage
LINUX_APPIMAGE_PROGRAM ?= /usr/bin/app
# Optional | separated argv for the selected AppImage program. The default is
# empty so the static boot fixture keeps argv = [program path].
LINUX_APPIMAGE_ARGS ?=
# Experimental Gate 3 headless GridCraft run.
GATECRAFT_APPIMAGE ?= /tmp/opencode/gridcraft.AppImage
GATE3_APP_PROGRAM ?= /usr/bin/gridcraft-cli
GATE3_APP_ARGS ?= run|--in|/usr/share/gridcraft/gate3-in.csv|--cmd|formulas.evaluate={"formula":"=1+1"}|--cmd|cell.set={"cell":"A4","input":"total"}|--cmd|cell.set={"cell":"B4","input":"=SUM(B2:B3)"}|--out|/tmp/gate3-out.csv|--print|A1:B4|--csv
GATE3_RUNTIME_VERSION ?= $(shell getconf GNU_LIBC_VERSION | tr ' ' '-')
LINUX_RUNTIME_SEED := tests/fixtures/keys/test.seed
LINUX_DYN_PATH ?= /usr/bin/dynamic
LINUX_RUNTIME_PREBUILT ?= 0
# Gate 4 GUI runtime rootfs and boot smoke test.
GATE4_RUNTIME_ROOTFS := $(BUILD)/gate4-runtime.sqfs
GATE4_RUNTIME_RECORD := $(BUILD)/gate4-runtime.import
GATE4_RUNTIME_PIN := $(BUILD)/gate4-runtime.pin
GATE4_RUNTIME_MANIFEST := $(BUILD)/gate4-runtime.manifest
GATE4_RUNTIME_TAMPERED := $(BUILD)/gate4-runtime-tampered.sqfs
GATE4_EXTRA_ROOT := $(BUILD)/gate4-runtime-extra
GATE4_APPIMAGE_RECORD := $(BUILD)/gridcraft.import
GATE4_BRIDGE_BUILD := $(BUILD)/gate4-bridge
# The Gate 4 GUI launch is a later slice. This boot smoke test runs the real
# AppImage's CLI through the Gate 4 rootfs and verifies its version.
GATE4_APP_PROGRAM ?= /usr/bin/gridcraft-cli
GATE4_APP_ARGS ?= --version
# Real windowed GUI: the app opens the portal-granted workbook as argv.
GATE4_GUI_PROGRAM ?= /usr/bin/gridcraft
GATE4_GUI_ARGS ?= $(GATE4_GUI_NAMESPACE)
IMAGE_SIZE ?= 64M
IMAGE := $(BUILD)/aether.img

KERNEL_TARGET := x86_64-unknown-none
LOADER_TARGET := x86_64-unknown-uefi

AETHER_GIT_COMMIT := $(shell git describe --always --dirty --abbrev=12 --exclude='*' 2>/dev/null || echo unknown)
export AETHER_GIT_COMMIT

KERNEL_FEATURES ?=
KERNEL_FEATURE_FLAGS := $(if $(KERNEL_FEATURES),--features "$(KERNEL_FEATURES)")

# The native user program is linked at 2 TiB, outside the small code model.
# PIC keeps its internal references PC-relative; -no-pie keeps the image an
# ET_EXEC file with all relocations resolved at link time.
USER_RUSTFLAGS := -C relocation-model=pic -C link-arg=-no-pie

.PHONY: all kernel user loader image iso iso-test vmware-vmx test glibc-test gate3-test gate4-portal-test gate4-gui-portal-test gate4-display-test gate4-runtime gate4-runtime-image gate4-image-only gate4-runtime-test gate4-test shell-test fault-test rollback-test \
	negative-test appimage-stage-tamper-test appimage-stage-missing-test unittest lint docs-check license-check compat-test update-test release release-demo \
	runtime-rootfs-pinned runtime-package-test release-check qualify ci clean

all: image

user:
	@mkdir -p $(BUILD)
	RUSTFLAGS="$(USER_RUSTFLAGS)" \
		$(CARGO) build -p aether-init --release --target $(KERNEL_TARGET) --locked
	cp target/$(KERNEL_TARGET)/release/aether-init $(USER_ELF)
	cp target/$(KERNEL_TARGET)/release/aether-linuxd $(LINUXD_ELF)

FIXTURE_GATE4_GUI := $(if $(findstring gate4-gui-test,$(KERNEL_FEATURES)),--defsym AETHER_GATE4_GUI=1)

linux-fixture:
	@mkdir -p $(BUILD)
	$(AS) --64 $(FIXTURE_GATE4_GUI) tests/fixtures/linux/hello.S -o $(BUILD)/linux-hello.o
	$(LD) -m elf_x86_64 -static -no-pie -T tests/fixtures/linux/linker.ld \
		$(BUILD)/linux-hello.o -o $(LINUX_ELF)
	$(AS) --64 tests/fixtures/linux/dynamic.S -o $(BUILD)/linux-dynamic.o
	$(LD) -pie -e _start --dynamic-linker /lib/ld-aether.so \
		$(BUILD)/linux-dynamic.o -o $(LINUX_DYN_ELF)
	$(AS) --64 tests/fixtures/linux/interp.S -o $(BUILD)/linux-interp.o
	$(LD) -shared -e _start $(BUILD)/linux-interp.o -o $(LINUX_INTERP_ELF)
	$(AS) --64 tests/fixtures/linux/exec.S -o $(BUILD)/linux-exec.o
	$(LD) -m elf_x86_64 -static -no-pie -T tests/fixtures/linux/linker.ld \
		$(BUILD)/linux-exec.o -o $(LINUX_EXEC_ELF)
	$(AS) --64 tests/fixtures/linux/dynamic-exec.S -o $(BUILD)/linux-dynamic-exec.o
	$(LD) -pie -e _start --dynamic-linker /lib/ld-aether.so \
		$(BUILD)/linux-dynamic-exec.o -o $(LINUX_DYN_EXEC_ELF)
	$(AS) --64 tests/fixtures/linux/appimage.S -o $(BUILD)/linux-appimage-app.o
	$(LD) -m elf_x86_64 -static -no-pie -T tests/fixtures/linux/linker.ld \
		$(BUILD)/linux-appimage-app.o -o $(LINUX_APP_ELF)
	$(AS) --64 tests/fixtures/linux/gate4-portal.S -o $(BUILD)/linux-gate4-portal.o
	$(LD) -m elf_x86_64 -static -no-pie -T tests/fixtures/linux/linker.ld \
		$(BUILD)/linux-gate4-portal.o -o $(GATE4_APP_ELF)
	$(AS) --64 tests/fixtures/linux/gate4-gui-portal.S -o $(BUILD)/linux-gate4-gui-portal.o
	$(LD) -m elf_x86_64 -static -no-pie -T tests/fixtures/linux/linker.ld \
		$(BUILD)/linux-gate4-gui-portal.o -o $(GATE4_GUI_APP_ELF)
	if [ "$(LINUX_RUNTIME_PREBUILT)" = "0" ]; then \
		sh tools/build-pinned-runtime.sh $(LINUX_ROOTFS) $(LINUX_ROOTFS_RECORD) \
			$(LINUX_RUNTIME_PIN) $(LINUX_RUNTIME_MANIFEST) $(LINUX_RUNTIME_SEED); \
	fi
	sh tools/build-appimage-fixture.sh $(LINUX_APPIMAGE) $(LINUX_APPIMAGE_RECORD) \
		$(LINUX_RUNTIME_SEED) $(LINUX_APP_ELF)

kernel: user linux-fixture
	@mkdir -p $(BUILD)
	AETHER_USER_ELF=$(abspath $(USER_ELF)) \
	AETHER_LINUXD_ELF=$(abspath $(LINUXD_ELF)) \
	AETHER_LINUX_ELF=$(abspath $(LINUX_ELF)) \
	AETHER_LINUX_ROOTFS=$(abspath $(LINUX_ROOTFS)) \
	AETHER_LINUX_ROOTFS_RECORD=$(abspath $(LINUX_ROOTFS_RECORD)) \
	AETHER_LINUX_RUNTIME_PIN="$$(cat $(LINUX_RUNTIME_PIN) 2>/dev/null || true)" \
	AETHER_LINUX_APPIMAGE_PROGRAM=$(LINUX_APPIMAGE_PROGRAM) \
	AETHER_LINUX_APPIMAGE_ARGS='$(LINUX_APPIMAGE_ARGS)' \
	AETHER_LINUX_DYN_PATH=$(LINUX_DYN_PATH) \
		$(CARGO) build -p aether-kernel --release --target $(KERNEL_TARGET) --locked $(KERNEL_FEATURE_FLAGS)
	cp target/$(KERNEL_TARGET)/release/aether-kernel $(KERNEL_ELF)

loader: kernel
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		$(CARGO) build -p aether-bootloader --release --target $(LOADER_TARGET) --locked
	cp target/$(LOADER_TARGET)/release/aether-bootloader.efi $(LOADER_EFI)

image: loader
	truncate -s $(IMAGE_SIZE) $(IMAGE)
	mformat -i $(IMAGE) -F -v AETHER ::
	mmd -i $(IMAGE) ::/EFI ::/EFI/BOOT
	mcopy -i $(IMAGE) $(LOADER_EFI) ::/EFI/BOOT/BOOTX64.EFI
	mmd -i $(IMAGE) ::/linux
	mcopy -i $(IMAGE) $(STAGED_APPIMAGE) ::/linux/app.AppImage
	mcopy -i $(IMAGE) $(STAGED_APPIMAGE_RECORD) ::/linux/app.import
	mcopy -i $(IMAGE) $(STAGED_RUNTIME_ROOTFS) ::/linux/runtime.sqfs
	mcopy -i $(IMAGE) $(STAGED_RUNTIME_RECORD) ::/linux/runtime.import
	@echo "built $(IMAGE)"

test: image
	$(PYTHON) tools/test-runtime-packaging.py
	$(PYTHON) tools/test-boot.py

# Build and inspect the pinned runtime profile without booting.
runtime-rootfs-pinned: linux-fixture
	@cat $(LINUX_RUNTIME_MANIFEST)

runtime-package-test: linux-fixture
	$(PYTHON) tools/test-runtime-packaging.py

# Opt-in host-ABI smoke test: package the host glibc and a dynamically linked
# hello, then boot it through the same loader. This is not the pinned runtime
# rootfs and is not part of `make ci`.
glibc-test:
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 linux-fixture
	sh tools/build-glibc-rootfs.sh $(LINUX_ROOTFS) $(LINUX_ROOTFS_RECORD) \
		$(LINUX_RUNTIME_SEED) $(LINUX_RUNTIME_PIN)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 LINUX_DYN_PATH=/usr/bin/hello image
	AETHER_SKIP_MARKERS=AETHER_LINUX_DYN_INTERP_OK,AETHER_LINUX_DYN_MAIN_OK \
	AETHER_EXTRA_MARKERS=AETHER_GLIBC_OK $(PYTHON) tools/test-boot.py

# Experimental Gate 3: run the real headless GridCraft CLI from the staged
# AppImage through PT_INTERP and the pinned runtime rootfs. The selected
# program and its argv are compiled into the kernel, so the image must be
# rebuilt after changing GATE3_APP_ARGS. This is deliberately outside `ci`
# until the Gate 3 runtime surface is complete.
gate3-test:
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 linux-fixture
	sh tools/build-gate3-runtime.sh $(LINUX_ROOTFS) $(LINUX_ROOTFS_RECORD) \
		$(LINUX_RUNTIME_PIN) $(LINUX_RUNTIME_MANIFEST) $(LINUX_RUNTIME_SEED)
	# The Gate 3 root intentionally contains only the application runtime, but
	# the boot gate still runs the Linux ABI fixtures. Add them after the Gate 3
	# manifest/pin are fixed (the manifest hashes glibc and ld.so only) and
	# re-sign the resulting tree so the loader-staged record matches it.
	@mkdir -p $(BUILD)/gate3-runtime-root/usr/bin
	# The Linux VMA fixture checks the first bytes of /etc/motd, so keep the
	# standard fixture line first and the Gate 3 provenance line after it.
	printf 'AETHER_LINUX_ROOT_OK source=/etc/motd\nAETHER_GATE3_ROOT_OK source=/etc/motd\n' \
		> $(BUILD)/gate3-runtime-root/etc/motd
	cp $(LINUX_INTERP_ELF) $(BUILD)/gate3-runtime-root/lib/ld-aether.so
	cp $(LINUX_DYN_ELF) $(BUILD)/gate3-runtime-root/usr/bin/dynamic
	cp $(LINUX_EXEC_ELF) $(BUILD)/gate3-runtime-root/usr/bin/exec
	cp $(LINUX_DYN_EXEC_ELF) $(BUILD)/gate3-runtime-root/usr/bin/dynamic-exec
	sh tools/build-runtime-rootfs.sh $(BUILD)/gate3-runtime-root \
		$(LINUX_ROOTFS) $(LINUX_RUNTIME_SEED) org.aether.linux-runtime \
		"$(GATE3_RUNTIME_VERSION)" "Aether Test Publisher" \
		$(LINUX_ROOTFS_RECORD)
	sh tools/build-gate3-appimage.sh $(GATECRAFT_APPIMAGE) \
		$(BUILD)/gridcraft.import $(LINUX_RUNTIME_SEED)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		LINUX_APPIMAGE_PROGRAM=$(GATE3_APP_PROGRAM) \
		LINUX_APPIMAGE_ARGS='$(GATE3_APP_ARGS)' \
		STAGED_APPIMAGE=$(GATECRAFT_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(BUILD)/gridcraft.import image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK \
	AETHER_EXTRA_MARKERS=AETHER_GATE3_APP_OK,AETHER_GATE3_DENIAL_OK \
	AETHER_REQUIRE_TEXT='AETHER_LINUX_APPIMAGE_STAGED_OK|bytes=19024376|AETHER_GATE3_APP_OK|AETHER_GATE3_DENIAL_OK|wrote /tmp/gate3-out.csv|item,value|alpha,1|beta,2|total,3|{"command":"formulas.evaluate","result":2.0}|{"command":"cell.set","result":{"cell":"B4","value":"3"}}' \
	$(PYTHON) tools/test-boot.py

# Gate 4 portal slice: run a small static Linux AppImage fixture that opens a
# seeded workbook through the exact-file portal namespace, saves new bytes,
# revokes the grant through a test-only kernel path, and proves the next read
# and write fail closed. The GUI and Wayland parts of Gate 4 are separate.
gate4-portal-test:
	$(MAKE) linux-fixture
	sh tools/build-appimage-fixture.sh $(GATE4_APPIMAGE) $(GATE4_APPIMAGE_RECORD) \
		$(LINUX_RUNTIME_SEED) $(GATE4_APP_ELF)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 KERNEL_FEATURES=gate4-portal-test \
		LINUX_APPIMAGE_PROGRAM=/usr/bin/app \
		STAGED_APPIMAGE=$(GATE4_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(GATE4_APPIMAGE_RECORD) image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK \
	AETHER_EXTRA_MARKERS=AETHER_GATE4_PORTAL_OK,AETHER_GATE4_SAVE_OK,AETHER_GATE4_REVOKE_OK \
	AETHER_REQUIRE_TEXT='AETHER_GATE4_PORTAL_OK path=/run/aether/portal/dd8f879ba78d207c/gate4.xlsx|AETHER_GATE4_SAVE_OK file=/home/gate4.xlsx bytes=58 staging=sctmp+rename temp_bytes=58 staged_bytes=87 renames=1|AETHER_GATE4_REVOKE_OK path=/run/aether/portal/dd8f879ba78d207c/gate4.xlsx|portal: audit revoked' \
	$(PYTHON) tools/test-boot.py
# Build the in-app libwayland-client bridge (client ABI -> /dev/aether-display).
gate4-bridge:
	@mkdir -p $(GATE4_BRIDGE_BUILD)
	$(MAKE) -C runtime/wayland BUILD=$(abspath $(GATE4_BRIDGE_BUILD))

# Build the Gate 4 Mesa/lavapipe runtime rootfs, sign it, and verify the
# manifest, every file digest, the loader closure, and the signed record on
# the host. This does not boot.
gate4-runtime: linux-fixture gate4-bridge
	@rm -rf $(GATE4_EXTRA_ROOT)
	@mkdir -p $(GATE4_EXTRA_ROOT)/lib $(GATE4_EXTRA_ROOT)/usr/bin $(GATE4_EXTRA_ROOT)/etc
	cp $(LINUX_INTERP_ELF) $(GATE4_EXTRA_ROOT)/lib/ld-aether.so
	cp $(LINUX_DYN_ELF) $(GATE4_EXTRA_ROOT)/usr/bin/dynamic
	cp $(LINUX_EXEC_ELF) $(GATE4_EXTRA_ROOT)/usr/bin/exec
	cp $(LINUX_DYN_EXEC_ELF) $(GATE4_EXTRA_ROOT)/usr/bin/dynamic-exec
	printf 'AETHER_LINUX_ROOT_OK source=/etc/motd\nAETHER_GATE4_RUNTIME_ROOT_OK source=/etc/motd\n' \
		> $(GATE4_EXTRA_ROOT)/etc/motd
	AETHER_WAYLAND_BRIDGE_DIR=$(abspath $(GATE4_BRIDGE_BUILD)) \
		sh tools/build-gate4-runtime.sh $(GATE4_RUNTIME_ROOTFS) \
		$(GATE4_RUNTIME_RECORD) $(GATE4_RUNTIME_PIN) \
		$(GATE4_RUNTIME_MANIFEST) $(LINUX_RUNTIME_SEED) $(GATE4_EXTRA_ROOT)
	sh tools/build-gate3-appimage.sh $(GATECRAFT_APPIMAGE) \
		$(GATE4_APPIMAGE_RECORD) $(LINUX_RUNTIME_SEED)
	$(PYTHON) tools/test-gate4-runtime.py \
		--root $(BUILD)/gate4-runtime-root \
		--manifest $(GATE4_RUNTIME_MANIFEST) \
		--rootfs $(GATE4_RUNTIME_ROOTFS) \
		--record $(GATE4_RUNTIME_RECORD) --pin $(GATE4_RUNTIME_PIN)

# Rebuild the boot image with the Gate 4 artifacts already on disk.
gate4-image-only:
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		KERNEL_FEATURES=gate4-runtime \
		LINUX_RUNTIME_PIN=$(GATE4_RUNTIME_PIN) \
		STAGED_RUNTIME_ROOTFS=$(GATE4_RUNTIME_ROOTFS) \
		STAGED_RUNTIME_RECORD=$(GATE4_RUNTIME_RECORD) \
		LINUX_APPIMAGE_PROGRAM=$(GATE4_APP_PROGRAM) \
		LINUX_APPIMAGE_ARGS='$(GATE4_APP_ARGS)' \
		STAGED_APPIMAGE=$(GATECRAFT_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(GATE4_APPIMAGE_RECORD) \
		IMAGE_SIZE=128M image

gate4-runtime-image: gate4-runtime
	$(MAKE) gate4-image-only

# Boot the staged Gate 4 runtime rootfs and require the kernel-owned marker,
# then prove tampered and missing runtime images fail closed.
gate4-runtime-test: gate4-runtime-image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK \
	AETHER_EXTRA_MARKERS=AETHER_GATE4_RUNTIME_OK \
	AETHER_BOOT_TIMEOUT=240 \
	AETHER_MEMORY=3G \
	AETHER_REQUIRE_TEXT='AETHER_GATE4_RUNTIME_OK|profile=gate4-host-lavapipe|mesa=lavapipe-|wayland=|AETHER_LINUX_APPIMAGE_STAGED_OK|bytes=19024376|gridcraft-cli 0.8.0|AETHER_LINUX_IMPORT_OK' \
	$(PYTHON) tools/test-boot.py
	@set -e; trap '$(MAKE) gate4-image-only >/dev/null' EXIT; \
		cp $(GATE4_RUNTIME_ROOTFS) $(GATE4_RUNTIME_TAMPERED); \
		$(PYTHON) -c 'import sys; p=sys.argv[1]; d=bytearray(open(p,"rb").read()); d[-1]^=0xff; open(p,"wb").write(d)' $(GATE4_RUNTIME_TAMPERED); \
		$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
			KERNEL_FEATURES=gate4-runtime \
			LINUX_RUNTIME_PIN=$(GATE4_RUNTIME_PIN) \
			STAGED_RUNTIME_ROOTFS=$(GATE4_RUNTIME_TAMPERED) \
			STAGED_RUNTIME_RECORD=$(GATE4_RUNTIME_RECORD) \
			LINUX_APPIMAGE_PROGRAM=$(GATE4_APP_PROGRAM) \
			LINUX_APPIMAGE_ARGS='$(GATE4_APP_ARGS)' \
			STAGED_APPIMAGE=$(GATECRAFT_APPIMAGE) \
			STAGED_APPIMAGE_RECORD=$(GATE4_APPIMAGE_RECORD) \
			IMAGE_SIZE=128M image >/dev/null; \
		AETHER_EXPECT_BOOT_FAILURE=1 \
		AETHER_EXPECT_FAILURE_TEXT='linux: rootfs signature or digest verification failed' \
		AETHER_MEMORY=3G AETHER_BOOT_TIMEOUT=240 $(PYTHON) tools/test-boot.py; \
		$(MAKE) gate4-image-only >/dev/null; \
		mdel -i $(IMAGE) ::/linux/runtime.sqfs; \
		AETHER_EXPECT_BOOT_FAILURE=1 \
		AETHER_EXPECT_FAILURE_TEXT='AETHER_LOADER_FAILED error=staged boot file' \
		AETHER_MEMORY=3G AETHER_BOOT_TIMEOUT=240 $(PYTHON) tools/test-boot.py

# Gate 4 windowed GridCraft: stage the signed GUI AppImage, the Mesa/lavapipe
# runtime rootfs (with the Wayland bridge), and the GUI portal workbook. The
# harness takes a screenshot and drives real PS/2 input; the kernel emits the
# present/input/save/revoke markers.
gate4-test: gate4-runtime
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		KERNEL_FEATURES="gate4-runtime gate4-gui-test" \
		LINUX_RUNTIME_PIN=$(GATE4_RUNTIME_PIN) \
		STAGED_RUNTIME_ROOTFS=$(GATE4_RUNTIME_ROOTFS) \
		STAGED_RUNTIME_RECORD=$(GATE4_RUNTIME_RECORD) \
		LINUX_APPIMAGE_PROGRAM=$(GATE4_GUI_PROGRAM) \
		LINUX_APPIMAGE_ARGS='$(GATE4_GUI_ARGS)' \
		STAGED_APPIMAGE=$(GATECRAFT_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(GATE4_APPIMAGE_RECORD) \
		IMAGE_SIZE=128M image
	AETHER_GATE4_TIMEOUT=600 AETHER_GATE4_SCREENSHOT=$(abspath $(BUILD))/gate4/gridcraft.ppm \
		AETHER_GATE4_POST_INPUT_SCREENSHOT=$(abspath $(BUILD))/gate4/gridcraft-after-input.ppm \
		AETHER_GATE4_CLICK=1 AETHER_GATE4_KEYS='4,2,ctrl-s' \
		$(PYTHON) tools/gate4_smoke.py

# Gate 4 GUI portal slice: run a static XLSX read/stage/commit fixture against
# the real workbook grant. This exercises the GUI feature without the Wayland
# display path. The windowed GridCraft target consumes the same feature and
# passes GATE4_GUI_NAMESPACE as argv.
gate4-gui-portal-test:
	$(MAKE) linux-fixture
	sh tools/build-appimage-fixture.sh $(GATE4_GUI_APPIMAGE) $(GATE4_GUI_APPIMAGE_RECORD) \
		$(LINUX_RUNTIME_SEED) $(GATE4_GUI_APP_ELF)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 KERNEL_FEATURES=gate4-gui-test \
		LINUX_APPIMAGE_PROGRAM=/usr/bin/app \
		LINUX_APPIMAGE_ARGS='$(GATE4_GUI_NAMESPACE)' \
		STAGED_APPIMAGE=$(GATE4_GUI_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(GATE4_GUI_APPIMAGE_RECORD) image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK \
	AETHER_EXTRA_MARKERS=AETHER_GATE4_PORTAL_OK,AETHER_GATE4_SAVE_OK,AETHER_GATE4_REVOKE_OK \
	AETHER_REQUIRE_TEXT='portal: seeded Gate 4 GUI workbook grant|id=5b428ae53860eb72|AETHER_GATE4_PORTAL_OK path=/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx read_bytes=4089|AETHER_GATE4_SAVE_OK file=/home/gate4-gui.xlsx bytes=4089 staging=sctmp+rename source=kernel|AETHER_GATE4_REVOKE_OK path=/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx read_denials=1 write_denials=1' \
	$(PYTHON) tools/test-boot.py

# Gate 4 display endpoint slice: the static Linux fixture opens
# /dev/aether-display, presents a memfd-backed pattern, and receives a
# synthetic input event routed through the real PS/2/desktop path. This is the
# display endpoint only; it does not run the real GUI or Mesa.
gate4-display-test: image
	$(PYTHON) tools/test-boot.py

shell-test: image
	$(PYTHON) tools/test-shell.py

iso:
	sh tools/make-iso.sh

# Prove the distribution ISO follows the El Torito UEFI path used by VMware.
iso-test: iso
	$(PYTHON) tools/test-iso.py

# Generate a VMware Workstation VM definition for the newest ISO.
vmware-vmx:
	sh tools/make-vmware-vmx.sh

fault-test:
	$(MAKE) KERNEL_FEATURES=fault-selftest image
	AETHER_EXPECT_FAULT=1 $(PYTHON) tools/test-boot.py

# Boot with the rollback self-test feature and require its own marker on top
# of the standard set, so the lost-image branch is a boot gate.
rollback-test:
	$(MAKE) KERNEL_FEATURES=recovery-rollback-selftest image
	AETHER_EXTRA_MARKERS=AETHER_RECOVERY_ROLLBACK_OK $(PYTHON) tools/test-boot.py

# Build a kernel that deliberately fails after paging and require the normal
# harness to detect the failure marker. This checks the CI gate itself, not
# just the boot path.
negative-test:
	$(MAKE) KERNEL_FEATURES=fault-selftest image
	AETHER_EXPECT_BOOT_FAILURE=1 $(PYTHON) tools/test-boot.py

# Prove the loader-staged AppImage is verified before parsing: mutate the
# container on the boot volume while leaving the signed record unchanged, then
# require the record/digest check to fail closed. No parse or launch may occur.
appimage-stage-tamper-test: image
	@cp $(LINUX_APPIMAGE) $(TAMPERED_APPIMAGE)
	@$(PYTHON) -c 'import sys; p=sys.argv[1]; d=bytearray(open(p,"rb").read()); d[-1]^=0xff; open(p,"wb").write(d)' $(TAMPERED_APPIMAGE)
	@$(MAKE) STAGED_APPIMAGE=$(TAMPERED_APPIMAGE) image
	@set -e; trap '$(MAKE) image >/dev/null' EXIT; \
		AETHER_EXPECT_BOOT_FAILURE=1 \
		AETHER_EXPECT_FAILURE_TEXT="AETHER_LINUX_APPIMAGE_STAGE_FAILED reason=verify" \
		$(PYTHON) tools/test-boot.py

# A missing staged AppImage must fail in the loader, before the kernel can
# import or launch anything.
appimage-stage-missing-test: image
	@set -e; trap '$(MAKE) image >/dev/null' EXIT; \
		mdel -i $(IMAGE) ::/linux/app.AppImage; \
		AETHER_EXPECT_BOOT_FAILURE=1 \
		AETHER_EXPECT_FAILURE_TEXT="AETHER_LOADER_FAILED error=staged boot file" \
		$(PYTHON) tools/test-boot.py

unittest:
	$(CARGO) test --locked
	$(PYTHON) -m unittest discover -s tools -p 'test_tooling.py'

compat-test:
	$(CARGO) test --locked -p aether-compat

appimage-test:
	$(CARGO) test --locked -p aether-appimage

squashfs-test:
	$(CARGO) test --locked -p aether-squashfs

# Package and update policy tests share the signed test fixtures under
# tests/fixtures; run both crates together.
update-test:
	$(CARGO) test --locked -p aether-packaging -p aether-update

docs-check:
	$(PYTHON) tools/check-docs.py

license-check:
	$(PYTHON) tools/check-licenses.py

lint: kernel
	$(CARGO) fmt --all --check
	$(CARGO) clippy --locked --all-targets -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		AETHER_USER_ELF=$(abspath $(USER_ELF)) \
		AETHER_LINUXD_ELF=$(abspath $(LINUXD_ELF)) \
		AETHER_LINUX_ELF=$(abspath $(LINUX_ELF)) \
		AETHER_LINUX_ROOTFS=$(abspath $(LINUX_ROOTFS)) \
		AETHER_LINUX_ROOTFS_RECORD=$(abspath $(LINUX_ROOTFS_RECORD)) \
		AETHER_LINUX_RUNTIME_PIN="$$(cat $(LINUX_RUNTIME_PIN) 2>/dev/null || true)" \
		AETHER_LINUX_APPIMAGE_PROGRAM=$(LINUX_APPIMAGE_PROGRAM) \
		AETHER_LINUX_APPIMAGE_ARGS='$(LINUX_APPIMAGE_ARGS)' \
		AETHER_LINUX_DYN_PATH=$(LINUX_DYN_PATH) \
		$(CARGO) clippy -p aether-kernel --release --target $(KERNEL_TARGET) --locked -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		AETHER_USER_ELF=$(abspath $(USER_ELF)) \
		AETHER_LINUXD_ELF=$(abspath $(LINUXD_ELF)) \
		AETHER_LINUX_ELF=$(abspath $(LINUX_ELF)) \
		AETHER_LINUX_ROOTFS=$(abspath $(LINUX_ROOTFS)) \
		AETHER_LINUX_ROOTFS_RECORD=$(abspath $(LINUX_ROOTFS_RECORD)) \
		AETHER_LINUX_RUNTIME_PIN="$$(cat $(LINUX_RUNTIME_PIN) 2>/dev/null || true)" \
		AETHER_LINUX_APPIMAGE_PROGRAM=$(LINUX_APPIMAGE_PROGRAM) \
		AETHER_LINUX_APPIMAGE_ARGS='$(LINUX_APPIMAGE_ARGS)' \
		AETHER_LINUX_DYN_PATH=$(LINUX_DYN_PATH) \
		$(CARGO) clippy -p aether-kernel --release --target $(KERNEL_TARGET) \
			--features fault-selftest --locked -- -D warnings
	RUSTFLAGS="$(USER_RUSTFLAGS)" \
		$(CARGO) clippy -p aether-sdk -p aether-init --release --target $(KERNEL_TARGET) --locked -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		$(CARGO) clippy -p aether-bootloader --release --target $(LOADER_TARGET) --locked -- -D warnings

# Signed release bundle. Requires AETHER_SIGNING_KEY and AETHER_PUBLISHER;
# unsigned releases are refused.
release:
	sh tools/release.sh

# Build and verify a release bundle with the checked-in test key. This proves
# the mechanism, not a release trust root.
release-demo:
	rm -rf $(BUILD)/release-demo
	AETHER_SIGNING_KEY=tests/fixtures/keys/test.seed \
	AETHER_PUBLISHER="Aether Test Publisher" \
	AETHER_VERSION=0.2.0 \
	AETHER_RELEASE_DIR=$(abspath $(BUILD)/release-demo) \
		sh tools/release.sh
	AETHER_TRUST="Aether Test Publisher:$$(cat tests/fixtures/keys/test.pub)" \
	AETHER_RELEASE_DIR=$(abspath $(BUILD)/release-demo) \
		sh tools/release-check.sh
	# The tamper test proves the verify step above detects a mutated
	# bundle; it must fail with the named cross-check or image digest error.
	AETHER_TRUST="Aether Test Publisher:$$(cat tests/fixtures/keys/test.pub)" \
	AETHER_BUNDLE=$(abspath $(BUILD)/release-demo) \
		sh tools/release-tamper-test.sh

release-check:
	sh tools/release-check.sh

qualify:
	$(PYTHON) tools/qualify.py

ci:
	sh tools/ci.sh

clean:
	$(CARGO) clean
	rm -rf $(BUILD)
