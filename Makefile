TARGET = x86_64-unknown-none.json
KERNEL_BIN = target/x86_64-unknown-none/release/mmurtl-rs
BIOS_IMG = target/mmurtl-rs-bios.img
UEFI_IMG = target/mmurtl-rs-uefi.img

# arm64 port: a bootable ELF for QEMU virt (no image step needed)
ARM64_TARGET = aarch64-unknown-none-softfloat
ARM64_KERNEL = target/$(ARM64_TARGET)/release/mmurtl-rs
ARM64_IMAGE = target/mmurtl-rs-arm64.Image
ARM64_SMP ?= 4
ARM64_GIC ?= 3

.PHONY: all build bios uefi run-bios run-uefi arm64 arm64-image run-arm64 run-rpi3 user user-arm64 disk disk-arm64 clean

all: build

# Build the amd64 kernel ELF
build:
	cargo build -Z build-std=core,compiler_builtins,alloc -Z json-target-spec \
		--target $(TARGET) \
		--release

# Create BIOS and UEFI boot images. The dependency patch keeps the
# bootloader's stage builds compiling on the pinned nightly (see the script).
bios uefi: build
	./tools/patch-bootloader-deps.sh
	cd tools/image-builder && CARGO_BUILD_STD="" CARGO_BUILD_STD_FEATURES="" \
		cargo run --release \
		--target-dir target \
		-- \
		../../$(KERNEL_BIN) \
		../../$(BIOS_IMG) \
		../../$(UEFI_IMG)

# Run in QEMU
run-bios: bios
	qemu-system-x86_64 \
		-drive format=raw,file=$(BIOS_IMG) \
		-serial stdio \
		-m 256M

run-uefi: uefi
	qemu-system-x86_64 \
		-bios /usr/share/ovmf/OVMF.fd \
		-drive format=raw,file=$(UEFI_IMG) \
		-serial stdio \
		-m 256M

# Build the arm64 kernel ELF
arm64:
	cargo build -Z build-std=core,compiler_builtins,alloc \
		--target $(ARM64_TARGET) \
		--release

# arm64 Linux-style "Image" (raw binary with the standard header), for
# U-Boot booti, firmware, or QEMU -kernel. llvm-objcopy comes with the
# toolchain's llvm-tools component.
LLVM_BIN = $(shell rustc --print sysroot)/lib/rustlib/$(shell rustc -vV | sed -n 's/host: //p')/bin
arm64-image: arm64
	$(LLVM_BIN)/llvm-objcopy -O binary $(ARM64_KERNEL) $(ARM64_IMAGE)

# Run the arm64 kernel on QEMU virt (GICv$(ARM64_GIC), $(ARM64_SMP) CPUs; GICv2
# works too, up to 8 CPUs). Add a disk
# and NIC with e.g.:
#   -drive if=none,format=raw,file=disk.img,id=hd0 -device virtio-blk-device,drive=hd0
#   -netdev user,id=n0 -device virtio-net-device,netdev=n0
run-arm64: arm64
	qemu-system-aarch64 \
		-machine virt,gic-version=$(ARM64_GIC) \
		-cpu cortex-a72 \
		-smp $(ARM64_SMP) \
		-m 256M \
		-nographic \
		-kernel $(ARM64_KERNEL)

# Run on QEMU's Raspberry Pi 3 (BCM2837: 4 cores, spin-table SMP, no GIC).
# QEMU has no device tree for it: use the Pi firmware's, from
# https://github.com/raspberrypi/firmware/raw/master/boot/bcm2710-rpi-3-b.dtb
RPI3_DTB ?= bcm2710-rpi-3-b.dtb
run-rpi3: arm64-image
	qemu-system-aarch64 \
		-machine raspi3b \
		-nographic \
		-kernel $(ARM64_IMAGE) \
		-dtb $(RPI3_DTB)

# User programs (Rust, ELF) for the kernel to load from /BIN on its disk.
# amd64 uses the large code model: the user window sits above 2 GiB.
user:
	cd user && RUSTFLAGS="-C code-model=large -C relocation-model=static" \
		cargo build -Z build-std=core --target x86_64-unknown-none --release

user-arm64:
	cd user && RUSTFLAGS="-C relocation-model=static" \
		cargo build -Z build-std=core --target aarch64-unknown-none-softfloat --release

# exFAT disk images with the programs in /BIN (needs root for the loop device)
disk: user
	./tools/make-disk.sh disk-amd64.img x86_64-unknown-none

disk-arm64: user-arm64
	./tools/make-disk.sh disk-arm64.img aarch64-unknown-none-softfloat

# Run with debug symbols
run-debug: build
	bootimage runner --target $(TARGET) -- \
		-serial stdio \
		-m 256M

clean:
	cargo clean
	rm -f $(BIOS_IMG) $(UEFI_IMG)
