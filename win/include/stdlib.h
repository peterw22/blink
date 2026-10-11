#ifndef BLINKWIN_STDLIB_H_
#define BLINKWIN_STDLIB_H_
#include_next <stdlib.h>
int posix_memalign(void **, size_t, size_t);
char *realpath(const char *, char *);
int setenv(const char *, const char *, int);
int unsetenv(const char *);
int mkstemp(char *);
char *mkdtemp(char *);
long long random(void);
void srandom(unsigned);
#endif
