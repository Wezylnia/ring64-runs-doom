// A small freestanding C library: just enough of string.h, stdio.h,
// stdlib.h and math.h for doomgeneric. Files live in an in-memory table;
// the host maps doom1.wad read-only into guest memory and registers it here.
#include <stddef.h>
#include <stdint.h>
#include <stdarg.h>
#include <limits.h>
#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <unistd.h>
#include <sys/stat.h>

#include "ring64.h"

int errno;

// ---------------------------------------------------------------- string.h

void *memcpy(void *d, const void *s, size_t n)
{
    void *ret = d;
    __asm__ volatile("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory");
    return ret;
}

void *memmove(void *d, const void *s, size_t n)
{
    unsigned char *dp = d;
    const unsigned char *sp = s;
    if (dp == sp || n == 0)
        return d;
    if (dp < sp || dp >= sp + n)
        return memcpy(d, s, n);
    while (n--)
        dp[n] = sp[n];
    return d;
}

void *memset(void *d, int c, size_t n)
{
    void *ret = d;
    __asm__ volatile("rep stosb" : "+D"(d), "+c"(n) : "a"(c) : "memory");
    return ret;
}

int memcmp(const void *a, const void *b, size_t n)
{
    const unsigned char *x = a, *y = b;
    for (; n; n--, x++, y++)
        if (*x != *y)
            return *x - *y;
    return 0;
}

void *memchr(const void *s, int c, size_t n)
{
    const unsigned char *p = s;
    for (; n; n--, p++)
        if (*p == (unsigned char)c)
            return (void *)p;
    return NULL;
}

size_t strlen(const char *s)
{
    const char *p = s;
    while (*p)
        p++;
    return p - s;
}

size_t strnlen(const char *s, size_t n)
{
    size_t i = 0;
    while (i < n && s[i])
        i++;
    return i;
}

char *strcpy(char *d, const char *s)
{
    char *r = d;
    while ((*d++ = *s++))
        ;
    return r;
}

char *strncpy(char *d, const char *s, size_t n)
{
    size_t i = 0;
    for (; i < n && s[i]; i++)
        d[i] = s[i];
    for (; i < n; i++)
        d[i] = 0;
    return d;
}

char *strcat(char *d, const char *s)
{
    strcpy(d + strlen(d), s);
    return d;
}

char *strncat(char *d, const char *s, size_t n)
{
    char *p = d + strlen(d);
    while (n-- && *s)
        *p++ = *s++;
    *p = 0;
    return d;
}

int strcmp(const char *a, const char *b)
{
    while (*a && *a == *b)
        a++, b++;
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n)
{
    for (; n; n--, a++, b++) {
        if (*a != *b)
            return (unsigned char)*a - (unsigned char)*b;
        if (!*a)
            return 0;
    }
    return 0;
}

int strcasecmp(const char *a, const char *b)
{
    int x, y;
    do {
        x = tolower((unsigned char)*a++);
        y = tolower((unsigned char)*b++);
    } while (x && x == y);
    return x - y;
}

int strncasecmp(const char *a, const char *b, size_t n)
{
    int x = 0, y = 0;
    while (n--) {
        x = tolower((unsigned char)*a++);
        y = tolower((unsigned char)*b++);
        if (!x || x != y)
            return x - y;
    }
    return 0;
}

char *strchr(const char *s, int c)
{
    for (;; s++) {
        if (*s == (char)c)
            return (char *)s;
        if (!*s)
            return NULL;
    }
}

char *strrchr(const char *s, int c)
{
    const char *last = NULL;
    for (;; s++) {
        if (*s == (char)c)
            last = s;
        if (!*s)
            return (char *)last;
    }
}

char *strstr(const char *h, const char *n)
{
    size_t len = strlen(n);
    if (!len)
        return (char *)h;
    for (; *h; h++)
        if (*h == *n && !strncmp(h, n, len))
            return (char *)h;
    return NULL;
}

size_t strspn(const char *s, const char *accept)
{
    size_t i = 0;
    while (s[i] && strchr(accept, s[i]))
        i++;
    return i;
}

size_t strcspn(const char *s, const char *reject)
{
    size_t i = 0;
    while (s[i] && !strchr(reject, s[i]))
        i++;
    return i;
}

char *strpbrk(const char *s, const char *accept)
{
    s += strcspn(s, accept);
    return *s ? (char *)s : NULL;
}

char *strdup(const char *s)
{
    size_t n = strlen(s) + 1;
    char *d = malloc(n);
    if (d)
        memcpy(d, s, n);
    return d;
}

