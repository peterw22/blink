#ifndef BLINKWIN_SYS_STATVFS_H_
#define BLINKWIN_SYS_STATVFS_H_
#include <sys/types.h>
#define ST_RDONLY 1
#define ST_NOSUID 2
struct statvfs {
  unsigned long long f_bsize, f_frsize;
  fsblkcnt_t f_blocks, f_bfree, f_bavail;
  fsfilcnt_t f_files, f_ffree, f_favail;
  unsigned long long f_fsid, f_flag, f_namemax;
};
int statvfs(const char *, struct statvfs *);
int fstatvfs(int, struct statvfs *);
#endif
