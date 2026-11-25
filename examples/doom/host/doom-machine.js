// Runs the doomgeneric guest (guest/doom.elf) on a ring64 CPU.
//
// The same module drives the browser page and the Node runner: callers pass
// in ring64's createCpu, a compiled ring64 core, the ELF and the WAD bytes,
// then call `slice()` repeatedly. Each slice runs guest code until it draws a
// frame, asks to sleep, exits or uses up its instruction budget.

const HYPERCALL = 0x400000n; // first byte of .text, see guest/linker.ld
const RAM_BASE = 0x1000000n;
const RAM_SIZE = 0x4000000; // 64 MiB: data, bss, heap and stack
const WAD_BASE = 0x10000000n;

const PORT_CONSOLE = 0xe9;
const PORT_TICKS = 0x70;
const PORT_KEY = 0x72;

const HC_EXIT = 1n;
const HC_FRAME = 2n;
const HC_SLEEP = 3n;
const HC_TITLE = 4n;
const HC_ARGS = 5n;

export const SCREEN_WIDTH = 320;
export const SCREEN_HEIGHT = 200;

const PAGE = 4096;
const roundPage = (n) => Math.ceil(n / PAGE) * PAGE;

function parseElf(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(0, false) !== 0x7f454c46 || bytes[4] !== 2 || bytes[5] !== 1)
    throw new Error('doom.elf must be a little-endian ELF64 file');
  const entry = view.getBigUint64(0x18, true);
  const phoff = Number(view.getBigUint64(0x20, true));
  const phentsize = view.getUint16(0x36, true);
  const phnum = view.getUint16(0x38, true);
  const segments = [];
  for (let i = 0; i < phnum; i++) {
    const at = phoff + i * phentsize;
    if (view.getUint32(at, true) !== 1) continue; // PT_LOAD
    const flags = view.getUint32(at + 4, true);
    const offset = Number(view.getBigUint64(at + 8, true));
    const vaddr = view.getBigUint64(at + 16, true);
    const filesz = Number(view.getBigUint64(at + 32, true));
    const memsz = Number(view.getBigUint64(at + 40, true));
    segments.push({
      vaddr,
      memsz,
      bytes: bytes.subarray(offset, offset + filesz),
      execute: (flags & 1) !== 0,
      write: (flags & 2) !== 0,
    });
  }
  return { entry, segments };
}

export const defaultLimits = {
  maxInstructions: 256,
  maxSourceBytes: 4096,
  maxArtifactBytes: 512 * 1024,
  maxCachedArtifacts: 16384,
  maxCachedBytes: 256 * 1024 * 1024,
  maxTranslationsPerRun: 512,
};

export class DoomMachine {
  /**
   * @param {object} options
   * @param {Function} options.createCpu ring64's createCpu
   * @param {WebAssembly.Module} options.compiler result of loadCpuCompiler()
   * @param {Uint8Array} options.elf guest/doom.elf
   * @param {Uint8Array} options.wad doom1.wad
   * @param {string[]} [options.args] extra DOOM command line arguments
   */
  static async create({ createCpu, compiler, elf, wad, args = [], limits = defaultLimits, now }) {
    const { entry, segments } = parseElf(elf);
    const code = segments.find((s) => s.execute);
    const data = segments.find((s) => s.write);
    if (!code || !data || data.vaddr !== RAM_BASE) throw new Error('unexpected doom.elf layout');

    const codeBytes = new Uint8Array(roundPage(code.memsz));
    codeBytes.set(code.bytes);
    const ramBytes = new Uint8Array(RAM_SIZE);
    ramBytes.set(data.bytes);
    const wadBytes = new Uint8Array(roundPage(wad.length));
    wadBytes.set(wad);

    const machine = new DoomMachine(now ?? (() => performance.now()));
    const cpu = await createCpu({
      compiler,
      limits,
      io: machine.#io,
      entry,
      memory: {
        backings: [
          { id: 'code', bytes: codeBytes },
          { id: 'ram', bytes: ramBytes },
          { id: 'wad', bytes: wadBytes },
        ],
        mappings: [
          {
            base: code.vaddr,
            backing: 'code',
            offset: 0,
            length: codeBytes.length,
            permissions: { read: true, write: false, execute: true },
          },
          {
            base: RAM_BASE,
            backing: 'ram',
            offset: 0,
            length: RAM_SIZE,
            permissions: { read: true, write: true, execute: false },
          },
          {
            base: WAD_BASE,
            backing: 'wad',
            offset: 0,
            length: wadBytes.length,
            permissions: { read: true, write: false, execute: false },
          },
        ],
      },
      memoryLimits: {
        maxBackingBytes: codeBytes.length + RAM_SIZE + wadBytes.length,
        maxBackings: 3,
        maxMappings: 3,
      },
    });
    machine.#cpu = cpu;
    machine.#args = ['doom', '-iwad', 'doom1.wad', ...args, `@wadsize=${wad.length}`];
    return machine;
  }

  #cpu;
  #args = [];
  #now;
  #start;
  #keys = [];
  #line = '';

  /** Called with (rgba: Uint8ClampedArray) for every finished frame. */
  onFrame = () => {};
  onTitle = () => {};
  onLog = () => {};

  exitCode = undefined;
  stats = { runs: 0, timings: {}, resources: {}, reasons: {} };
  frames = 0;
  retired = 0n;