char *strerror(int e)
{
    switch (e) {
    case ENOENT:
        return "No such file or directory";
    case ENOMEM:
        return "Out of memory";
    default:
        return "Error";
    }
}

// ---------------------------------------------------------------- malloc
//
// First-fit free list over a bump heap between __heap_start and __heap_end.
// DOOM allocates one big zone up front, so this rarely does anything clever.

extern char __heap_start[], __heap_end[];
static char *heap_top = __heap_start;

typedef struct block {
    size_t size; // usable bytes
    struct block *next_free;
} block_t;

#define HEADER 16
static block_t *free_list;

static block_t *block_of(void *p) { return (block_t *)((char *)p - HEADER); }

void *malloc(size_t n)
{
    n = (n + 15) & ~(size_t)15;
    if (n == 0)
        n = 16;
    block_t **link = &free_list;
    for (block_t *b = free_list; b; link = &b->next_free, b = b->next_free) {
        if (b->size >= n && b->size <= n * 2 + 256) {
            *link = b->next_free;
            return (char *)b + HEADER;
        }
    }
    if ((size_t)(__heap_end - heap_top) < n + HEADER) {
        errno = ENOMEM;
        return NULL;
    }
    block_t *b = (block_t *)heap_top;
    heap_top += n + HEADER;
    b->size = n;
    return (char *)b + HEADER;
}

void free(void *p)
{
    if (!p)
        return;
    block_t *b = block_of(p);
    if ((char *)b + HEADER + b->size == heap_top) {
        heap_top = (char *)b;
        return;
    }
    b->next_free = free_list;
    free_list = b;
}

void *calloc(size_t n, size_t size)
{
    size_t total = n * size;
    void *p = malloc(total);
    if (p)
        memset(p, 0, total);
    return p;
}

void *realloc(void *p, size_t n)
{
    if (!p)
        return malloc(n);
    if (!n) {
        free(p);
        return NULL;
    }
    block_t *b = block_of(p);
    if (b->size >= n)
        return p;
    size_t want = (n + 15) & ~(size_t)15;
    if ((char *)b + HEADER + b->size == heap_top &&
        (size_t)(__heap_end - (char *)p) >= want) {
        heap_top = (char *)p + want;
        b->size = want;
        return p;
    }
    void *q = malloc(n);
    if (!q)
        return NULL;
    memcpy(q, p, b->size);
    free(p);
    return q;
}

// ---------------------------------------------------------------- files

typedef struct vfile {
    char name[96];
    unsigned char *data;
    size_t size;
    size_t cap;
    int external; // data is not owned by the heap (the mapped WAD)
    int used;
} vfile_t;

struct ring64_file {
    vfile_t *vf;
    size_t pos;
    int console;
    int writable;
    int eof;
    int used;
};

#define MAX_VFILES 64
#define MAX_FILES 32
static vfile_t vfiles[MAX_VFILES];
static struct ring64_file files[MAX_FILES];
static struct ring64_file console_out = {.console = 1, .writable = 1, .used = 1};
static struct ring64_file console_in = {.console = 1, .used = 1};
FILE *stdout = &console_out;
FILE *stderr = &console_out;
FILE *stdin = &console_in;

static const char *base_name(const char *path)
{
    // The guest has a single flat directory; "./foo", ".savegame/foo" -> keep
    // the path but strip a leading "./".
    while (path[0] == '.' && path[1] == '/')
        path += 2;
    return path;
}

static vfile_t *vfile_find(const char *path)
{
    path = base_name(path);
    for (int i = 0; i < MAX_VFILES; i++)
        if (vfiles[i].used && !strcasecmp(vfiles[i].name, path))
            return &vfiles[i];
    return NULL;
}

static vfile_t *vfile_create(const char *path)
{
    path = base_name(path);
    for (int i = 0; i < MAX_VFILES; i++) {
        if (!vfiles[i].used) {
            vfile_t *vf = &vfiles[i];
            memset(vf, 0, sizeof(*vf));
            vf->used = 1;
            strncpy(vf->name, path, sizeof(vf->name) - 1);
            return vf;
        }
    }
    return NULL;
}

void ring64_register_file(const char *name, const void *data, size_t size)
{
    vfile_t *vf = vfile_create(name);
    if (!vf)
        return;
    vf->data = (unsigned char *)data;
    vf->size = size;
    vf->cap = size;
    vf->external = 1;
}

