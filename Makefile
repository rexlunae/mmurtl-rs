TARGET = x86_64-unknown-none.json
KERNEL_BIN = target/x86_64-unknown-none/release/mmurtl-rs
BIOS_IMG = target/mmurtl-rs-bios.img
UEFI_IMG = target/mmurtl-rs-uefi.img

# arm64 port: a bootable ELF for QEMU virt (no image step needed)
ARM64_TARGET = aarch64-unknown-none-softfloat
ARM64_KERNEL = target/$(ARM64_TARGET)/release/mmurtl-rs
ARM64_SMP ?= 4
ARM64_GIC ?= 3

.PHONY: all build bios uefi run-bios run-uefi arm64 run-arm64 clean

all: build

# Build the amd64 kernel ELF
build:
	cargo build -Z build-std=core,compiler_builtins,alloc -Z json-target-spec \
		--target $(TARGET) \
		--release

# Create BIOS and UEFI boot images
bios uefi: build
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

# Run with debug symbols
run-debug: build
	bootimage runner --target $(TARGET) -- \
		-serial stdio \
		-m 256M

clean:
	cargo clean
	rm -f $(BIOS_IMG) $(UEFI_IMG)
