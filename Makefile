# SPDX-License-Identifier: Apache-2.0

SHELL := /bin/sh

CARGO ?= cargo
PYTHON ?= python3

BUILD := build
KERNEL_ELF := $(BUILD)/aether-kernel.elf
LOADER_EFI := $(BUILD)/BOOTX64.EFI
IMAGE := $(BUILD)/aether.img

KERNEL_TARGET := x86_64-unknown-none
LOADER_TARGET := x86_64-unknown-uefi

AETHER_GIT_COMMIT := $(shell git rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
export AETHER_GIT_COMMIT

.PHONY: all kernel loader image test unittest lint ci clean

all: image

kernel:
	@mkdir -p $(BUILD)
	$(CARGO) build -p aether-kernel --release --target $(KERNEL_TARGET) --locked
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

unittest:
	$(CARGO) test --locked

lint: kernel
	$(CARGO) fmt --all --check
	$(CARGO) clippy --locked -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		$(CARGO) clippy -p aether-kernel --release --target $(KERNEL_TARGET) --locked -- -D warnings
	AETHER_KERNEL_ELF=$(abspath $(KERNEL_ELF)) \
		$(CARGO) clippy -p aether-bootloader --release --target $(LOADER_TARGET) --locked -- -D warnings

ci:
	sh tools/ci.sh

clean:
	$(CARGO) clean
	rm -rf $(BUILD)
