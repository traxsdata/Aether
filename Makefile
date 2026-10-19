# SPDX-License-Identifier: Apache-2.0

SHELL := /bin/sh

CARGO ?= cargo
PYTHON ?= python3

BUILD := build
KERNEL_ELF := $(BUILD)/aether-kernel.elf
USER_ELF := $(BUILD)/aether-init.elf
LOADER_EFI := $(BUILD)/BOOTX64.EFI
IMAGE := $(BUILD)/aether.img

KERNEL_TARGET := x86_64-unknown-none
LOADER_TARGET := x86_64-unknown-uefi

AETHER_GIT_COMMIT := $(shell git rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
export AETHER_GIT_COMMIT

KERNEL_FEATURES ?=
KERNEL_FEATURE_FLAGS := $(if $(KERNEL_FEATURES),--features "$(KERNEL_FEATURES)")

# The native user program is linked at 2 TiB, outside the small code model.
# PIC keeps its internal references PC-relative; -no-pie keeps the image an
# ET_EXEC file with all relocations resolved at link time.
USER_RUSTFLAGS := -C relocation-model=pic -C link-arg=-no-pie

.PHONY: all kernel user loader image test fault-test negative-test unittest lint \
	docs-check license-check ci clean

all: image

user:
	@mkdir -p $(BUILD)
	RUSTFLAGS="$(USER_RUSTFLAGS)" \
		$(CARGO) build -p aether-init --release --target $(KERNEL_TARGET) --locked
	cp target/$(KERNEL_TARGET)/release/aether-init $(USER_ELF)

kernel: user
	@mkdir -p $(BUILD)
	AETHER_USER_ELF=$(abspath $(USER_ELF)) \
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
	$(PYTHON) tools/test-boot.py

fault-test:
	$(MAKE) KERNEL_FEATURES=fault-selftest image
	AETHER_EXPECT_FAULT=1 $(PYTHON) tools/test-boot.py

# Build a kernel that deliberately fails after paging and require the normal
# harness to detect the failure marker. This checks the CI gate itself, not
# just the boot path.
negative-test:
	$(MAKE) KERNEL_FEATURES=fault-selftest image
	AETHER_EXPECT_BOOT_FAILURE=1 $(PYTHON) tools/test-boot.py

unittest:
	$(CARGO) test --locked

docs-check:
	$(PYTHON) tools/check-docs.py

license-check:
	$(PYTHON) tools/check-licenses.py

lint: kernel
	$(CARGO) fmt --all --check
	$(CARGO) clippy --locked -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		AETHER_USER_ELF=$(abspath $(USER_ELF)) \
		$(CARGO) clippy -p aether-kernel --release --target $(KERNEL_TARGET) --locked -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		AETHER_USER_ELF=$(abspath $(USER_ELF)) \
		$(CARGO) clippy -p aether-kernel --release --target $(KERNEL_TARGET) \
			--features fault-selftest --locked -- -D warnings
	RUSTFLAGS="$(USER_RUSTFLAGS)" \
		$(CARGO) clippy -p aether-sdk -p aether-init --release --target $(KERNEL_TARGET) --locked -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		$(CARGO) clippy -p aether-bootloader --release --target $(LOADER_TARGET) --locked -- -D warnings

ci:
	sh tools/ci.sh

clean:
	$(CARGO) clean
	rm -rf $(BUILD)
