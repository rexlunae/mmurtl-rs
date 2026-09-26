/* User programs are linked into the kernel's user window
 * (memory::user::USER_BASE = 0x6400_0000_0000), 48 MiB in. Every program
 * runs in its own address space, so they can all use the same range. */
ENTRY(_start)

SECTIONS
{
    . = 0x640003000000;
    /* Each output section starts on its own page, so every segment gets
     * exactly its own permissions (the loader enforces W^X per page).
     * The .l* inputs come from x86_64's large code model. */
    .text : ALIGN(4096) { *(.text._start) *(.text .text.*) *(.ltext .ltext.*) }
    .rodata : ALIGN(4096) { *(.rodata .rodata.*) *(.lrodata .lrodata.*) }
    .data : ALIGN(4096) { *(.data .data.*) *(.ldata .ldata.*) *(.got .got.*) }
    .bss : ALIGN(4096) { *(.bss .bss.*) *(.lbss .lbss.*) *(COMMON) }
    /DISCARD/ : { *(.comment) *(.eh_frame*) *(.note*) }
}
