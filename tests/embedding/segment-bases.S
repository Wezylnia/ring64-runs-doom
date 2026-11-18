// Standalone Linux process. No C runtime uses FS while the packet changes it.
.text
.globl _start
_start:
    mov $7, %eax
    xor %ecx, %ecx
    cpuid
    test $1, %ebx
    jz skip
    mov (%rsp), %rcx
    lea 16(%rsp,%rcx,8), %rsi
environment:
    lodsq
    test %rax, %rax
    jnz environment
auxiliary:
    lodsq
    test %rax, %rax
    jz skip
    mov %rax, %rcx
    lodsq
    cmp $26, %rcx               // AT_HWCAP2
    jne auxiliary
    test $2, %eax               // kernel enabled FSGSBASE
    jz skip
    lea result(%rip), %rdi
    call base_packet
    mov $1, %eax                // write(1, result, 64)
    mov $1, %edi
    lea result(%rip), %rsi
    mov $64, %edx
    syscall
    cmp $64, %eax
    jne failed
    mov $1, %eax                // append the exact bytes just executed
    mov $1, %edi
    lea base_packet(%rip), %rsi
    mov $packet_end-base_packet, %edx
    syscall
    cmp $packet_end-base_packet, %eax
    jne failed
    xor %edi, %edi
    jmp exit
failed:
    mov $1, %edi
    jmp exit
skip:
    mov $77, %edi
exit:
    mov $60, %eax
    syscall

.bss
result: .skip 64

// No relocations. The test executes this exact section on the public Ring64 CPU.
.section .oracle,"ax",@progbits
base_packet:
    rdfsbase %r10
    rdgsbase %r11
    movabs $0x00006001abcde123, %r8
    movabs $0x000050024321fedc, %r9
    wrfsbase %r8
    wrgsbase %r9
    rdfsbase %r12
    rdgsbase %r13
    mov %r12, 0(%rdi)
    mov %r13, 8(%rdi)
    rdfsbase %r12d
    rdgsbase %r13d
    mov %r12, 16(%rdi)
    mov %r13, 24(%rdi)
    mov $-1, %rax
    .byte 0x66,0xf3,0x0f,0xae,0xc8 // rdgsbase eax with redundant operand-size prefix
    mov %rax, 32(%rdi)
    mov $-1, %rax
    .byte 0xf3,0x66,0x0f,0xae,0xc8
    mov %rax, 40(%rdi)
    wrfsbase %r9d
    wrgsbase %r8d
    rdfsbase %r12
    rdgsbase %r13
    mov %r12, 48(%rdi)
    mov %r13, 56(%rdi)
    wrfsbase %r10
    wrgsbase %r11
    ret
packet_end:
.section .note.GNU-stack,"",@progbits
