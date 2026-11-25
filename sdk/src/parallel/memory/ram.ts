function wasmName(s: string): number[] {
  return [s.length, ...new TextEncoder().encode(s)];
}

function section(id: number, payload: number[]): number[] {
  if (payload.length >= 128) throw new Error('invalid parallel RAM helper encoding');
  return [id, payload.length, ...payload];
}

export const MAX_SHARED_MEMORY64_PAGES = 262144;

function unsignedLeb128(value: number): number[] {
  const bytes = [];
  while (value >= 128) {
    bytes.push((value % 128) | 128);
    value = Math.floor(value / 128);
  }
  bytes.push(value);
  return bytes;
}

export function usesParallelMemory64(compiler: WebAssembly.Module): boolean {
  return WebAssembly.Module.exports(compiler).some(
    ({ name, kind }) => name === 'core_parallel_memory64' && kind === 'function',
  );
}

// memory 0 is the worker's private Core memory. All of these operations address memory 1.
export function ramModule(memory64 = false): WebAssembly.Module {
  const i32 = 0x7f;
  const i64 = 0x7e;
  const helpers = [
    { name: 'load_lane', operands: 1, opcode: 0x11, align: 3, value: i64 },
    { name: 'store_lane', operands: 2, opcode: 0x18, align: 3, value: i64 },
    { name: 'compare_exchange_lane', operands: 3, opcode: 0x49, align: 3, value: i64 },
    { name: 'load_control', operands: 1, opcode: 0x10, align: 2, value: i32 },
    { name: 'store_control', operands: 2, opcode: 0x17, align: 2, value: i32 },
    { name: 'compare_exchange_control', operands: 3, opcode: 0x48, align: 2, value: i32 },
  ];
  const count = helpers.length;
  const bytes = Uint8Array.from([
    0,
    97,
    115,
    109,
    1,
    0,
    0,
    0,
    ...section(1, [
      count,
      ...helpers.flatMap(({ operands, value }) => [
        0x60,
        operands,
        memory64 ? i64 : i32,
        ...Array<number>(operands - 1).fill(value),
        ...(operands === 2 ? [0] : [1, value]),
      ]),
    ]),
    ...section(2, [
      2,
      ...wasmName('env'),
      ...wasmName('private'),
      2,
      0,
      1,
      ...wasmName('env'),
      ...wasmName('shared'),
      2,
      memory64 ? 7 : 3,
      1,
      ...unsignedLeb128(memory64 ? MAX_SHARED_MEMORY64_PAGES : 65536),
    ]),
    ...section(3, [count, ...helpers.map((_, index) => index)]),
    ...section(7, [count, ...helpers.flatMap(({ name }, index) => [...wasmName(name), 0, index])]),
    ...section(10, [
      count,
      ...helpers.flatMap(({ operands, opcode, align }) => {
        const body = [
          0,
          ...Array.from({ length: operands }, (_, index) => [0x20, index]).flat(),
          0xfe,
          opcode,
          align | 0x40,
          1,
          0,
          0x0b,
        ];
        return [body.length, ...body];
      }),
    ]),
  ]);
  return new WebAssembly.Module(bytes);
}

export function sharedRam(pages: number, memory64: boolean): WebAssembly.Memory {
  if (!memory64) return new WebAssembly.Memory({ initial: pages, maximum: pages, shared: true });
  // The Wasm memory type also works on hosts that cannot construct memory64 from JS.
  const count = unsignedLeb128(pages);
  const bytes = Uint8Array.from([
    0,
    97,
    115,
    109,
    1,
    0,
    0,
    0,
    ...section(5, [1, 7, ...count, ...count]),
    ...section(7, [1, ...wasmName('ram'), 2, 0]),
  ]);
  return new WebAssembly.Instance(new WebAssembly.Module(bytes)).exports.ram as WebAssembly.Memory;
}

let memory64Supported: boolean | undefined;

export function supportsParallelMemory64(): boolean {
  if (memory64Supported !== undefined) return memory64Supported;
  try {
    const memory = sharedRam(1, true);
    const helpers = bindRam(ramModule(true), new WebAssembly.Memory({ initial: 1 }), memory);
    memory64Supported = helpers.compare_exchange_lane(0n, 0n, 1n) === 0n;
  } catch (error) {
    if (
      !(error instanceof WebAssembly.CompileError) &&
      !(error instanceof WebAssembly.LinkError) &&
      !(error instanceof WebAssembly.RuntimeError) &&
      !(error instanceof RangeError)
    )
      throw error;
    memory64Supported = false;
  }
  return memory64Supported;
}

type RamOffset = number | bigint;

export function wasmRamOffset(offset: number, memory64: boolean): RamOffset {
  return memory64 ? BigInt(offset) : offset;
}

export type RamHelpers = WebAssembly.Exports & {
  load_lane(offset: RamOffset): bigint;
  store_lane(offset: RamOffset, value: bigint): void;
  compare_exchange_lane(offset: RamOffset, previous: bigint, next: bigint): bigint;
  load_control(offset: RamOffset): number;
  store_control(offset: RamOffset, value: number): void;
  compare_exchange_control(offset: RamOffset, previous: number, next: number): number;
};

export function bindRam(
  module: WebAssembly.Module,
  privateMemory: WebAssembly.Memory,
  sharedMemory: WebAssembly.Memory,
): RamHelpers {
  return new WebAssembly.Instance(module, {
    env: { private: privateMemory, shared: sharedMemory },
  }).exports as RamHelpers;
}
