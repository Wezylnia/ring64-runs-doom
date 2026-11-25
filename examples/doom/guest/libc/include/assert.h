#ifndef RING64_ASSERT_H
#define RING64_ASSERT_H
void __ring64_assert_fail(const char *expr, const char *file, int line) __attribute__((noreturn));
#ifdef NDEBUG
#define assert(x) ((void)0)
#else
#define assert(x) ((x) ? (void)0 : __ring64_assert_fail(#x, __FILE__, __LINE__))
#endif
#endif
