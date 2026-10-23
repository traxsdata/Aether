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
USERLAND_ELF := $(BUILD)/aether-userland.elf
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
NET_TEST_PORT ?= 18080
NET_APP_ELF := $(BUILD)/linux-net-client.elf
NET_APPIMAGE := $(BUILD)/linux-net-client.AppImage
NET_APPIMAGE_RECORD := $(BUILD)/linux-net-client.import
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
# Optional | separated NAME=value entries appended to the program environment.
LINUX_APPIMAGE_ENV ?=
# Signed app id that receives the configured argv/environment. Empty keeps the
# GridCraft-only rule, so a multi-app pack never leaks the portal workbook path.
LINUX_APPIMAGE_ARGS_APP ?=
# Experimental Gate 3 headless GridCraft run.
GATECRAFT_APPIMAGE ?= /tmp/opencode/gridcraft.AppImage
GATE3_APP_PROGRAM ?= /usr/bin/gridcraft-cli
GATE3_APP_ARGS ?= run|--in|/usr/share/gridcraft/gate3-in.csv|--cmd|formulas.evaluate={"formula":"=1+1"}|--cmd|cell.set={"cell":"A4","input":"total"}|--cmd|cell.set={"cell":"B4","input":"=SUM(B2:B3)"}|--out|/tmp/gate3-out.csv|--print|A1:B4|--csv
GATE3_RUNTIME_VERSION ?= $(shell getconf GNU_LIBC_VERSION | tr ' ' '-')
# Headless PhotoCraft (Craft app) through the same Gate 3 glibc runtime.
PHOTOCRAFT_APPIMAGE ?= /tmp/opencode/photocraft-0.12.0-linux-x86_64.AppImage
PHOTOCRAFT_PIN := da821339afce93b72991c7900a28f589eb06a885e0ab59ea702ddec9a280ac66
PHOTOCRAFT_APP_PROGRAM ?= /usr/bin/photocraft-cli
PHOTOCRAFT_APP_ARGS ?= run|/usr/share/photocraft/photo-in.png|--cmd|image.imageSize|--params|{"width":4,"height":4}|--out|/tmp/photocraft-out.png|--format|png
# Generic Craft-app smoke/GUI tests. The caller supplies the pinned artifact
# and the identity from its signed import record.
CRAFT_ID ?=
CRAFT_VERSION ?=
CRAFT_APPIMAGE ?=
CRAFT_PIN ?=
CRAFT_PROGRAM ?= /usr/bin/app
CRAFT_ARGS ?=
CRAFT_REQUIRE_TEXT ?=
CRAFT_IMAGE_SIZE ?= 192M
CRAFT_MEMORY ?= 3G
CRAFT_TIMEOUT ?= 300
CRAFT_GUI_TIMEOUT ?= 600
CRAFT_OUT ?= $(BUILD)/craft-app
CRAFT_GUI_SCREENSHOT ?= $(CRAFT_OUT)/gui.ppm
# Firefox ESR staging (firefox-stage) and the milestone 1 headless discovery
# run (firefox-headless-test). The pin lives in tools/pins/firefox-esr.pin.
FIREFOX_OUT ?= $(BUILD)/firefox
FIREFOX_ID ?= org.mozilla.firefox
FIREFOX_STAGE_FLAGS ?=
# firefox-bin is the browser; `firefox` is a launcher that execs it.
FIREFOX_PROGRAM ?= /opt/firefox/firefox-bin
FIREFOX_ARGS ?= --headless|--screenshot|/tmp/out.png|file:///usr/share/firefox-test/index.html
FIREFOX_ENV ?= MOZ_HEADLESS=1|MOZ_DISABLE_CONTENT_SANDBOX=1|MOZ_DISABLE_GMP_SANDBOX=1|MOZ_DISABLE_RDD_SANDBOX=1|MOZ_DISABLE_SOCKET_PROCESS_SANDBOX=1|MOZ_FORCE_DISABLE_E10S=153.5.1|MOZ_CRASHREPORTER_DISABLE=1
# large-apps: Linux-sized fd/thread/mapping limits; gate4-runtime: the 1.5 GiB
# kernel heap that holds the imported trees.
FIREFOX_FEATURES ?= large-apps gate4-runtime
FIREFOX_MEMORY ?= 6G
FIREFOX_TIMEOUT ?= 1800
FIREFOX_IMAGE_SIZE ?= 448M
# Full Craft app suite staged for the multi-app ISO.
CRAFT_APPS_DIR ?= $(BUILD)/craft-apps
CRAFT_APP_CACHE ?= /tmp/opencode/craft-apps
CRAFT_ISO_IMAGE_SIZE ?= 1536M
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
# The suite ISO carries its own signed app pack, so it can build the runtime
# without signing the single GridCraft demo artifact.
GATE4_RUNTIME_SKIP_APP ?= 0
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

