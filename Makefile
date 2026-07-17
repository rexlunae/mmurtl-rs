TARGET = x86_64-unknown-none.json
KERNEL_BIN = target/x86_64-unknown-none/release/mmurtl-rs
BIOS_IMG = target/mmurtl-rs-bios.img
UEFI_IMG = target/mmurtl-rs-uefi.img

.PHONY: all build bios uefi run-bios run-uefi clean

all: build

# Build the kernel ELF
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

# Run with debug symbols
run-debug: build
	bootimage runner --target $(TARGET) -- \
		-serial stdio \
		-m 256M

clean:
	cargo clean
	rm -f $(BIOS_IMG) $(UEFI_IMG)
