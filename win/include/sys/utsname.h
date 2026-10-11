#ifndef BLINKWIN_SYS_UTSNAME_H_
#define BLINKWIN_SYS_UTSNAME_H_
struct utsname { char sysname[65], nodename[65], release[65], version[65], machine[65], domainname[65]; };
int uname(struct utsname *);
#endif