.PHONY: all kernel user loader image iso iso-gridcraft craft-apps-stage iso-craft-apps iso-test vmware-vmx test large-apps-test glibc-test gate3-test photocraft-test craft-smoke-test craft-gui-test firefox-stage firefox-headless-test gate4-portal-test gate4-gui-portal-test gate4-display-test gate4-runtime gate4-runtime-image gate4-image-only gate4-runtime-test gate4-test shell-test terminal-test net-test fault-test usercopy-fault-test smap-fault-test crash-screen-test rollback-test path-length-test \
	negative-test appimage-stage-tamper-test appimage-stage-missing-test unittest lint docs-check license-check compat-test update-test release release-demo \
	runtime-rootfs-pinned runtime-package-test release-check qualify ci clean

all: image

# Bootable demo ISO: the signed GridCraft AppImage, the Gate 4 Mesa/lavapipe
# runtime rootfs, and the windowed GUI launch path. This is a QEMU/VMware
# prototype image, not a general Linux compatibility release.
iso-gridcraft: gate4-runtime
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
	AETHER_ISO_SKIP_IMAGE=1 \
		EFI_IMAGE_SIZE=128M \
		ISO_PREFIX=aether-gridcraft \
		STAGED_APPIMAGE=$(GATECRAFT_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(GATE4_APPIMAGE_RECORD) \
		STAGED_RUNTIME_ROOTFS=$(GATE4_RUNTIME_ROOTFS) \
		STAGED_RUNTIME_RECORD=$(GATE4_RUNTIME_RECORD) \
		sh tools/make-iso.sh

# Verify, sign, and pack every pinned Craft AppImage under
# build/craft-apps/. The download cache can be reused across runs.
craft-apps-stage:
	$(PYTHON) tools/stage-craft-apps.py \
		--cache $(CRAFT_APP_CACHE) --out $(CRAFT_APPS_DIR)

# Bootable Craft suite ISO. The kernel imports the selected app on demand from
# the boot-volume pack; the desktop launches apps from Applications and keeps
# two app domains alive at once.
iso-craft-apps: craft-apps-stage
	$(MAKE) gate4-runtime GATE4_RUNTIME_SKIP_APP=1
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		KERNEL_FEATURES="gate4-runtime gate4-gui-test" \
		LINUX_RUNTIME_PIN=$(GATE4_RUNTIME_PIN) \
		STAGED_RUNTIME_ROOTFS=$(GATE4_RUNTIME_ROOTFS) \
		STAGED_RUNTIME_RECORD=$(GATE4_RUNTIME_RECORD) \
		LINUX_APPIMAGE_PROGRAM=$(GATE4_GUI_PROGRAM) \
		LINUX_APPIMAGE_ARGS='$(GATE4_GUI_ARGS)' \
		loader
	AETHER_ISO_SKIP_IMAGE=1 \
		EFI_IMAGE_SIZE=$(CRAFT_ISO_IMAGE_SIZE) \
		ISO_PREFIX=aether-craft-apps \
		STAGED_RUNTIME_ROOTFS=$(GATE4_RUNTIME_ROOTFS) \
		STAGED_RUNTIME_RECORD=$(GATE4_RUNTIME_RECORD) \
		STAGED_CRAFT_APPS_DIR=$(CRAFT_APPS_DIR) \
		sh tools/make-iso.sh

user:
	@mkdir -p $(BUILD)
	RUSTFLAGS="$(USER_RUSTFLAGS)" \
		$(CARGO) build -p aether-init --release --target $(KERNEL_TARGET) --locked
	cp target/$(KERNEL_TARGET)/release/aether-init $(USER_ELF)
	cp target/$(KERNEL_TARGET)/release/aether-linuxd $(LINUXD_ELF)
	# The kernel embeds the userland, so leave its debug info behind.
	objcopy --strip-debug target/$(KERNEL_TARGET)/release/aether-userland $(USERLAND_ELF) 2>/dev/null \
		|| cp target/$(KERNEL_TARGET)/release/aether-userland $(USERLAND_ELF)

FIXTURE_GATE4_GUI := $(if $(findstring gate4-gui-test,$(KERNEL_FEATURES)),--defsym AETHER_GATE4_GUI=1)
FIXTURE_LARGE_APPS := $(if $(findstring large-apps,$(KERNEL_FEATURES)),--defsym AETHER_LARGE_APPS=1)

linux-fixture:
	@mkdir -p $(BUILD)
	$(AS) --64 $(FIXTURE_GATE4_GUI) $(FIXTURE_LARGE_APPS) tests/fixtures/linux/hello.S -o $(BUILD)/linux-hello.o
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
	$(AS) --64 --defsym AETHER_NET_PORT=$(NET_TEST_PORT) tests/fixtures/linux/net-client.S \
		-o $(BUILD)/linux-net-client.o
	$(LD) -m elf_x86_64 -static -no-pie -T tests/fixtures/linux/linker.ld \
		$(BUILD)/linux-net-client.o -o $(NET_APP_ELF)
	if [ "$(LINUX_RUNTIME_PREBUILT)" = "0" ]; then \
		sh tools/build-pinned-runtime.sh $(LINUX_ROOTFS) $(LINUX_ROOTFS_RECORD) \
			$(LINUX_RUNTIME_PIN) $(LINUX_RUNTIME_MANIFEST) $(LINUX_RUNTIME_SEED); \
	fi
	sh tools/build-appimage-fixture.sh $(LINUX_APPIMAGE) $(LINUX_APPIMAGE_RECORD) \
		$(LINUX_RUNTIME_SEED) $(LINUX_APP_ELF) memory=jit

kernel: user linux-fixture
	@mkdir -p $(BUILD)
	AETHER_USER_ELF=$(abspath $(USER_ELF)) \
	AETHER_LINUXD_ELF=$(abspath $(LINUXD_ELF)) \
	AETHER_USERLAND_ELF=$(abspath $(USERLAND_ELF)) \
	AETHER_LINUX_ELF=$(abspath $(LINUX_ELF)) \
	AETHER_LINUX_ROOTFS=$(abspath $(LINUX_ROOTFS)) \
	AETHER_LINUX_ROOTFS_RECORD=$(abspath $(LINUX_ROOTFS_RECORD)) \
	AETHER_LINUX_RUNTIME_PIN="$$(cat $(LINUX_RUNTIME_PIN) 2>/dev/null || true)" \
	AETHER_LINUX_APPIMAGE_PROGRAM=$(LINUX_APPIMAGE_PROGRAM) \
	AETHER_LINUX_APPIMAGE_ARGS='$(LINUX_APPIMAGE_ARGS)' \
	AETHER_LINUX_APPIMAGE_ENV='$(LINUX_APPIMAGE_ENV)' \
	AETHER_LINUX_APPIMAGE_ARGS_APP='$(LINUX_APPIMAGE_ARGS_APP)' \
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

# Headless PhotoCraft: the second Craft app, run unmodified from its signed
# AppImage through PT_INTERP and the Gate 3 glibc runtime. The CLI opens a
# fixture PNG, resizes it, and writes a new PNG to its private overlay. The
# kernel-owned markers prove the import and the sandbox; the guest output is
# matched only as evidence that the engine ran.
photocraft-test:
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 linux-fixture
	sh tools/build-gate3-runtime.sh $(LINUX_ROOTFS) $(LINUX_ROOTFS_RECORD) \
		$(LINUX_RUNTIME_PIN) $(LINUX_RUNTIME_MANIFEST) $(LINUX_RUNTIME_SEED)
	@mkdir -p $(BUILD)/gate3-runtime-root/usr/bin \
		$(BUILD)/gate3-runtime-root/usr/share/photocraft
	printf 'AETHER_LINUX_ROOT_OK source=/etc/motd\nAETHER_GATE3_ROOT_OK source=/etc/motd\n' \
		> $(BUILD)/gate3-runtime-root/etc/motd
	cp $(LINUX_INTERP_ELF) $(BUILD)/gate3-runtime-root/lib/ld-aether.so
	cp $(LINUX_DYN_ELF) $(BUILD)/gate3-runtime-root/usr/bin/dynamic
	cp $(LINUX_EXEC_ELF) $(BUILD)/gate3-runtime-root/usr/bin/exec
	cp $(LINUX_DYN_EXEC_ELF) $(BUILD)/gate3-runtime-root/usr/bin/dynamic-exec
	cp tests/fixtures/craft/photo-in.png \
		$(BUILD)/gate3-runtime-root/usr/share/photocraft/photo-in.png
	sh tools/build-runtime-rootfs.sh $(BUILD)/gate3-runtime-root \
		$(LINUX_ROOTFS) $(LINUX_RUNTIME_SEED) org.aether.linux-runtime \
		"$(GATE3_RUNTIME_VERSION)" "Aether Test Publisher" \
		$(LINUX_ROOTFS_RECORD)
	sh tools/build-craft-appimage.sh $(PHOTOCRAFT_APPIMAGE) \
		$(BUILD)/photocraft.import $(LINUX_RUNTIME_SEED) \
		ai.storyteller.photocraft 0.12.0 $(PHOTOCRAFT_PIN)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		LINUX_APPIMAGE_PROGRAM=$(PHOTOCRAFT_APP_PROGRAM) \
		LINUX_APPIMAGE_ARGS='$(PHOTOCRAFT_APP_ARGS)' \
		STAGED_APPIMAGE=$(PHOTOCRAFT_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(BUILD)/photocraft.import \
		IMAGE_SIZE=128M image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK \
	AETHER_EXTRA_MARKERS=AETHER_GATE3_APP_OK,AETHER_GATE3_DENIAL_OK \
	AETHER_MEMORY=2G \
	AETHER_REQUIRE_TEXT='AETHER_LINUX_APPIMAGE_STAGED_OK|bytes=80026104|AETHER_GATE3_APP_OK|AETHER_GATE3_DENIAL_OK|{"command":"image.imageSize","result":{"height":4,"width":4}}' \
	$(PYTHON) tools/test-boot.py

# Generic headless smoke test for a Craft app: stage a pinned AppImage, run its
# selected program in the Gate 3 glibc runtime, and require the kernel-owned
# boot and import evidence (AETHER_BOOT_OK is only printed after the kernel
# verified the imported app exited with status zero). The GridCraft gate
# markers are not required here, so any Craft app can pass. Append app output
# with CRAFT_REQUIRE_TEXT. Use `--version` (or the app's headless command)
# through CRAFT_ARGS.
craft-smoke-test:
	@test -n "$(CRAFT_ID)" || { echo "error: CRAFT_ID is required" >&2; exit 2; }
	@test -n "$(CRAFT_VERSION)" || { echo "error: CRAFT_VERSION is required" >&2; exit 2; }
	@test -f "$(CRAFT_APPIMAGE)" || { echo "error: CRAFT_APPIMAGE not found: $(CRAFT_APPIMAGE)" >&2; exit 2; }
	@test -n "$(CRAFT_PIN)" || { echo "error: CRAFT_PIN is required" >&2; exit 2; }
	@mkdir -p $(CRAFT_OUT)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 linux-fixture
	sh tools/build-gate3-runtime.sh $(LINUX_ROOTFS) $(LINUX_ROOTFS_RECORD) \
		$(LINUX_RUNTIME_PIN) $(LINUX_RUNTIME_MANIFEST) $(LINUX_RUNTIME_SEED)
	@mkdir -p $(BUILD)/gate3-runtime-root/usr/bin
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
	sh tools/build-craft-appimage.sh $(CRAFT_APPIMAGE) \
		$(CRAFT_OUT)/app.import $(LINUX_RUNTIME_SEED) \
		"$(CRAFT_ID)" "$(CRAFT_VERSION)" $(CRAFT_PIN)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		LINUX_APPIMAGE_PROGRAM=$(CRAFT_PROGRAM) \
		LINUX_APPIMAGE_ARGS='$(CRAFT_ARGS)' \
		LINUX_APPIMAGE_ARGS_APP='$(CRAFT_ID)' \
		STAGED_APPIMAGE=$(CRAFT_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(CRAFT_OUT)/app.import \
		IMAGE_SIZE=$(CRAFT_IMAGE_SIZE) image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK \
	AETHER_MEMORY=$(CRAFT_MEMORY) \
	AETHER_BOOT_TIMEOUT=$(CRAFT_TIMEOUT) \
	AETHER_REQUIRE_TEXT="AETHER_LINUX_APPIMAGE_STAGED_OK|bytes=$$(stat -c %s '$(CRAFT_APPIMAGE)')$(if $(CRAFT_REQUIRE_TEXT),|$(CRAFT_REQUIRE_TEXT))" \
	$(PYTHON) tools/test-boot.py

# Generic windowed GUI smoke test for a Craft app. Uses the Gate 4 Mesa/lavapipe
# runtime and Wayland bridge, signs the AppImage with memory=jit, waits for the
# kernel-owned present marker, and requires a non-black QEMU screenshot. It
# deliberately does not require the GridCraft portal save/revoke markers.
craft-gui-test: gate4-runtime
	@test -n "$(CRAFT_ID)" || { echo "error: CRAFT_ID is required" >&2; exit 2; }
	@test -n "$(CRAFT_VERSION)" || { echo "error: CRAFT_VERSION is required" >&2; exit 2; }
	@test -f "$(CRAFT_APPIMAGE)" || { echo "error: CRAFT_APPIMAGE not found: $(CRAFT_APPIMAGE)" >&2; exit 2; }
	@test -n "$(CRAFT_PIN)" || { echo "error: CRAFT_PIN is required" >&2; exit 2; }
	@mkdir -p $(CRAFT_OUT)
	sh tools/build-craft-appimage.sh $(CRAFT_APPIMAGE) \
		$(CRAFT_OUT)/app.import $(LINUX_RUNTIME_SEED) \
		"$(CRAFT_ID)" "$(CRAFT_VERSION)" $(CRAFT_PIN) memory=jit
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		KERNEL_FEATURES="gate4-runtime gate4-gui-test" \
		LINUX_RUNTIME_PIN=$(GATE4_RUNTIME_PIN) \
		STAGED_RUNTIME_ROOTFS=$(GATE4_RUNTIME_ROOTFS) \
		STAGED_RUNTIME_RECORD=$(GATE4_RUNTIME_RECORD) \
		LINUX_APPIMAGE_PROGRAM=$(CRAFT_PROGRAM) \
		LINUX_APPIMAGE_ARGS='$(CRAFT_ARGS)' \
		LINUX_APPIMAGE_ARGS_APP='$(CRAFT_ID)' \
		STAGED_APPIMAGE=$(CRAFT_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(CRAFT_OUT)/app.import \
		IMAGE_SIZE=$(CRAFT_IMAGE_SIZE) image
	AETHER_GATE4_WAIT_SAVE=0 \
	AETHER_GATE4_TIMEOUT=$(CRAFT_GUI_TIMEOUT) \
	AETHER_GATE4_SCREENSHOT=$(abspath $(CRAFT_GUI_SCREENSHOT)) \
	$(PYTHON) tools/gate4_smoke.py

# Firefox ESR (issue #118). `firefox-stage` downloads the pinned tarball once
# into build/firefox/cache (tools/pins/firefox-esr.pin), wraps it as an AppImage
# and signs a memory=jit import record. Neither target is part of `make ci`.
firefox-stage:
	$(PYTHON) tools/stage-firefox.py --out $(FIREFOX_OUT) $(FIREFOX_STAGE_FLAGS)

# Milestone 1 discovery run (issue #107): boot the staged Firefox headless in
# the Gate 3 glibc runtime with Linux-sized limits and try a screenshot of a
# page in the image. This does not pass yet; the serial log in
# build/serial.log records how far it got. See docs/reference/firefox.md.
firefox-headless-test:
	$(MAKE) firefox-stage FIREFOX_STAGE_FLAGS="--keep-root $(FIREFOX_STAGE_FLAGS)"
	$(MAKE) KERNEL_FEATURES="$(FIREFOX_FEATURES)" LINUX_RUNTIME_PREBUILT=1 linux-fixture
	sh tools/build-gate3-runtime.sh $(LINUX_ROOTFS) $(LINUX_ROOTFS_RECORD) \
		$(LINUX_RUNTIME_PIN) $(LINUX_RUNTIME_MANIFEST) $(LINUX_RUNTIME_SEED)
	@mkdir -p $(BUILD)/gate3-runtime-root/usr/bin
	printf 'AETHER_LINUX_ROOT_OK source=/etc/motd\nAETHER_GATE3_ROOT_OK source=/etc/motd\n' \
		> $(BUILD)/gate3-runtime-root/etc/motd
	cp $(LINUX_INTERP_ELF) $(BUILD)/gate3-runtime-root/lib/ld-aether.so
	cp $(LINUX_DYN_ELF) $(BUILD)/gate3-runtime-root/usr/bin/dynamic
	cp $(LINUX_EXEC_ELF) $(BUILD)/gate3-runtime-root/usr/bin/exec
	cp $(LINUX_DYN_EXEC_ELF) $(BUILD)/gate3-runtime-root/usr/bin/dynamic-exec
	sh tools/firefox-headless-root.sh $(FIREFOX_OUT)/root $(BUILD)/gate3-runtime-root
	sh tools/build-runtime-rootfs.sh $(BUILD)/gate3-runtime-root \
		$(LINUX_ROOTFS) $(LINUX_RUNTIME_SEED) org.aether.linux-runtime \
		"$(GATE3_RUNTIME_VERSION)" "Aether Test Publisher" \
		$(LINUX_ROOTFS_RECORD)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		KERNEL_FEATURES="$(FIREFOX_FEATURES)" \
		LINUX_APPIMAGE_PROGRAM=$(FIREFOX_PROGRAM) \
		LINUX_APPIMAGE_ARGS='$(FIREFOX_ARGS)' \
		LINUX_APPIMAGE_ENV='$(FIREFOX_ENV)' \
		LINUX_APPIMAGE_ARGS_APP=$(FIREFOX_ID) \
		STAGED_APPIMAGE=$(FIREFOX_OUT)/firefox.AppImage \
		STAGED_APPIMAGE_RECORD=$(FIREFOX_OUT)/firefox.import \
		IMAGE_SIZE=$(FIREFOX_IMAGE_SIZE) image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK \
	AETHER_MEMORY=$(FIREFOX_MEMORY) \
	AETHER_BOOT_TIMEOUT=$(FIREFOX_TIMEOUT) \
	AETHER_REQUIRE_TEXT="AETHER_LINUX_APPIMAGE_STAGED_OK|id=$(FIREFOX_ID)|bytes=$$(stat -c %s '$(FIREFOX_OUT)/firefox.AppImage')" \
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
	if [ "$(GATE4_RUNTIME_SKIP_APP)" = "0" ]; then \
		sh tools/build-gate3-appimage.sh $(GATECRAFT_APPIMAGE) \
			$(GATE4_APPIMAGE_RECORD) $(LINUX_RUNTIME_SEED); \
	fi
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
# The fixture compiles its display and quota tests out of this build
# (`.ifndef AETHER_GATE4_GUI`), so their markers are skipped.
gate4-gui-portal-test:
	$(MAKE) linux-fixture
	sh tools/build-appimage-fixture.sh $(GATE4_GUI_APPIMAGE) $(GATE4_GUI_APPIMAGE_RECORD) \
		$(LINUX_RUNTIME_SEED) $(GATE4_GUI_APP_ELF)
	$(MAKE) LINUX_RUNTIME_PREBUILT=1 KERNEL_FEATURES=gate4-gui-test \
		LINUX_APPIMAGE_PROGRAM=/usr/bin/app \
		LINUX_APPIMAGE_ARGS='$(GATE4_GUI_NAMESPACE)' \
		STAGED_APPIMAGE=$(GATE4_GUI_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(GATE4_GUI_APPIMAGE_RECORD) image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK,AETHER_LINUX_QUOTA_OK,AETHER_GATE4_DISPLAY_CREATE_OK,AETHER_GATE4_PRESENT_OK,AETHER_GATE4_INPUT_OK,AETHER_GATE4_GUEST_DISPLAY_OK \
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

# Network test, once per NIC model (e1000 and e1000e): DHCP, a kernel TCP
# fetch from the QEMU host, and a Linux app with network=client that waits for
# (test-answered) consent, fetches the same page through the socket API, and
# reads /etc/resolv.conf. The port is compiled into the kernel and fixture.
net-test:
	AETHER_NET_TEST_PORT=$(NET_TEST_PORT) $(MAKE) linux-fixture
	sh tools/build-appimage-fixture.sh $(NET_APPIMAGE) $(NET_APPIMAGE_RECORD) \
		$(LINUX_RUNTIME_SEED) $(NET_APP_ELF) network=client
	AETHER_NET_TEST_PORT=$(NET_TEST_PORT) $(MAKE) LINUX_RUNTIME_PREBUILT=1 \
		KERNEL_FEATURES="net-selftest net-consent-test" \
		STAGED_APPIMAGE=$(NET_APPIMAGE) \
		STAGED_APPIMAGE_RECORD=$(NET_APPIMAGE_RECORD) image
	AETHER_SKIP_MARKERS=AETHER_LINUX_APPIMAGE_EXEC_OK,AETHER_LINUX_OVERLAY_PERSIST_OK,AETHER_LINUX_OVERLAY_ERASE_OK \
	AETHER_NET_TEST_PORT=$(NET_TEST_PORT) $(PYTHON) tools/test-net.py

terminal-test: image
	$(PYTHON) tools/test-terminal.py

iso: image
	sh tools/make-iso.sh

# Prove the distribution ISO follows the El Torito UEFI path used by VMware.
iso-test: iso
	$(PYTHON) tools/test-iso.py

# Generate a VMware Workstation VM definition for the newest ISO.
vmware-vmx:
	sh tools/make-vmware-vmx.sh

# The full boot with Linux-sized limits for large applications: the quota
# fixture must still reach EMFILE at 1,024 descriptors and ENOMEM at 65,530
# mapping areas.
large-apps-test:
	$(MAKE) KERNEL_FEATURES=large-apps image
	$(PYTHON) tools/test-boot.py

fault-test:
	$(MAKE) KERNEL_FEATURES=fault-selftest image
	AETHER_EXPECT_FAULT=1 $(PYTHON) tools/test-boot.py

# A user-copy fault on a kernel-side address must halt, not be recovered as a
# user fault. The normal boot already proves the user-side fixup.
usercopy-fault-test:
	$(MAKE) KERNEL_FEATURES=usercopy-fault-selftest image
	AETHER_EXPECT_BOOT_FAILURE=1 \
		AETHER_EXPECT_FAILURE_TEXT='AETHER_BOOT_FAILED reason=exception vector=14' \
		$(PYTHON) tools/test-boot.py

# With SMAP on, a ring-0 read of a user page outside the copy routine must
# fault (error code 0x1: present page, supervisor read) and halt.
smap-fault-test:
	$(MAKE) KERNEL_FEATURES=smap-fault-selftest image
	AETHER_EXPECT_BOOT_FAILURE=1 \
		AETHER_EXPECT_FAILURE_TEXT='page fault (14) error=0x1' \
		$(PYTHON) tools/test-boot.py

# A fatal error must replace the screen with the crash report. The first boot
# page-faults in the kernel after the desktop is up (exception path, CPU
# state); the second panics right after graphics init (panic path).
crash-screen-test:
	$(MAKE) KERNEL_FEATURES=crash-screen-selftest image
	AETHER_EXPECT_BOOT_FAILURE=1 AETHER_EXPECT_CRASH_SCREEN=1 \
		AETHER_EXPECT_FAILURE_TEXT='AETHER_CRASH_SCREEN shown=1 reason=unhandled kernel exception: page fault' \
		$(PYTHON) tools/test-boot.py
	$(MAKE) KERNEL_FEATURES=crash-screen-panic-selftest image
	AETHER_EXPECT_BOOT_FAILURE=1 AETHER_EXPECT_CRASH_SCREEN=1 \
		AETHER_EXPECT_FAILURE_TEXT='AETHER_CRASH_SCREEN shown=1 reason=kernel panic: crash screen self-test' \
		$(PYTHON) tools/test-boot.py

# Boot with the rollback self-test feature and require its own marker on top
# of the standard set, so the lost-image branch is a boot gate.
rollback-test:
	$(MAKE) KERNEL_FEATURES=recovery-rollback-selftest image
	AETHER_EXTRA_MARKERS=AETHER_RECOVERY_ROLLBACK_OK $(PYTHON) tools/test-boot.py

# Prove the Linux path reader (issue #101) treats a copy fault as EFAULT, a
# no-terminator string as ENAMETOOLONG, and still reads a 300-byte path.
path-length-test:
	$(MAKE) KERNEL_FEATURES=path-length-selftest image
	AETHER_EXTRA_MARKERS=AETHER_PATH_LENGTH_OK $(PYTHON) tools/test-boot.py

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
		AETHER_USERLAND_ELF=$(abspath $(USERLAND_ELF)) \
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
		AETHER_USERLAND_ELF=$(abspath $(USERLAND_ELF)) \
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
	AETHER_ALLOW_TEST_TRUST=1 \
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
