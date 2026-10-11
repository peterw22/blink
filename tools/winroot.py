#!/usr/bin/env python3
"""Copies a Linux root filesystem into the form blink-xbox's Windows host
layer reads (src/win/fs.rs): symlinks become small marker files
("!<blink-symlink>\\n" + target) with the Windows SYSTEM attribute, since
UWP apps can't create or read real symlinks.

For Wine testing on Linux, the SYSTEM attribute is set the way Wine stores
DOS attributes: a user.DOSATTRIB extended attribute. Special files (device
nodes, fifos, sockets) are skipped.

usage: winroot.py SRC DEST
"""
import os
import shutil
import stat
import sys

MAGIC = b"!<blink-symlink>\n"


def convert(src, dest):
    links = files = 0
    for root, dirs, names in os.walk(src):
        rel = os.path.relpath(root, src)
        out = os.path.join(dest, rel) if rel != "." else dest
        os.makedirs(out, exist_ok=True)
        for name in dirs + names:
            s = os.path.join(root, name)
            d = os.path.join(out, name)
            st = os.lstat(s)
            if stat.S_ISLNK(st.st_mode):
                if name in dirs:
                    dirs.remove(name)  # don't descend through it
                if os.path.lexists(d):
                    os.remove(d)
                with open(d, "wb") as f:
                    f.write(MAGIC + os.readlink(s).encode())
                os.setxattr(d, "user.DOSATTRIB", b"0x4")
                links += 1
            elif stat.S_ISREG(st.st_mode):
                shutil.copy2(s, d)
                files += 1
    print(f"{files} files, {links} symlinks -> {dest}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    convert(sys.argv[1], sys.argv[2])
