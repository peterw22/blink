#ifndef BLINKWIN_STRING_H_
#define BLINKWIN_STRING_H_
#include_next <string.h>
#include <sys/types.h>
char *strdup(const char *);
char *strndup(const char *, size_t);
char *stpcpy(char *, const char *);
char *stpncpy(char *, const char *, size_t);
char *strchrnul(const char *, int);
void *memccpy(void *, const void *, int, size_t);
void *memrchr(const void *, int, size_t);
void *mempcpy(void *, const void *, size_t);
char *strtok_r(char *, const char *, char **);
char *strsignal(int);
int strcasecmp(const char *, const char *);
int strncasecmp(const char *, const char *, size_t);
size_t strlcpy(char *, const char *, size_t);
int strerror_r(int, char *, size_t);
#endif
