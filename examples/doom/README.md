# doom

shareware DOOM (1993) running on ring64. the engine is [doomgeneric](https://github.com/ozkl/doomgeneric) compiled as a freestanding x86-64 program; ring64 translates its machine code to wasm while it runs, in node or in the browser.

## layout

- `guest/` builds `doom.elf`: a tiny C library (`libc/`), the doomgeneric platform layer (`doomgeneric_ring64.c`), the entry point (`start.S`) and the linker script.
- `host/doom-machine.js` loads the elf into a ring64 cpu and services the guest.
- `host/run-node.mjs` plays headless in node and writes frames as png.
- `web/index.html` is the playable page. `host/stage-web.mjs` stages it with the bundled sdk in `build/web/`.

## guest interface

the guest runs in long mode with linear memory:

| address      | contents                                    | permissions |
| ------------ | ------------------------------------------- | ----------- |
| `0x00400000` | `.text`, `.rodata`                          | r-x         |
| `0x01000000` | `.data`, `.bss`, heap, 1 MiB stack (64 MiB) | rw-         |
| `0x10000000` | `doom1.wad`                                 | r--         |

frequent requests use port i/o, which never leaves translated code: `in` from port `0x70` reads the millisecond clock, `in` from `0x72` pops a key event and `out` to `0xe9` writes the console.

requests that need guest memory or a turn of the host event loop are hypercalls. the guest calls `ring64_hypercall` at `0x400000`; the host runs with `stop: 0x400000n`, services the call while the cpu is idle (`readMemory`, `writeMemory`) and performs the `ret` with `setState`. hypercalls draw a frame (320x200 palette indices plus the palette), sleep, set the title, copy the command line and exit.

## build and run

needs gcc (or clang), node 22 and a built ring64 (`npm run build` in the repository root). the shareware iwad is not in the repository; put `doom1.wad` (v1.9, md5 `f0cefca49926d00903cf57551d901abe`) in `build/`.

```sh
guest/build.sh              # clones doomgeneric into .cache/ and links build/doom.elf
node host/run-node.mjs --seconds 20
node host/stage-web.mjs
node host/serve.mjs         # http://localhost:8064/
```

`run-node.mjs` takes doom arguments after `--`, for example `-- -timedemo demo1`, or `-- -warp 1 3`.

## notes

- ubuntu's gcc emits `endbr64` by default, which ring64 reports as `unsupported`. the guest builds with `-fcf-protection=none`.
- translation size matters. with `maxInstructions: 2048` the first frame took 14 s, almost all of it in `WebAssembly.compile` of large artifacts. `256` instructions and `4096` source bytes start in about a second and keep the highest frame rate in `-timedemo demo1`.
- doom renders 35 tics per second. measured on this example's development container, `-timedemo demo1` ran at about 130 frames per second in node and 60 to 100 in headless chromium.
