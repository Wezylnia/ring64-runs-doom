#ifndef RING64_UNISTD_H
#define RING64_UNISTD_H
#include <stddef.h>
int isatty(int fd);
int usleep(unsigned usec);
int access(const char *path, int mode);
#define F_OK 0
#define R_OK 4
#endif
