#ifndef BLINKWIN_NET_IF_H_
#define BLINKWIN_NET_IF_H_
#include <sys/socket.h>
#define IFNAMSIZ 16
#define IF_NAMESIZE IFNAMSIZ
struct ifreq {
  char ifr_name[IFNAMSIZ];
  union { struct sockaddr ifr_addr; struct sockaddr ifr_netmask; struct sockaddr ifr_broadaddr; short ifr_flags; int ifr_ifindex; int ifr_mtu; };
};
struct ifconf { int ifc_len; union { char *ifc_buf; struct ifreq *ifc_req; }; };
#endif