FILE *fopen(const char *path, const char *mode)
{
    vfile_t *vf = vfile_find(path);
    int writing = mode[0] == 'w' || mode[0] == 'a' || strchr(mode, '+') != NULL;
    if (mode[0] == 'r' && !vf) {
        errno = ENOENT;
        return NULL;
    }
    if (!vf) {
        vf = vfile_create(path);
        if (!vf) {
            errno = ENOMEM;
            return NULL;
        }
    }
    if (writing && vf->external) {
        // Copy-on-write for host provided files.
        unsigned char *copy = malloc(vf->size ? vf->size : 1);
        if (!copy)
            return NULL;
        memcpy(copy, vf->data, vf->size);
        vf->data = copy;
        vf->cap = vf->size;
        vf->external = 0;
    }
    if (mode[0] == 'w')
        vf->size = 0;
    for (int i = 0; i < MAX_FILES; i++) {
        if (!files[i].used) {
            FILE *f = &files[i];
            memset(f, 0, sizeof(*f));
            f->used = 1;
            f->vf = vf;
            f->writable = writing;
            f->pos = mode[0] == 'a' ? vf->size : 0;
            return f;
        }
    }
    errno = ENOMEM;
    return NULL;
}

int fclose(FILE *f)
{
    if (f && !f->console)
        f->used = 0;
    return 0;
}

static void console_write(const char *s, size_t n)
{
    for (size_t i = 0; i < n; i++)
        r64_out8(R64_PORT_CONSOLE, (uint8_t)s[i]);
}

size_t fread(void *ptr, size_t size, size_t n, FILE *f)
{
    if (f->console || !size)
        return 0;
    size_t want = size * n;
    size_t avail = f->pos < f->vf->size ? f->vf->size - f->pos : 0;
    if (want > avail) {
        want = avail - avail % size;
        f->eof = 1;
    }
    memcpy(ptr, f->vf->data + f->pos, want);
    f->pos += want;
    return want / size;
}

size_t fwrite(const void *ptr, size_t size, size_t n, FILE *f)
{
    size_t bytes = size * n;
    if (!size)
        return 0;
    if (f->console) {
        console_write(ptr, bytes);
        return n;
    }
    if (!f->writable)
        return 0;
    vfile_t *vf = f->vf;
    size_t end = f->pos + bytes;
    if (end > vf->cap) {
        size_t cap = vf->cap ? vf->cap : 256;
        while (cap < end)
            cap *= 2;
        unsigned char *data = realloc(vf->data, cap);
        if (!data)
            return 0;
        vf->data = data;
        vf->cap = cap;
    }
    if (f->pos > vf->size)
        memset(vf->data + vf->size, 0, f->pos - vf->size);
    memcpy(vf->data + f->pos, ptr, bytes);
    f->pos = end;
    if (end > vf->size)
        vf->size = end;
    return n;
}

int fseek(FILE *f, long offset, int whence)
{
    if (f->console)
        return -1;
    long base = whence == SEEK_SET ? 0 : whence == SEEK_CUR ? (long)f->pos : (long)f->vf->size;
    if (base + offset < 0)
        return -1;
    f->pos = base + offset;
    f->eof = 0;
    return 0;
}

long ftell(FILE *f) { return f->console ? 0 : (long)f->pos; }
int feof(FILE *f) { return f->console ? 1 : f->eof; }
int ferror(FILE *f) { (void)f; return 0; }
int fflush(FILE *f) { (void)f; return 0; }
int fileno(FILE *f) { return f == stdin ? 0 : f == stdout ? 1 : 3; }
int isatty(int fd) { (void)fd; return 0; }
void setbuf(FILE *f, char *buf) { (void)f; (void)buf; }
int setvbuf(FILE *f, char *buf, int mode, size_t size) { (void)f; (void)buf; (void)mode; (void)size; return 0; }

int fgetc(FILE *f)
{
    unsigned char c;
    return fread(&c, 1, 1, f) == 1 ? c : EOF;
}

int getc(FILE *f) { return fgetc(f); }

char *fgets(char *s, int size, FILE *f)
{
    int i = 0;
    while (i < size - 1) {
        int c = fgetc(f);
        if (c == EOF)
            break;
        s[i++] = (char)c;
        if (c == '\n')
            break;
    }
    if (i == 0)
        return NULL;
    s[i] = 0;
    return s;
}

int fputc(int c, FILE *f)
{
    unsigned char ch = (unsigned char)c;
    return fwrite(&ch, 1, 1, f) == 1 ? ch : EOF;
}

int putc(int c, FILE *f) { return fputc(c, f); }
int putchar(int c) { return fputc(c, stdout); }

