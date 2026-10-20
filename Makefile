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
LINUX_RUNTIME_DIR := $(BUILD)/linux-runtime-pinned-root
LINUX_ROOTFS := $(BUILD)/linux-runtime.sqfs
LINUX_ROOTFS_RECORD := $(BUILD)/linux-runtime.import
LINUX_RUNTIME_PIN := $(BUILD)/linux-runtime-pinned.pin
LINUX_RUNTIME_MANIFEST := $(BUILD)/linux-runtime-pinned.manifest
LINUX_APPIMAGE := $(BUILD)/linux-user.AppImage
LINUX_APPIMAGE_RECORD := $(BUILD)/linux-user.import
LINUX_APPIMAGE_PROGRAM ?= /usr/bin/app
LINUX_RUNTIME_SEED := tests/fixtures/keys/test.seed
LINUX_DYN_PATH ?= /usr/bin/dynamic
LINUX_RUNTIME_PREBUILT ?= 0
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

.PHONY: all kernel user loader image iso iso-test vmware-vmx test glibc-test shell-test fault-test rollback-test \
	negative-test unittest lint docs-check license-check compat-test update-test release release-demo \
	runtime-rootfs-pinned runtime-package-test release-check qualify ci clean

all: image

user:
	@mkdir -p $(BUILD)
	RUSTFLAGS="$(USER_RUSTFLAGS)" \
		$(CARGO) build -p aether-init --release --target $(KERNEL_TARGET) --locked
	cp target/$(KERNEL_TARGET)/release/aether-init $(USER_ELF)
	cp target/$(KERNEL_TARGET)/release/aether-linuxd $(LINUXD_ELF)

linux-fixture:
	@mkdir -p $(BUILD)
	$(AS) --64 tests/fixtures/linux/hello.S -o $(BUILD)/linux-hello.o
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
	AETHER_LINUX_APPIMAGE=$(abspath $(LINUX_APPIMAGE)) \
	AETHER_LINUX_APPIMAGE_RECORD=$(abspath $(LINUX_APPIMAGE_RECORD)) \
	AETHER_LINUX_APPIMAGE_PROGRAM=$(LINUX_APPIMAGE_PROGRAM) \
	AETHER_LINUX_DYN_PATH=$(LINUX_DYN_PATH) \
		$(CARGO) build -p aether-kernel --release --target $(KERNEL_TARGET) --locked $(KERNEL_FEATURE_FLAGS)
	cp target/$(KERNEL_TARGET)/release/aether-kernel $(KERNEL_ELF)

loader: kernel
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		$(CARGO) build -p aether-bootloader --release --target $(LOADER_TARGET) --locked
	cp target/$(LOADER_TARGET)/release/aether-bootloader.efi $(LOADER_EFI)

image: loader
	truncate -s 64M $(IMAGE)
	mformat -i $(IMAGE) -F -v AETHER ::
	mmd -i $(IMAGE) ::/EFI ::/EFI/BOOT
	mcopy -i $(IMAGE) $(LOADER_EFI) ::/EFI/BOOT/BOOTX64.EFI
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
		AETHER_LINUX_APPIMAGE=$(abspath $(LINUX_APPIMAGE)) \
		AETHER_LINUX_APPIMAGE_RECORD=$(abspath $(LINUX_APPIMAGE_RECORD)) \
		AETHER_LINUX_APPIMAGE_PROGRAM=$(LINUX_APPIMAGE_PROGRAM) \
		AETHER_LINUX_DYN_PATH=$(LINUX_DYN_PATH) \
		$(CARGO) clippy -p aether-kernel --release --target $(KERNEL_TARGET) --locked -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		AETHER_USER_ELF=$(abspath $(USER_ELF)) \
		AETHER_LINUXD_ELF=$(abspath $(LINUXD_ELF)) \
		AETHER_LINUX_ELF=$(abspath $(LINUX_ELF)) \
		AETHER_LINUX_ROOTFS=$(abspath $(LINUX_ROOTFS)) \
		AETHER_LINUX_ROOTFS_RECORD=$(abspath $(LINUX_ROOTFS_RECORD)) \
		AETHER_LINUX_RUNTIME_PIN="$$(cat $(LINUX_RUNTIME_PIN) 2>/dev/null || true)" \
		AETHER_LINUX_APPIMAGE=$(abspath $(LINUX_APPIMAGE)) \
		AETHER_LINUX_APPIMAGE_RECORD=$(abspath $(LINUX_APPIMAGE_RECORD)) \
		AETHER_LINUX_APPIMAGE_PROGRAM=$(LINUX_APPIMAGE_PROGRAM) \
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
