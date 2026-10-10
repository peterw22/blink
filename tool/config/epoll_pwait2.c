#include <sys/epoll.h>

int main(int argc, char *argv[]) {
  struct epoll_event e; epoll_pwait2(-1, &e, 0, 0, 0);
  return 0;
}