int fputs(const char *s, FILE *f)
{
    fwrite(s, 1, strlen(s), f);
    return 0;
}

int puts(const char *s)
{
    fputs(s, stdout);
    putchar('\n');
    return 0;
}

int remove(const char *path)
{
    vfile_t *vf = vfile_find(path);
    if (!vf) {
        errno = ENOENT;
        return -1;
    }
    if (!vf->external)
        free(vf->data);
    vf->used = 0;
    return 0;
}

int rename(const char *from, const char *to)
{
    vfile_t *vf = vfile_find(from);
    if (!vf) {
        errno = ENOENT;
        return -1;
    }
    vfile_t *old = vfile_find(to);
    if (old && old != vf)
        remove(to);
    strncpy(vf->name, base_name(to), sizeof(vf->name) - 1);
    return 0;
}

int mkdir(const char *path, mode_t mode) { (void)path; (void)mode; return 0; }
int access(const char *path, int mode) { (void)mode; return vfile_find(path) ? 0 : -1; }

// ---------------------------------------------------------------- printf

typedef struct {
    char *buf;
    size_t cap;
    size_t len;
    FILE *file;
} out_t;

static void out_char(out_t *o, char c)
{
    if (o->file) {
        char tmp = c;
        fwrite(&tmp, 1, 1, o->file);
    } else if (o->len + 1 < o->cap) {
        o->buf[o->len] = c;
    }
    o->len++;
}

static void out_pad(out_t *o, char c, int n)
{
    while (n-- > 0)
        out_char(o, c);
}

static void out_str(out_t *o, const char *s, size_t n)
{
    for (size_t i = 0; i < n; i++)
        out_char(o, s[i]);
}

static int format_unsigned(char *tmp, unsigned long long v, int base, int upper)
{
    const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    int n = 0;
    do {
        tmp[n++] = digits[v % base];
        v /= base;
    } while (v);
    return n; // reversed
}

