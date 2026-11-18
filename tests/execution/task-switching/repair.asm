; Standalone 32-bit Multiboot payload. Assemble with nasm -f bin repair.asm -o repair.bin.
bits 32
BASE equ 0x100000
org BASE
A equ 0x18
B equ 0x20
C equ 0x28
DATA equ 0x10
INVALID_DS equ 0x38
A_STACK equ 0x90000
B_STACK equ 0x98000
C_STACK equ 0x9c000

PD_A equ 0x20000
PD_B equ 0x21000
PT_A equ 0x22000
PT_B equ 0x23000
PHYS_A equ 0x24000
PHYS_B equ 0x25000
PT_IDENTITY equ 0x26000
VIEW equ 0x400000
CODE_A equ 0xa1a2a3a4
CODE_B equ 0xb1b2b3b4
DATA_A equ 0x1234ab56
DATA_B equ 0x7689cd01
DATA_UPDATED_B equ 0x897acbde

%define ABS(x) (BASE + (x - $$))
%macro TSS_DESCRIPTOR 1
    dw 0x67, ABS(%1) & 0xffff
    db (ABS(%1) >> 16) & 255, 0x89, 0, (ABS(%1) >> 24) & 255
%endmacro
%macro TRACE 1
    mov al, %1
    out 0xe9, al
%endmacro
%macro VIEW_PAGE 3
    ; mov eax, imm32; ret. The data word lives at +8 in the same page.
    mov byte [%1], 0xb8
    mov dword [%1 + 1], %2
    mov byte [%1 + 5], 0xc3
    mov dword [%1 + 8], %3
%endmacro
%macro CHECK_VIEW 2
    mov edi, VIEW
    call edi
    cmp eax, %1
    jne bad
    cmp dword [edi + 8], %2
    jne bad
%endmacro

header:
    dd 0x1badb002, 0x10000, -(0x1badb002 + 0x10000)
    dd header, BASE, image_end, image_end, entry
entry:
    cli
    lgdt [gdtr]
    jmp 8:.flat
.flat:
    mov ax, DATA
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    mov esp, A_STACK
    lidt [idtr]
    cld
    xor eax, eax
    mov edi, PD_A
    mov ecx, 7 * 1024
    rep stosd
    mov eax, 3
    mov edi, PT_IDENTITY
    mov ecx, 1024
.identity:
    stosd
    add eax, 4096
    loop .identity
    mov dword [PD_A], PT_IDENTITY | 3
    mov dword [PD_B], PT_IDENTITY | 3
    mov dword [PD_A + 4], PT_A | 3
    mov dword [PD_B + 4], PT_B | 3
    mov dword [PT_A], PHYS_A | 3
    mov dword [PT_B], PHYS_B | 3
    VIEW_PAGE PHYS_A, CODE_A, DATA_A
    VIEW_PAGE PHYS_B, CODE_B, DATA_B
    mov eax, PD_A
    mov cr3, eax
    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax
    mov ax, A
    ltr ax
    TRACE 'S'
    CHECK_VIEW CODE_A, DATA_A
    mov eax, 0x11223344
    call B:0
resume_a:
    cmp eax, 0x11223344
    jne bad
    cmp esp, A_STACK
    jne bad
    str ax
    cmp ax, A
    jne bad
    mov eax, cr3
    cmp eax, PD_A
    jne bad
    pushfd
    pop eax
    test eax, 1 << 14
    jnz bad
    cmp byte [gdt + A + 5], 0x8b
    jne bad
    cmp byte [gdt + B + 5], 0x89
    jne bad
    cmp byte [gdt + C + 5], 0x89
    jne bad
    CHECK_VIEW CODE_A, DATA_A
    cmp dword [PHYS_B + 8], DATA_UPDATED_B
    jne bad
    TRACE 'A'
    TRACE 'P'
    mov eax, 1
    jmp shutdown

incoming_b:
    cmp eax, 0x55667788
    jne bad
    cmp esp, B_STACK
    jne bad
    str ax
    cmp ax, B
    jne bad
    mov ax, ds
    cmp ax, DATA
    jne bad
    mov eax, cr3
    cmp eax, PD_B
    jne bad
    mov eax, cr0
    test eax, 1 << 3
    jz bad
    pushfd
    pop eax
    test eax, 1 << 14
    jz bad
    cmp word [tss_b], A
    jne bad
    cmp byte [gdt + B + 5], 0x8b
    jne bad
    cmp byte [gdt + C + 5], 0x89
    jne bad
    CHECK_VIEW CODE_B, DATA_B
    mov dword [edi + 8], DATA_UPDATED_B
    cmp dword [PHYS_B + 8], DATA_UPDATED_B
    jne bad
    cmp dword [PHYS_A + 8], DATA_A
    jne bad
    TRACE 'B'
    iretd
    jmp bad

handler_c:
    TRACE 'C'
    str ax
    cmp ax, C
    jne bad
    mov eax, cr3
    cmp eax, PD_A
    jne bad
    cmp esp, C_STACK - 4
    jne bad
    cmp dword [esp], INVALID_DS
    jne bad
    cmp word [tss_c], B
    jne bad
    cmp word [tss_b], A
    jne bad
    cmp dword [tss_a + 32], resume_a
    jne bad
    cmp dword [tss_b + 32], incoming_b
    jne bad
    pushfd
    pop eax
    test eax, 1 << 14
    jz bad
    cmp byte [gdt + A + 5], 0x8b
    jne bad
    cmp byte [gdt + B + 5], 0x8b
    jne bad
    cmp byte [gdt + C + 5], 0x8b
    jne bad
    CHECK_VIEW CODE_A, DATA_A
    cmp dword [PHYS_B + 8], DATA_B
    jne bad
    mov word [tss_b + 84], DATA  ; DS selector in B's 32-bit TSS
    add esp, 4                   ; retire the exception error code from C's stack
    TRACE 'R'
    iretd
    jmp bad

bad:
    TRACE '!'
    mov eax, 2
shutdown:
    out 0xf4, eax
    mov esi, shutdown_text
    mov dx, 0x8900
    mov ecx, 8
.byte:
    cs lodsb
    out dx, al
    loop .byte
    cli
    hlt
shutdown_text: db 'Shutdown'

align 8, db 0
gdt:
    dq 0, 0x00cf9b000000ffff, 0x00cf93000000ffff
    TSS_DESCRIPTOR tss_a
    TSS_DESCRIPTOR tss_b
    TSS_DESCRIPTOR tss_c
gdtr: dw $ - gdt - 1
    dd gdt
idt:
    times 10 dq 0
    dw 0, C
    db 0, 0x85
    dw 0
idtr: dw $ - idt - 1
    dd idt
align 16, db 0
tss_a:
    times 28 db 0
    dd PD_A
    times 68 db 0
    dw 0, 104
tss_b:
    times 28 db 0
    dd PD_B, incoming_b, 2, 0x55667788, 0, 0, 0, B_STACK, 0, 0, 0
    dd DATA, 8, DATA, INVALID_DS, DATA, DATA, 0
    dw 0, 104
tss_c:
    times 28 db 0
    dd PD_A, handler_c, 2, 0, 0, 0, 0, C_STACK, 0, 0, 0
    dd DATA, 8, DATA, DATA, DATA, DATA, 0
    dw 0, 104
image_end:
