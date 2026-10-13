#ifndef BLINKWIN_SYS_SYSINFO_H_
#define BLINKWIN_SYS_SYSINFO_H_
struct sysinfo {
  long long uptime;
  unsigned long long loads[3];
  unsigned long long totalram, freeram, sharedram, bufferram, totalswap, freeswap;
  unsigned short procs, pad;
  unsigned long long totalhigh, freehigh;
  unsigned int mem_unit;
};
int sysinfo(struct sysinfo *);
#endif
