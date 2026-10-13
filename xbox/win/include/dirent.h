#ifndef BLINKWIN_DIRENT_H_
#define BLINKWIN_DIRENT_H_
#include <sys/types.h>
#define DT_UNKNOWN 0
#define DT_FIFO 1
#define DT_CHR 2
#define DT_DIR 4
#define DT_BLK 6
#define DT_REG 8
#define DT_LNK 10
#define DT_SOCK 12
struct dirent {
  ino_t d_ino;
  off_t d_off;
  unsigned short d_reclen;
  unsigned char d_type;
  char d_name[256];
};
typedef struct BlinkWinDir DIR;
DIR *opendir(const char *);
DIR *fdopendir(int);
struct dirent *readdir(DIR *);
int closedir(DIR *);
void rewinddir(DIR *);
long telldir(DIR *);
void seekdir(DIR *, long);
int dirfd(DIR *);
#endif
