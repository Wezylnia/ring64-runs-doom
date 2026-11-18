#define _GNU_SOURCE
#include <cpuid.h>
#include <errno.h>
#include <inttypes.h>
#include <setjmp.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#ifdef __APPLE__
#include <sys/sysctl.h>
#include <sys/ucontext.h>
#else
#include <ucontext.h>
#endif
#include <emmintrin.h>

#if !defined(__x86_64__)
#error compile this POSIX probe for x86-64
#endif

static sigjmp_buf recovery;
static volatile sig_atomic_t caught, sigcode, trap, err;
static _Atomic uintptr_t ip, signal_address, fault_address;
_Static_assert(ATOMIC_LONG_LOCK_FREE == 2 && ATOMIC_LLONG_LOCK_FREE == 2,
    "signal handler needs lock-free address fields");

static void capture(int signal, siginfo_t *info, void *raw_context)
{
    const ucontext_t *context = raw_context;
    caught = signal;
    sigcode = info->si_code;
    signal_address = (uintptr_t)info->si_addr;
#ifdef __APPLE__
    const mcontext_t machine = context->uc_mcontext;
    ip = machine->__ss.__rip;
    trap = machine->__es.__trapno;
    err = machine->__es.__err;
    fault_address = machine->__es.__faultvaddr;
#else
    const greg_t *machine = context->uc_mcontext.gregs;
    ip = (uintptr_t)machine[REG_RIP];
    trap = (int)machine[REG_TRAPNO];
    err = (int)machine[REG_ERR];
    fault_address = (uintptr_t)machine[REG_CR2];
#endif
    siglongjmp(recovery, 1);
}

static size_t hex(const char *src, uint8_t *dst)
{
    size_t length = strlen(src);
    if (length == 0 || length > 32 || length % 2)
        return 0;
    for (size_t i = 0; i < length / 2; i++) {
        char pair[] = { src[2*i], src[2*i+1], 0 };
        char *end;
        unsigned long value = strtoul(pair, &end, 16);
        if (*end || value > 255)
            return 0;
        dst[i] = (uint8_t)value;
    }
    return length / 2;
}

static int relative(uintptr_t value, uintptr_t origin)
{
    if (value >= origin && value - origin < 65536)
        return (int)(value - origin);
    if (origin > value && origin - value < 65536)
        return -(int)(origin - value);
    return -65536;
}

static int identity(int translated, size_t page)
{
    unsigned int eax, ebx, ecx, edx;
    char vendor[13] = {0};
    if (!__get_cpuid(0, &eax, &ebx, &ecx, &edx))
        return 3;
    memcpy(vendor, &ebx, sizeof ebx);
    memcpy(vendor + 4, &edx, sizeof edx);
    memcpy(vendor + 8, &ecx, sizeof ecx);
    if (!__get_cpuid(1, &eax, &ebx, &ecx, &edx))
        return 3;
    printf("{\"vendor\":\"%s\",\"signature\":%u,\"hypervisor\":%s,\"translated\":%d,\"page\":%zu}\n",
        vendor, eax, ecx >> 31 ? "true" : "false", translated, page);
    return 0;
}

static int one_of(const char *arg, const char *allowed)
{
    return arg[0] && !arg[1] && strchr(allowed, arg[0]) != NULL;
}

int main(int argc, char **argv)
{
    int translated = 0;
#ifdef __APPLE__
    size_t translated_size = sizeof translated;
    if (sysctlbyname("sysctl.proc_translated", &translated, &translated_size, NULL, 0) &&
        errno != ENOENT) {
        perror("sysctl.proc_translated");
        return 3;
    }
#endif
    const long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0)
        return 3;
    const size_t page = (size_t)page_size;
    if (argc == 2 && strcmp(argv[1], "--identity") == 0)
        return identity(translated, page);
    if (argc != 6 || !one_of(argv[3], "unx") || !one_of(argv[4], "rg") ||
        !one_of(argv[5], "sd"))
        return 2;

    uint8_t bytes[17];
    const size_t length = hex(argv[1], bytes);
    char *split_end;
    const size_t split = strtoul(argv[2], &split_end, 10);
    if (!length || split > length || !argv[2][0] || *split_end)
        return 2;
    const int fetch = argv[3][0];
    const int readable_data = argv[4][0] == 'r';
    const int single = argv[5][0] == 's';
    bytes[length] = 0xc3;

    uint8_t *code = mmap(NULL, 3 * page, PROT_NONE, MAP_ANON | MAP_PRIVATE, -1, 0);
    if (code == MAP_FAILED)
        return 3;
    uint8_t *next = code + page;
    uint8_t *data = code + 2 * page;
    uint8_t *entry = split ? next - split : code + 32;

    if (mprotect(code, (fetch == 'u' ? 1 : 2) * page, PROT_READ | PROT_WRITE))
        return 3;
    if (fetch == 'u' && munmap(next, page))
        return 3;
    size_t available = (split && fetch == 'u') ? split : length + 1;
    memcpy(entry, bytes, available);
    if (mprotect(code, page, PROT_READ | PROT_EXEC))
        return 3;
    if (fetch != 'u' &&
        mprotect(next, page, fetch == 'n' ? PROT_READ : PROT_READ | PROT_EXEC))
        return 3;

    if (readable_data) {
        if (mprotect(data, page, PROT_READ | PROT_WRITE))
            return 3;
        if (single) {
            const float scalar = 3.f;
            memcpy(data, &scalar, sizeof scalar);
        } else {
            const double scalar = 3.;
            memcpy(data, &scalar, sizeof scalar);
        }
    }

    struct sigaction action = { .sa_sigaction = capture, .sa_flags = SA_SIGINFO };
    if (sigemptyset(&action.sa_mask) || sigaction(SIGILL, &action, NULL) ||
        sigaction(SIGSEGV, &action, NULL) || sigaction(SIGBUS, &action, NULL))
        return 3;

    if (!sigsetjmp(recovery, 1)) {
        typedef __m128i entry_fn(const void *, unsigned int, __m128i);
        entry_fn *test = (entry_fn *)entry;
        const __m128i seed = single
            ? _mm_set_epi64x(0x4130000040e00000, 0x40a0000040000000)
            : _mm_set_epi64x(0x4014000000000000, 0x4000000000000000);
        const __m128i value = test(data, 0, seed);
        uint64_t bits[2];
        memcpy(bits, &value, sizeof bits);
        printf("{\"translated\":%d,\"page\":%zu,\"result\":\"ret\",\"xmm0\":\"%016" PRIx64 "%016" PRIx64 "\"}\n",
            translated, page, bits[1], bits[0]);
    } else {
        const char *kind = caught == SIGILL ? "SIGILL" : caught == SIGSEGV ? "SIGSEGV" : "SIGBUS";
        printf("{\"translated\":%d,\"page\":%zu,\"result\":\"%s\",\"si_code\":%d,"
            "\"trap\":%d,\"err\":%d,\"pc_delta\":%d,\"si_addr_delta\":%d,"
            "\"fault_addr_delta\":%d,\"data_delta\":%d}\n",
            translated, page, kind, sigcode, trap, err, relative(ip, (uintptr_t)entry),
            relative(signal_address, (uintptr_t)entry), relative(fault_address, (uintptr_t)entry),
            relative((uintptr_t)data, (uintptr_t)entry));
    }
    return 0;
}