static int vformat(out_t *o, const char *fmt, va_list ap)
{
    for (; *fmt; fmt++) {
        if (*fmt != '%') {
            out_char(o, *fmt);
            continue;
        }
        fmt++;
        int left = 0, zero = 0, plus = 0, space = 0, alt = 0;
        for (;; fmt++) {
            if (*fmt == '-')
                left = 1;
            else if (*fmt == '0')
                zero = 1;
            else if (*fmt == '+')
                plus = 1;
            else if (*fmt == ' ')
                space = 1;
            else if (*fmt == '#')
                alt = 1;
            else
                break;
        }
        int width = 0, prec = -1;
        if (*fmt == '*') {
            width = va_arg(ap, int);
            if (width < 0) {
                left = 1;
                width = -width;
            }
            fmt++;
        } else {
            while (isdigit((unsigned char)*fmt))
                width = width * 10 + (*fmt++ - '0');
        }
        if (*fmt == '.') {
            fmt++;
            prec = 0;
            if (*fmt == '*') {
                prec = va_arg(ap, int);
                fmt++;
            } else {
                while (isdigit((unsigned char)*fmt))
                    prec = prec * 10 + (*fmt++ - '0');
            }
        }
        int lng = 0; // 0 int, 1 long, 2 long long, -1 short, -2 char
        for (;; fmt++) {
            if (*fmt == 'l')
                lng++;
            else if (*fmt == 'h')
                lng--;
            else if (*fmt == 'z' || *fmt == 't' || *fmt == 'j')
                lng = 1;
            else
                break;
        }
        char conv = *fmt;
        if (!conv)
            break;
        char tmp[72];
        const char *prefix = "";
        int n = 0;
        switch (conv) {
        case 'd':
        case 'i': {
            long long v = lng >= 1 ? va_arg(ap, long) : va_arg(ap, int);
            if (lng == -1)
                v = (short)v;
            else if (lng <= -2)
                v = (signed char)v;
            unsigned long long u = v < 0 ? -(unsigned long long)v : (unsigned long long)v;
            prefix = v < 0 ? "-" : plus ? "+" : space ? " " : "";
            n = format_unsigned(tmp, u, 10, 0);
            goto emit_number;
        }
        case 'u':
        case 'x':
        case 'X':
        case 'o':
        case 'p': {
            unsigned long long u;
            int base = conv == 'o' ? 8 : conv == 'u' ? 10 : 16;
            if (conv == 'p') {
                u = (uintptr_t)va_arg(ap, void *);
                prefix = "0x";
            } else {
                u = lng >= 1 ? va_arg(ap, unsigned long) : va_arg(ap, unsigned int);
                if (lng == -1)
                    u = (unsigned short)u;
                else if (lng <= -2)
                    u = (unsigned char)u;
                if (alt && u && base == 16)
                    prefix = conv == 'X' ? "0X" : "0x";
                if (alt && base == 8)
                    prefix = "0";
            }
            n = format_unsigned(tmp, u, base, conv == 'X');
        emit_number: {
            if (prec == 0 && n == 1 && tmp[0] == '0')
                n = 0;
            int digits = n < prec ? prec : n;
            int plen = (int)strlen(prefix);
            int pad = width - digits - plen;
            if (!left && !(zero && prec < 0))
                out_pad(o, ' ', pad);
            out_str(o, prefix, plen);
            if (!left && zero && prec < 0)
                out_pad(o, '0', pad);
            out_pad(o, '0', digits - n);
            while (n)
                out_char(o, tmp[--n]);
            if (left)
                out_pad(o, ' ', pad);
            break;
        }
        }
        case 'c': {
            char c = (char)va_arg(ap, int);
            if (!left)
                out_pad(o, ' ', width - 1);
            out_char(o, c);
            if (left)
                out_pad(o, ' ', width - 1);
            break;
        }
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s)
                s = "(null)";
            size_t len = prec >= 0 ? strnlen(s, prec) : strlen(s);
            if (!left)
                out_pad(o, ' ', width - (int)len);
            out_str(o, s, len);
            if (left)
                out_pad(o, ' ', width - (int)len);
            break;
        }
        case 'f':
        case 'F':
        case 'g':
        case 'G':
        case 'e':
        case 'E': {
            double v = va_arg(ap, double);
            if (prec < 0)
                prec = 6;
            if (prec > 9)
                prec = 9;
            char num[64];
            int len = 0;
            if (v < 0) {
                num[len++] = '-';
                v = -v;
            } else if (plus) {
                num[len++] = '+';
            }
            double scale = 1;
            for (int i = 0; i < prec; i++)
                scale *= 10;
            unsigned long long whole = (unsigned long long)v;
            unsigned long long frac = (unsigned long long)((v - (double)whole) * scale + 0.5);
            if (frac >= (unsigned long long)scale) {
                whole++;
                frac -= (unsigned long long)scale;
            }
            char t[32];
            int k = format_unsigned(t, whole, 10, 0);
            while (k)
                num[len++] = t[--k];
            if (prec) {
                num[len++] = '.';
                k = format_unsigned(t, frac, 10, 0);
                for (int i = k; i < prec; i++)
                    num[len++] = '0';
                while (k)
                    num[len++] = t[--k];
            }
            if (!left)
                out_pad(o, zero ? '0' : ' ', width - len);
            out_str(o, num, len);
            if (left)
                out_pad(o, ' ', width - len);
            break;
        }
        case '%':
            out_char(o, '%');
            break;
        default:
            out_char(o, '%');
            out_char(o, conv);
            break;
        }
    }
    if (!o->file && o->cap)
        o->buf[o->len < o->cap ? o->len : o->cap - 1] = 0;
    return (int)o->len;
}

int vsnprintf(char *s, size_t n, const char *fmt, va_list ap)
{
    out_t o = {s, n, 0, NULL};
    return vformat(&o, fmt, ap);
}

int vsprintf(char *s, const char *fmt, va_list ap) { return vsnprintf(s, (size_t)-1 >> 1, fmt, ap); }

int vfprintf(FILE *f, const char *fmt, va_list ap)
{
    out_t o = {NULL, 0, 0, f};
    return vformat(&o, fmt, ap);
}

int vprintf(const char *fmt, va_list ap) { return vfprintf(stdout, fmt, ap); }

int snprintf(char *s, size_t n, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = vsnprintf(s, n, fmt, ap);
    va_end(ap);
    return r;
}

int sprintf(char *s, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = vsprintf(s, fmt, ap);
    va_end(ap);
    return r;
}

int fprintf(FILE *f, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = vfprintf(f, fmt, ap);
    va_end(ap);
    return r;
}

int printf(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = vfprintf(stdout, fmt, ap);
    va_end(ap);
    return r;
}

// ---------------------------------------------------------------- numbers

