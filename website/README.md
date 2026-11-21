# Ring64 website

Run these commands from the repository root. The build uses Node.js, the repository's Rust toolchain, Clang, and an ELF-compatible `objcopy`. Rust's `llvm-tools` component supplies `llvm-objcopy` when it is missing from the system.

```sh
npm ci
rustup component add llvm-tools
npm run site:dev
```

The local preview listens on `http://127.0.0.1:4173`. `RING64_SITE_PORT` overrides the port.

`npm run site:build` writes the static site to `build/website/`. This also builds the Ring64 SDK and compiles the Wet Ink x86-64 guest from `wet-ink/ink.c`.
