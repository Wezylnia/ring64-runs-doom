#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  printf '%s\n' 'Usage: build.sh OUTPUT_DIRECTORY' >&2
  exit 2
fi

mkdir -p "$1"
output_dir=$(CDPATH= cd -- "$1" && pwd)
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
scratch_dir=$(mktemp -d "${TMPDIR:-/tmp}/ring64-wet-ink.XXXXXX")
trap 'rm -rf "$scratch_dir"' EXIT
cd "$source_dir"

toolchain=$(rustc --print sysroot)
host=$(rustc -vV | sed -n 's/^host: //p')
tools=$toolchain/lib/rustlib/$host/bin
objcopy=$tools/llvm-objcopy
if [ ! -x "$objcopy" ]; then
  objcopy=$(command -v llvm-objcopy || command -v objcopy || true)
  if [ -z "$objcopy" ]; then
    printf '%s\n' 'Wet Ink needs llvm-objcopy. Install it with rustup component add llvm-tools.' >&2
    exit 1
  fi
fi

clang --target=x86_64-unknown-none-elf -O3 -Wall -Wextra -Werror -fPIC -ffreestanding -fno-builtin \
  -fno-stack-protector -fno-vectorize -fno-slp-vectorize -fno-math-errno \
  -mno-avx -mno-red-zone -c ink.c -o "$scratch_dir/ink.o"
"$tools/rust-lld" -flavor gnu -T guest.ld -o "$scratch_dir/ink.elf" "$scratch_dir/ink.o"
"$objcopy" --only-section=.guest -O binary "$scratch_dir/ink.elf" "$output_dir/wet-ink.bin"