long strtol(const char *s, char **end, int base)
{
    const char *p = s;
    while (isspace((unsigned char)*p))
        p++;
    int neg = 0;
    if (*p == '-' || *p == '+')
        neg = *p++ == '-';
    if ((base == 0 || base == 16) && p[0] == '0' && (p[1] == 'x' || p[1] == 'X') &&
        isxdigit((unsigned char)p[2])) {
        p += 2;
        base = 16;
    } else if (base == 0) {
        base = p[0] == '0' ? 8 : 10;
    }
    unsigned long v = 0;
    const char *start = p;
    for (;; p++) {
        int d;
        if (isdigit((unsigned char)*p))
            d = *p - '0';
        else if (isalpha((unsigned char)*p))
            d = tolower((unsigned char)*p) - 'a' + 10;
        else
            break;
        if (d >= base)
            break;
        v = v * base + d;
    }
    if (end)
        *end = (char *)(p == start ? s : p);
    return neg ? -(long)v : (long)v;
}

unsigned long strtoul(const char *s, char **end, int base) { return (unsigned long)strtol(s, end, base); }
int atoi(const char *s) { return (int)strtol(s, NULL, 10); }
long atol(const char *s) { return strtol(s, NULL, 10); }

double strtod(const char *s, char **end)
{
    const char *p = s;
    while (isspace((unsigned char)*p))
        p++;
    int neg = 0;
    if (*p == '-' || *p == '+')
        neg = *p++ == '-';
    double v = 0;
    const char *start = p;
    while (isdigit((unsigned char)*p))
        v = v * 10 + (*p++ - '0');
    if (*p == '.') {
        p++;
        double scale = 0.1;
        while (isdigit((unsigned char)*p)) {
            v += (*p++ - '0') * scale;
            scale *= 0.1;
        }
    }
    if (p != start && (*p == 'e' || *p == 'E')) {
        char *after;
        long e = strtol(p + 1, &after, 10);
        if (after != p + 1) {
            p = after;
            while (e > 0) {
                v *= 10;
                e--;
            }
            while (e < 0) {
                v /= 10;
                e++;
            }
        }
    }
    if (end)
        *end = (char *)(p == start ? s : p);
    return neg ? -v : v;
}

double atof(const char *s) { return strtod(s, NULL); }
int abs(int v) { return v < 0 ? -v : v; }
long labs(long v) { return v < 0 ? -v : v; }

static unsigned long rand_state = 1;
int rand(void)
{
    rand_state = rand_state * 6364136223846793005UL + 1442695040888963407UL;
    return (int)(rand_state >> 33);
}
void srand(unsigned seed) { rand_state = seed; }

void qsort(void *base, size_t n, size_t size, int (*cmp)(const void *, const void *))
{
    // Shell sort with a byte-wise swap: small, allocation free, good enough.
    unsigned char *a = base;
    for (size_t gap = n / 2; gap > 0; gap /= 2) {
        for (size_t i = gap; i < n; i++) {
            for (size_t j = i; j >= gap && cmp(a + (j - gap) * size, a + j * size) > 0; j -= gap) {
                unsigned char *x = a + (j - gap) * size, *y = a + j * size;
                for (size_t k = 0; k < size; k++) {
                    unsigned char t = x[k];
                    x[k] = y[k];
                    y[k] = t;
                }
            }
        }
    }
}

char *getenv(const char *name) { (void)name; return NULL; }
int system(const char *cmd) { (void)cmd; return -1; }
int usleep(unsigned usec) { ring64_hypercall(R64_HC_SLEEP, usec / 1000, 0, 0); return 0; }

// ---------------------------------------------------------------- scanf

