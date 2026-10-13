#ifndef BLINKWIN_STDIO_H_
#define BLINKWIN_STDIO_H_
#include_next <stdio.h>
#include <sys/types.h>
FILE *fdopen(int, const char *);
int fileno(FILE *);
FILE *fopen64(const char *, const char *);
ssize_t getline(char **, size_t *, FILE *);
int dprintf(int, const char *, ...);
int renameat(int, const char *, int, const char *);
#endif
