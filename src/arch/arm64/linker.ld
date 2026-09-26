/* MMURTL/RS arm64 kernel layout (QEMU virt: RAM at 0x4000_0000).
 *
 * The kernel is linked and loaded at RAM + 2 MiB, leaving the start of RAM
 * for the device tree blob QEMU places there when booting an ELF. It runs
 * identity-mapped (VA == PA), so these are also physical addresses.
 */
ENTRY(_start)

SECTIONS
{
    . = 0x40200000;
    __kernel_start = .;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
        *(.got .got.*)
    }

    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* Boot CPU stack (64 KiB) */
    . = ALIGN(4096);
    __boot_stack_bottom = .;
    . += 0x10000;
    __boot_stack_top = .;

    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame*)
        *(.note*)
    }
}
