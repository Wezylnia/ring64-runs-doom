// Entry points for the DOOM guest. The linker script places .text.hyper at
// the start of the code segment so the host knows the hypercall address.

    .section .text.hyper, "ax"
    .globl ring64_hypercall
    .type ring64_hypercall, @function
ring64_hypercall:
    // Never executed: the host stops here, services the call and returns.
    ret
    .size ring64_hypercall, . - ring64_hypercall

    .section .text.start, "ax"
    .globl _start
    .type _start, @function
_start:
    lea __stack_top(%rip), %rsp
    and $-16, %rsp
    xor %ebp, %ebp
    call __libc_start
1:  hlt
    jmp 1b
    .size _start, . - _start

    .section .note.GNU-stack, "", @progbits
