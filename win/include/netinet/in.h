#ifndef BLINKWIN_NETINET_IN_H_
#define BLINKWIN_NETINET_IN_H_
#include <sys/socket.h>
#include <stdint.h>
typedef uint16_t in_port_t;
typedef uint32_t in_addr_t;
struct in_addr { in_addr_t s_addr; };
struct in6_addr { union { uint8_t s6_addr[16]; uint16_t s6_addr16[8]; uint32_t s6_addr32[4]; }; };
#define s6_addr s6_addr
struct sockaddr_in { sa_family_t sin_family; in_port_t sin_port; struct in_addr sin_addr; unsigned char sin_zero[8]; };
struct sockaddr_in6 { sa_family_t sin6_family; in_port_t sin6_port; uint32_t sin6_flowinfo; struct in6_addr sin6_addr; uint32_t sin6_scope_id; };
#define IPPROTO_IP 0
#define IPPROTO_ICMP 1
#define IPPROTO_TCP 6
#define IPPROTO_UDP 17
#define IPPROTO_IPV6 41
#define IPPROTO_RAW 255
#define INADDR_ANY ((in_addr_t)0)
#define INADDR_LOOPBACK ((in_addr_t)0x7f000001)
#define INADDR_BROADCAST ((in_addr_t)0xffffffff)
#define INADDR_NONE ((in_addr_t)0xffffffff)
#define IP_TOS 1
#define IP_TTL 2
#define IP_HDRINCL 3
#define IP_RECVTTL 12
#define IP_MTU_DISCOVER 10
#define IPV6_V6ONLY 26
#define INET_ADDRSTRLEN 16
#define INET6_ADDRSTRLEN 46
uint16_t htons(uint16_t);
uint16_t ntohs(uint16_t);
uint32_t htonl(uint32_t);
uint32_t ntohl(uint32_t);
#define htons(x) __builtin_bswap16(x)
#define ntohs(x) __builtin_bswap16(x)
#define htonl(x) __builtin_bswap32(x)
#define ntohl(x) __builtin_bswap32(x)
#endif