static int scan_core(const char *s, const char *fmt, va_list ap, size_t *consumed)
{
    const char *p = s;
    int assigned = 0;
    for (; *fmt; fmt++) {
        if (isspace((unsigned char)*fmt)) {
            while (isspace((unsigned char)*p))
                p++;
            continue;
        }
        if (*fmt != '%' || fmt[1] == '%') {
            if (*fmt == '%')
                fmt++;
            if (*p != *fmt)
                break;
            p++;
            continue;
        }
        fmt++;
        int suppress = 0;
        if (*fmt == '*') {
            suppress = 1;
            fmt++;
        }
        int width = 0;
        while (isdigit((unsigned char)*fmt))
            width = width * 10 + (*fmt++ - '0');
        int lng = 0;
        while (*fmt == 'l' || *fmt == 'h') {
            lng += *fmt == 'l' ? 1 : -1;
            fmt++;
        }
        char conv = *fmt;
        if (conv != '[' && conv != 'c')
            while (isspace((unsigned char)*p))
                p++;
        if (!*p)
            break;
        if (conv == 'd' || conv == 'i' || conv == 'x' || conv == 'X' || conv == 'o' || conv == 'u') {
            int base = conv == 'd' || conv == 'u' ? 10 : conv == 'i' ? 0 : conv == 'o' ? 8 : 16;
            char *end;
            long v = strtol(p, &end, base);
            if (end == p)
                break;
            p = end;
            if (!suppress) {
                if (lng >= 1)
                    *va_arg(ap, long *) = v;
                else if (lng == -1)
                    *va_arg(ap, short *) = (short)v;
                else
                    *va_arg(ap, int *) = (int)v;
                assigned++;
            }
        } else if (conv == 'f' || conv == 'g' || conv == 'e') {
            char *end;
            double v = strtod(p, &end);
            if (end == p)
                break;
            p = end;
            if (!suppress) {
                if (lng >= 1)
                    *va_arg(ap, double *) = v;
                else
                    *va_arg(ap, float *) = (float)v;
                assigned++;
            }
        } else if (conv == 's') {
            char *out = suppress ? NULL : va_arg(ap, char *);
            int n = 0;
            while (*p && !isspace((unsigned char)*p) && (!width || n < width)) {
                if (out)
                    out[n] = *p;
                n++;
                p++;
            }
            if (out) {
                out[n] = 0;
                assigned++;
            }
        } else if (conv == 'c') {
            char *out = suppress ? NULL : va_arg(ap, char *);
            int n = width ? width : 1;
            for (int i = 0; i < n && *p; i++, p++)
                if (out)
                    out[i] = *p;
            if (out)
                assigned++;
        } else if (conv == '[') {
            fmt++;
            int negate = 0;
            if (*fmt == '^') {
                negate = 1;
                fmt++;
            }
            const char *set = fmt;
            if (*fmt == ']')
                fmt++;
            while (*fmt && *fmt != ']')
                fmt++;
            size_t setlen = fmt - set;
            char *out = suppress ? NULL : va_arg(ap, char *);
            int n = 0;
            while (*p && (!width || n < width)) {
                int in = memchr(set, *p, setlen) != NULL;
                if (in == negate)
                    break;
                if (out)
                    out[n] = *p;
                n++;
                p++;
            }
            if (!n)
                break;
            if (out) {
                out[n] = 0;
                assigned++;
            }
        } else {
            break;
        }
    }
    if (consumed)
        *consumed = p - s;
    return assigned;
}

int sscanf(const char *s, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = scan_core(s, fmt, ap, NULL);
    va_end(ap);
    return r;
}

int fscanf(FILE *f, const char *fmt, ...)
{
    if (f->console || f->pos >= f->vf->size) {
        f->eof = 1;
        return EOF;
    }
    // Scan from a NUL terminated copy of the next chunk of the file.
    size_t avail = f->vf->size - f->pos;
    size_t chunk = avail < 1024 ? avail : 1024;
    char buf[1025];
    memcpy(buf, f->vf->data + f->pos, chunk);
    buf[chunk] = 0;
    va_list ap;
    va_start(ap, fmt);
    size_t used = 0;
    int r = scan_core(buf, fmt, ap, &used);
    va_end(ap);
    f->pos += used;
    return r;
}

// ---------------------------------------------------------------- math

double fabs(double x) { return x < 0 ? -x : x; }
float fabsf(float x) { return x < 0 ? -x : x; }
double sqrt(double x) { return __builtin_sqrt(x); }

double floor(double x)
{
    if (x >= 9007199254740992.0 || x <= -9007199254740992.0)
        return x;
    long long i = (long long)x;
    return (double)(x < (double)i ? i - 1 : i);
}

double ceil(double x) { return -floor(-x); }
double fmod(double x, double y) { return x - y * (double)(long long)(x / y); }

double exp(double x)
{
    // exp(x) = 2^k * exp(r), |r| <= ln2/2
    const double ln2 = 0.6931471805599453;
    long long k = (long long)(x / ln2 + (x < 0 ? -0.5 : 0.5));
    double r = x - (double)k * ln2, term = 1, sum = 1;
    for (int i = 1; i < 20; i++) {
        term *= r / i;
        sum += term;
    }
    while (k > 0) {
        sum *= 2;
        k--;
    }
    while (k < 0) {
        sum /= 2;
        k++;
    }
    return sum;
}

double log(double x)
{
    if (x <= 0)
        return -1e308;
    const double ln2 = 0.6931471805599453;
    int k = 0;
    while (x > 2) {
        x /= 2;
        k++;
    }
    while (x < 1) {
        x *= 2;
        k--;
    }
    // ln(x) = 2 atanh((x-1)/(x+1))
    double y = (x - 1) / (x + 1), y2 = y * y, term = y, sum = 0;
    for (int i = 1; i < 40; i += 2) {
        sum += term / i;
        term *= y2;
    }
    return 2 * sum + k * ln2;
}

