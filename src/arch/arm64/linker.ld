/* MMURTL/RS arm64 kernel layout.
 *
 * Linked as a position-independent executable at 0x4020_0000 (QEMU virt:
 * RAM + 2 MiB, leaving the start of RAM for the device tree QEMU places
 * there when booting an ELF). It can be loaded anywhere 4 KiB-aligned:
 * `_start` applies the R_AARCH64_RELATIVE relocations in .rela.dyn for
 * wherever it landed, then runs identity-mapped (VA == PA).
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

    /* Dynamic relocations applied by _start (all R_AARCH64_RELATIVE) */
    .rela.dyn : ALIGN(8) {
        __rela_start = .;
        *(.rela.dyn .rela.*)
        __rela_end = .;
    }

    .dynamic : { *(.dynamic) }
    .dynsym : { *(.dynsym) }
    .dynstr : { *(.dynstr) }
    .gnu.hash : { *(.gnu.hash) }
    .hash : { *(.hash) }

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

    /* For the arm64 Image header: bytes the loader must reserve */
    __image_size = __kernel_end - __kernel_start;

    /* boot.rs computes the load delta against this */
    ASSERT(__kernel_start == 0x40200000, "LINK_BASE in boot.rs must match")

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame*)
        *(.note*)
    }
}
