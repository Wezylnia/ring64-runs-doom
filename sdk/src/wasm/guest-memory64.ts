// Both memories live in Wasm. Only allocation and reclamation enter JavaScript.
// read/write use memory.copy, whose length is i32 when either memory is memory32.
const i32 = 0x7f;
const i64 = 0x7e;
const functions = [
  { name: 'read', params: [i32, i64, i32], op: [0xfc, 10, 0, 1] },
  { name: 'write', params: [i64, i32, i32], op: [0xfc, 10, 1, 0] },
  { name: 'clear', params: [i64, i32, i64], op: [0xfc, 11, 1] },
];

function name(text: string): number[] {
  return [text.length, ...new TextEncoder().encode(text)];
}

function section(id: number, bytes: number[]): number[] {
  if (bytes.length >= 128) throw new Error('memory64 helper section too large');
  return [id, bytes.length, ...bytes];
}

let module: WebAssembly.Module | undefined;
function copyModule(): WebAssembly.Module {
  return (module ??= new WebAssembly.Module(
    Uint8Array.from([
      0,
      97,
      115,
      109,
      1,
      0,
      0,
      0,
      ...section(1, [
        functions.length,
        ...functions.flatMap(({ params }) => [0x60, params.length, ...params, 0]),
      ]),
      ...section(2, [1, ...name('env'), ...name('memory'), 2, 0, 32]),
      ...section(3, [functions.length, ...functions.map((_, i) => i)]),
      ...section(5, [1, 4, 0]), // one memory64 with zero initial pages
      ...section(7, [
        functions.length + 1,
        ...name('ram'),
        2,
        1,
        ...functions.flatMap((f, i) => [...name(f.name), 0, i]),
      ]),
      ...section(10, [
        functions.length,
        ...functions.flatMap(({ op }) => {
          const body = [0, 0x20, 0, 0x20, 1, 0x20, 2, ...op, 0x0b];
          return [body.length, ...body];
        }),
      ]),
    ]),
  ));
}

export function bindGuestMemory64() {
  const memory = new WebAssembly.Memory({ initial: 32 });
  const helpers = new WebAssembly.Instance(copyModule(), { env: { memory } }).exports;
  const ram = helpers.ram as WebAssembly.Memory;
  const clear = helpers.clear as (start: bigint, zero: number, length: bigint) => void;
  const extents: { base: bigint; end: bigint }[] = [];
  const page = 65536n;
  let pages = 0n;
  let highWater = 0n;

  function allocate(length: bigint): bigint {
    let base = 0n;
    let index = 0;
    for (const extent of extents) {
      if (base + length <= extent.base) break;
      base = extent.end;
      index++;
    }
    const end = base + length;
    const needed = (end + page - 1n) / page;
    if (needed > pages) {
      try {
        // JS engines accepting memory64 use bigint for its page counts.
        (ram.grow as unknown as (delta: bigint) => bigint)(needed - pages);
      } catch (error) {
        if (error instanceof RangeError) return -1n;
        throw error;
      }
      pages = needed;
    }
    if (base < highWater) clear(base, 0, (end < highWater ? end : highWater) - base);
    if (end > highWater) highWater = end;
    extents.splice(index, 0, { base, end });
    return base;
  }

  function release(base: bigint): void {
    const index = extents.findIndex((extent) => extent.base === base);
    if (index < 0) throw new Error('unknown memory64 backing');
    extents.splice(index, 1);
  }
  return {
    env: { memory },
    guest64: { ram, allocate, release, read: helpers.read!, write: helpers.write! },
  };
}
