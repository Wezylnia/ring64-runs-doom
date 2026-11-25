#!/usr/bin/env bash
# Builds doom.elf: doomgeneric as a freestanding x86-64 program for ring64.
#
# usage: guest/build.sh [path/to/doomgeneric/doomgeneric]
# The doomgeneric sources (https://github.com/ozkl/doomgeneric) are fetched
# at a fixed commit into .cache/ when no path is given.
set -euo pipefail

doomgeneric_repo=https://github.com/ozkl/doomgeneric
doomgeneric_commit=e257a09527062bfb986b6e5e0780839b6cbd5b71

here="$(cd "$(dirname "$0")" && pwd)"
root="$(dirname "$here")"
cache="$root/.cache"
src="${1:-}"
if [ -z "$src" ]; then
  checkout="$cache/doomgeneric"
  if [ "$(git -C "$checkout" rev-parse HEAD 2>/dev/null)" != "$doomgeneric_commit" ]; then
    rm -rf "$checkout"
    git init -q "$checkout"
    git -C "$checkout" fetch -q --depth 1 "$doomgeneric_repo" "$doomgeneric_commit"
    git -C "$checkout" checkout -q FETCH_HEAD
  fi
  src="$checkout/doomgeneric"
fi

out="$root/build"
obj="$out/obj"
mkdir -p "$obj"

CC="${CC:-gcc}"
CFLAGS=(
  -m64 -O2 -g0 -std=gnu99
  -ffreestanding -fno-builtin-printf -nostdinc
  -isystem "$here/libc/include" -isystem "$($CC -print-file-name=include)"
  -I "$here/libc" -I "$src"
  -fno-pic -fno-pie -fno-stack-protector -fno-asynchronous-unwind-tables -fno-unwind-tables
  -fno-common -ffunction-sections -fdata-sections
  -mno-red-zone -fcf-protection=none
  -DCMAP256 -DDOOMGENERIC_RESX=320 -DDOOMGENERIC_RESY=200 -DNORMALUNIX -DLINUX
  -Werror=implicit-function-declaration -Werror=int-conversion -Wno-pointer-to-int-cast -Wno-int-to-pointer-cast -Wno-format -Wno-unused-result -Wno-deprecated-declarations
)

doom_sources=(
  dummy am_map doomdef doomstat dstrings d_event d_items d_iwad d_loop d_main d_mode d_net
  f_finale f_wipe g_game hu_lib hu_stuff info i_cdmus i_endoom i_joystick i_scale i_sound
  i_system i_timer memio m_argv m_bbox m_cheat m_config m_controls m_fixed m_menu m_misc
  m_random p_ceilng p_doors p_enemy p_floor p_inter p_lights p_map p_maputl p_mobj p_plats
  p_pspr p_saveg p_setup p_sight p_spec p_switch p_telept p_tick p_user r_bsp r_data r_draw
  r_main r_plane r_segs r_sky r_things sha1 sounds statdump st_lib st_stuff s_sound tables
  v_video wi_stuff w_checksum w_file w_main w_wad z_zone w_file_stdc i_input i_video
  doomgeneric
)

objects=()
pids=()
for name in "${doom_sources[@]}"; do
  "$CC" "${CFLAGS[@]}" -c "$src/$name.c" -o "$obj/$name.o" &
  pids+=($!)
  objects+=("$obj/$name.o")
done
"$CC" "${CFLAGS[@]}" -c "$here/libc/libc.c" -o "$obj/libc.o" & pids+=($!)
"$CC" "${CFLAGS[@]}" -c "$here/doomgeneric_ring64.c" -o "$obj/doomgeneric_ring64.o" & pids+=($!)
"$CC" -m64 -c "$here/start.S" -o "$obj/start.o" & pids+=($!)
for pid in "${pids[@]}"; do wait "$pid"; done
objects+=("$obj/libc.o" "$obj/doomgeneric_ring64.o" "$obj/start.o")

"$CC" -m64 -nostdlib -static -no-pie -Wl,--gc-sections -Wl,-T,"$here/linker.ld" \
  -Wl,-z,noexecstack -Wl,--build-id=none \
  "${objects[@]}" -lgcc -o "$out/doom.elf"

size "$out/doom.elf"
