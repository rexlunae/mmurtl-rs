/* Preserve compiler KCFI trap records and expose their runtime bounds. */
SECTIONS {
    .kcfi_traps : ALIGN(4) {
        __kcfi_traps_start = .;
        KEEP(*(.kcfi_traps))
        __kcfi_traps_end = .;
    }
} INSERT AFTER .rodata;
