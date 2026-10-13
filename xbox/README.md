# blink on the Xbox

Rust around the C emulator in `../blink`, for running Linux programs
(Claude Code) inside an Xbox UWP app, where there is no `fork()`, no POSIX
and no console.

## `xbox/` — the `blink-xbox` crate

- `src/procs.rs`: the guest process table. With it, guest `fork()`/`vfork()`
  run the child as a thread of the same host process (each guest process
  is its own blink `System`); pids, parents, process groups, sessions,
  `wait4()` and `kill()` live here. The C side is `blink/procs.{c,h}`.
- `src/win/`: the POSIX host layer blink's C needs on Windows: fd table,
  files and directories, in-memory pipes, `poll`, epoll and eventfd, a
  terminal, Winsock sockets, pthreads, emulated signals, clocks and
  `VirtualAllocFromApp` memory. `win/include/` has the POSIX headers it is
  compiled against and `win/config.h` replaces blink's `config.h`.
- `src/lib.rs`: `run()` (a `blink` command line) and, on Windows,
  `start()` (a guest on a thread with a terminal, for an embedding app).
- `tools/winroot.py`: converts a Linux rootfs into the Windows form
  (symlinks become marker files).

Build: `cargo build --release` (Linux: the `blink` CLI, add
`BLINK_INPROCESS=1` to run guest processes as threads) or
`cargo build --release --target x86_64-pc-windows-msvc` (needs clang,
lld and `xwin splat --output ~/.local/share/xwin`).

## `xbox/term/` — Blink Terminal

A UWP app (CoreWindow + Direct3D 11): a VT/xterm terminal whose shell is
Alpine Linux under blink. Push a rootfs tar as `LocalState\rootfs.tar`
with `tools/xbox.py push`; `LocalState\command.txt` replaces the default
`/bin/bash -l`. Build with `cargo build --release` (targets
`x86_64-pc-windows-msvc`), deploy with `tools/xbox.py deploy` to a console
in Dev Mode.
