#ifndef RING64_STDIO_H
#define RING64_STDIO_H
#include <stddef.h>
#include <stdarg.h>

typedef struct ring64_file FILE;
extern FILE *stdin, *stdout, *stderr;

#define EOF (-1)
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
#define BUFSIZ 1024
#define FILENAME_MAX 260

FILE *fopen(const char *path, const char *mode);
int fclose(FILE *f);
size_t fread(void *ptr, size_t size, size_t n, FILE *f);
size_t fwrite(const void *ptr, size_t size, size_t n, FILE *f);
int fseek(FILE *f, long offset, int whence);
long ftell(FILE *f);
int feof(FILE *f);
int ferror(FILE *f);
int fflush(FILE *f);
int fgetc(FILE *f);
int getc(FILE *f);
char *fgets(char *s, int size, FILE *f);
int fputc(int c, FILE *f);
int putc(int c, FILE *f);
int fputs(const char *s, FILE *f);
int puts(const char *s);
int putchar(int c);
int fileno(FILE *f);
int remove(const char *path);
int rename(const char *from, const char *to);
void setbuf(FILE *f, char *buf);
int setvbuf(FILE *f, char *buf, int mode, size_t size);
#define _IOFBF 0
#define _IOLBF 1
#define _IONBF 2

int printf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
int fprintf(FILE *f, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
int sprintf(char *s, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
int snprintf(char *s, size_t n, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
int vprintf(const char *fmt, va_list ap);
int vfprintf(FILE *f, const char *fmt, va_list ap);
int vsprintf(char *s, const char *fmt, va_list ap);
int vsnprintf(char *s, size_t n, const char *fmt, va_list ap);
int sscanf(const char *s, const char *fmt, ...);
int fscanf(FILE *f, const char *fmt, ...);
#endif