  constructor(now) {
    this.#now = now;
    this.#start = now();
  }

  /** Queue a key transition. `key` is a doomkeys.h code. */
  key(key, pressed) {
    this.#keys.push(0x8000 | (pressed ? 0x100 : 0) | (key & 0xff));
  }

  #io = {
    read: (port, width) => {
      if (port === PORT_TICKS && width === 32) return Math.floor(this.#now() - this.#start) >>> 0;
      if (port === PORT_KEY && width === 16) return this.#keys.shift() ?? 0;
      return 0;
    },
    write: (port, width, value) => {
      if (port !== PORT_CONSOLE || width !== 8) return;
      if (value === 10) {
        this.onLog(this.#line);
        this.#line = '';
      } else {
        this.#line += String.fromCharCode(value);
      }
    },
  };

  #rgba = new Uint8ClampedArray(SCREEN_WIDTH * SCREEN_HEIGHT * 4);

  #drawFrame(pixelsAddress, paletteAddress) {
    const cpu = this.#cpu;
    const pixels = cpu.readMemory(pixelsAddress, SCREEN_WIDTH * SCREEN_HEIGHT);
    const palette = cpu.readMemory(paletteAddress, 256 * 4); // {b, g, r, a} per entry
    const rgba = this.#rgba;
    for (let i = 0, o = 0; i < pixels.length; i++, o += 4) {
      const p = pixels[i] * 4;
      rgba[o] = palette[p + 2];
      rgba[o + 1] = palette[p + 1];
      rgba[o + 2] = palette[p];
      rgba[o + 3] = 255;
    }
    this.frames++;
    this.onFrame(rgba);
  }

  #readString(address, max = 256) {
    const bytes = this.#cpu.readMemory(address, max);
    const end = bytes.indexOf(0);
    return new TextDecoder().decode(bytes.subarray(0, end < 0 ? max : end));
  }

  /** Services the hypercall the CPU stopped at and returns what it was. */
  #hypercall() {
    const cpu = this.#cpu;
    const state = cpu.readState();
    const r = state.registers;
    const [nr, a, b] = [r[7], r[6], r[2]]; // rdi, rsi, rdx
    let result = 0n;
    let event = 'call';
    switch (nr) {
      case HC_FRAME:
        this.#drawFrame(a, b);
        event = 'frame';
        break;
      case HC_SLEEP:
        event = 'sleep';
        result = a;
        break;
      case HC_TITLE:
        this.onTitle(this.#readString(a));
        break;
      case HC_ARGS: {
        const text = new TextEncoder().encode(this.#args.join('\0') + '\0');
        const length = Math.min(text.length, Number(b));
        cpu.writeMemory(a, text.subarray(0, length));
        result = BigInt(length);
        break;
      }
      case HC_EXIT:
        this.exitCode = Number(BigInt.asIntN(32, a));
        if (this.#line) this.onLog(this.#line);
        return { event: 'exit', sleepMs: 0 };
      default:
        throw new Error(`unknown hypercall ${nr}`);
    }
    // Perform the guest's `ret`.
    const rsp = r[4];
    const ret = new DataView(cpu.readMemory(rsp, 8).buffer).getBigUint64(0, true);
    r[0] = result;
    r[4] = rsp + 8n;
    state.rip = ret;
    cpu.setState(state);
    return { event, sleepMs: event === 'sleep' ? Number(a) : 0 };
  }

  /**
   * Runs the guest until it finishes a frame, sleeps or spends `budget`
   * instructions. Returns { event: 'frame' | 'sleep' | 'budget' | 'exit' }.
   */
  async slice(budget = 50_000_000n) {
    if (this.exitCode !== undefined) return { event: 'exit', sleepMs: 0 };
    for (;;) {
      const result = await this.#cpu.run({ budget, stop: HYPERCALL });
      this.retired += result.retired;
      budget -= result.retired;
      this.stats.runs++;
      for (const [k, v] of Object.entries(result.timings))
        this.stats.timings[k] = (this.stats.timings[k] ?? 0) + v;
      for (const [k, v] of Object.entries(result.resources))
        if (typeof v === 'number') this.stats.resources[k] = (this.stats.resources[k] ?? 0) + v;
      this.stats.reasons[result.reason] = (this.stats.reasons[result.reason] ?? 0) + 1;
      if (result.reason === 'stop') {
        const handled = this.#hypercall();
        if (handled.event !== 'call') return handled;
        continue;
      }
      if (result.reason === 'budget') return { event: 'budget', sleepMs: 0 };
      // Translation resource limits only end this run; the next run continues.
      if (result.reason === 'resource-limit') {
        if (budget <= 0n) return { event: 'budget', sleepMs: 0 };
        continue;
      }
      const rip = result.state.rip.toString(16);
      const detail = result.fault
        ? ` ${JSON.stringify(result.fault, (_, v) => (typeof v === 'bigint' ? `0x${v.toString(16)}` : v))}`
        : '';
      let bytes = '';
      try {
        const code = this.#cpu.readMemory(result.state.rip, 16);
        bytes = ` bytes ${[...code].map((b) => b.toString(16).padStart(2, '0')).join(' ')}`;
      } catch {
        // rip is outside readable memory
      }
      throw new Error(`guest stopped with ${result.reason} at 0x${rip}${bytes}${detail}`);
    }
  }

  dispose() {
    this.#cpu?.dispose();
  }
}
