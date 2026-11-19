function wasmName(s: string): number[] {
  return [s.length, ...new TextEncoder().encode(s)];
}

function section(id: number, payload: number[]): number[] {
  if (payload.length >= 128) throw new Error('invalid parallel RAM helper encoding');
  return [id, payload.length, ...payload];
}

// memory 0 is the worker's private Core memory. All of these operations address memory 1.
export function ramModule(): WebAssembly.Module {
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
        i32,
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
      3,
      1,
      0x80,
      0x80,
      4,
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

export type RamHelpers = WebAssembly.Exports & {
  load_lane(offset: number): bigint;
  store_lane(offset: number, value: bigint): void;
  compare_exchange_lane(offset: number, previous: bigint, next: bigint): bigint;
  load_control(offset: number): number;
  store_control(offset: number, value: number): void;
  compare_exchange_control(offset: number, previous: number, next: number): number;
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
