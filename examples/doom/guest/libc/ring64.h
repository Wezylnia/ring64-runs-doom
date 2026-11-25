// Guest <-> host interface for running DOOM on ring64.
//
// Cheap, frequent requests use x86 port I/O, which ring64 forwards to the
// host's synchronous IoProvider without leaving the translated code.
// Requests that need the host to touch guest memory (drawing a frame,
// copying the command line) or to give the browser a turn (sleeping) use a
// "hypercall": the guest calls ring64_hypercall, the host runs the CPU with
// `stop` set to that address, services the request while the CPU is idle and
// then performs the `ret` itself.
#ifndef RING64_GUEST_H
#define RING64_GUEST_H

#include <stdint.h>

#define R64_PORT_CONSOLE 0xe9 // out8: debug console byte
#define R64_PORT_TICKS 0x70   // in32: milliseconds since start
#define R64_PORT_KEY 0x72     // in16: 0x8000 | pressed << 8 | doom key, 0 if empty

#define R64_HC_EXIT 1    // (code)
#define R64_HC_FRAME 2   // (pixels: 320x200 palette indices, palette: 256 x {b,g,r,a})
#define R64_HC_SLEEP 3   // (milliseconds)
#define R64_HC_TITLE 4   // (const char *title)
#define R64_HC_ARGS 5    // (char *buffer, size) -> length written, NUL separated

long ring64_hypercall(long nr, long a, long b, long c);

static inline void r64_out8(uint16_t port, uint8_t value)
{
    __asm__ volatile("outb %0, %1" : : "a"(value), "Nd"(port));
}

static inline uint16_t r64_in16(uint16_t port)
{
    uint16_t value;
    __asm__ volatile("inw %1, %0" : "=a"(value) : "Nd"(port));
    return value;
}

static inline uint32_t r64_in32(uint16_t port)
{
    uint32_t value;
    __asm__ volatile("inl %1, %0" : "=a"(value) : "Nd"(port));
    return value;
}

#endif
