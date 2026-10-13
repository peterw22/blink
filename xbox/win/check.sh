#!/bin/bash
# Syntax-checks every blink C file for x86_64-pc-windows-msvc against the
# Windows config and POSIX shim headers. usage: check.sh [file.c...]
# Prints "file: first error" per failing file and a total.
here=$(cd "$(dirname "$0")" && pwd)
blink=${BLINK_DIR:-$here/../../blink}
X=$HOME/.local/share/xwin
flags=(--target=x86_64-pc-windows-msvc -fms-compatibility-version=19.44 -fgnuc-version=4.2.1
  -std=gnu11 -w -fsyntax-only -include "$here/include/blinkwin.h"
  -I "$here" -I "$here/include" -I "$blink" -I "$blink/third_party/libz"
  -isystem "$X/crt/include" -isystem "$X/sdk/include/ucrt"
  -isystem "$X/sdk/include/um" -isystem "$X/sdk/include/shared"
  -D_FILE_OFFSET_BITS=64)
files=("$@"); [ ${#files[@]} -eq 0 ] && files=("$blink"/blink/*.c)
fail=0
for f in "${files[@]}"; do
  [ "$(basename "$f")" = blinkenlights.c ] && continue
  out=$(clang "${flags[@]}" "$f" 2>&1 | grep -E "error:" | head -${ERRS:-1})
  if [ -n "$out" ]; then fail=$((fail+1)); echo "$out" | sed "s|^$blink/||"; fi
done
echo "failing files: $fail"