double pow(double x, double y)
{
    if (y == 0)
        return 1;
    if (y == (double)(long long)y && y > -64 && y < 64) {
        long long n = (long long)y;
        double r = 1, b = n < 0 ? 1 / x : x;
        for (long long i = n < 0 ? -n : n; i; i >>= 1, b *= b)
            if (i & 1)
                r *= b;
        return r;
    }
    if (x <= 0)
        return 0;
    return exp(y * log(x));
}

static double reduce_angle(double x)
{
    const double two_pi = 6.283185307179586;
    x = fmod(x, two_pi);
    if (x > M_PI)
        x -= two_pi;
    if (x < -M_PI)
        x += two_pi;
    return x;
}

double sin(double x)
{
    x = reduce_angle(x);
    double term = x, sum = x, x2 = x * x;
    for (int i = 1; i < 12; i++) {
        term *= -x2 / ((2 * i) * (2 * i + 1));
        sum += term;
    }
    return sum;
}

double cos(double x) { return sin(x + M_PI / 2); }
double tan(double x) { return sin(x) / cos(x); }

double atan(double x)
{
    if (x > 1)
        return M_PI / 2 - atan(1 / x);
    if (x < -1)
        return -M_PI / 2 - atan(1 / x);
    // atan(x) = 2 atan(x / (1 + sqrt(1 + x^2))) keeps the series short.
    double y = x / (1 + sqrt(1 + x * x));
    double y2 = y * y, term = y, sum = 0;
    for (int i = 1; i < 60; i += 2) {
        sum += term / i;
        term *= -y2;
    }
    return 2 * sum;
}

double atan2(double y, double x)
{
    if (x > 0)
        return atan(y / x);
    if (x < 0)
        return y >= 0 ? atan(y / x) + M_PI : atan(y / x) - M_PI;
    return y > 0 ? M_PI / 2 : y < 0 ? -M_PI / 2 : 0;
}

// ---------------------------------------------------------------- process

#define MAX_ATEXIT 32
static void (*atexit_fns[MAX_ATEXIT])(void);
static int atexit_count;

int atexit(void (*fn)(void))
{
    if (atexit_count == MAX_ATEXIT)
        return -1;
    atexit_fns[atexit_count++] = fn;
    return 0;
}

void exit(int code)
{
    while (atexit_count)
        atexit_fns[--atexit_count]();
    for (;;)
        ring64_hypercall(R64_HC_EXIT, code, 0, 0);
}

void abort(void)
{
    fputs("abort()\n", stderr);
    for (;;)
        ring64_hypercall(R64_HC_EXIT, 134, 0, 0);
}

void __ring64_assert_fail(const char *expr, const char *file, int line)
{
    fprintf(stderr, "assertion failed: %s (%s:%d)\n", expr, file, line);
    abort();
}

// ---------------------------------------------------------------- startup

#define WAD_BASE ((const void *)0x10000000)
#define MAX_ARGS 32

int main(int argc, char **argv);

void __libc_start(void)
{
    extern char __bss_start[], __bss_end[];
    memset(__bss_start, 0, __bss_end - __bss_start);

    // The args hypercall returns NUL separated arguments, for example
    // "doom\0-iwad\0doom1.wad\0@wadsize=4196020\0". The @wadsize entry is not
    // passed to DOOM: it tells us how many bytes of the read-only WAD mapping
    // at WAD_BASE belong to doom1.wad.
    static char argbuf[1024];
    static char *argv[MAX_ARGS + 1];
    long len = ring64_hypercall(R64_HC_ARGS, (long)argbuf, sizeof(argbuf) - 1, 0);
    if (len < 0)
        len = 0;
    argbuf[len] = 0;
    int argc = 0;
    long wad_size = 0;
    for (long i = 0; i < len && argc < MAX_ARGS;) {
        char *arg = &argbuf[i];
        size_t n = strlen(arg);
        if (!strncmp(arg, "@wadsize=", 9))
            wad_size = strtol(arg + 9, NULL, 10);
        else
            argv[argc++] = arg;
        i += n + 1;
    }
    argv[argc] = NULL;
    if (wad_size > 0)
        ring64_register_file("doom1.wad", WAD_BASE, wad_size);
    exit(main(argc, argv));
}
