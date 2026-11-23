import type { GeneratedRun } from './abi.js';

const dispatch = 0;
const maxLinkedEntries = 64;
export const maxLinkedTargets = 8;
// These offsets are enforced by the SDK's Core constructor.
const stateOffsets = { rip: 136, remaining: 144, repeat: 168 };

const i32 = 0x7f;
const i64 = 0x7e;
const signature = [i32, i64, i64, i32, i32];
const result = [i32];
const op = {
  i32Eqz: 0x45,
  i32Eq: 0x46,
  i32Ne: 0x47,
  i32GeU: 0x4f,
  i64Eqz: 0x50,
  i64Eq: 0x51,
  i64GtU: 0x56,
  i32And: 0x71,
  i32Or: 0x72,
  i32Add: 0x6a,
  trap: 0x00,
  i64Or: 0x84,
  i64Shl: 0x86,
  i64ExtendI32U: 0xad,
  block: 0x02,
  loop: 0x03,
  if: 0x04,
  branch: 0x0c,
  end: 0x0b,
};
const local = {
  state: 0,
  budget: 1,
  stop: 2,
  hasStop: 3,
  entry: 4,
  slot: 5,
  repeatBefore: 6,
  remaining: 7,
  repeatNow: 8,
  rip: 9,
  reason: 10,
  iterations: 11,
  firstOrdinal: 12,
};

function uleb(value: number) {
  const bytes = [];
  do {
    const byte = value & 0x7f;
    value >>>= 7;
    bytes.push(byte | (value === 0 ? 0 : 0x80));
  } while (value !== 0);
  return bytes;
}

function sleb(value: bigint) {
  const bytes = [];
  for (;;) {
    const byte = Number(value & 0x7fn);
    value >>= 7n;
    const done = value === ((byte & 0x40) === 0 ? 0n : -1n);
    bytes.push(byte | (done ? 0 : 0x80));
    if (done) return bytes;
  }
}

const vec = (items: readonly (number | number[])[]) => [...uleb(items.length), ...items.flat()];
const name = (value: string) => vec([...new TextEncoder().encode(value)]);
const section = (id: number, contents: number[]) => [id, ...vec(contents)];
const type = (params: number[], results: number[]) => [0x60, ...vec(params), ...vec(results)];
const importFromEnv = (key: string, kind: number, description: number[]) => [
  ...name('env'),
  ...name(key),
  kind,
  ...description,
];
const body = (locals: number[][], instructions: number[]) =>
  vec([...vec(locals), ...instructions, op.end]);
const get = (index: number) => [0x20, ...uleb(index)];
const set = (index: number) => [0x21, ...uleb(index)];
const tee = (index: number) => [0x22, ...uleb(index)];
const call = (index: number) => [0x10, ...uleb(index)];
const imm = (value: number) => [0x41, ...sleb(BigInt(value))];
const word = (value: bigint) => [0x42, ...sleb(BigInt.asIntN(64, value))];
const read = (offset: number) => [...get(local.state), 0x29, 3, ...uleb(offset)];
const when = (instructions: number[]) => [op.if, 0x40, ...instructions, op.end];
const forwardedArgs = signature.flatMap((_, index) => get(index));

const setGlobal = (index: number) => [0x24, ...uleb(index)];
const receipt = { count: 0, lastUse: 1, invalidated: 2 };

export interface LinkTarget {
  readonly pc: bigint;
  readonly owner: bigint;
  readonly entry: number;
  readonly slot: number;
}

export interface LinkedRun {
  readonly runs: readonly GeneratedRun[];
  readonly count: WebAssembly.Global;
  readonly lastUse: WebAssembly.Global;
  readonly invalidated: WebAssembly.Global;
}

export function linkedRunBytes(
  runCount: number,
  targets: readonly LinkTarget[],
): Uint8Array<ArrayBuffer> {
  const activate = 1;
  const prepare = 0;
  const takeInvalidation = 2;
  const coreImports = [
    importFromEnv('prepare', 0, [1]),
    importFromEnv('activate', 0, [2]),
    importFromEnv('takeInvalidation', 0, [1]),
  ];
  const slots = Array.from({ length: runCount }, (_, slot) => slot);
  const firstRaw = coreImports.length;
  const driver = firstRaw + runCount;
  const firstWrapper = driver + 1;
  const rawImports = slots.map((index) => importFromEnv(`raw${index}`, 0, uleb(0)));
  const leave = (depth: number) => [op.branch, depth];
  const writeOrdinals = slots.flatMap((slot) => [
    ...get(local.firstOrdinal + slot),
    op.i64ExtendI32U,
    ...(slot === 0 ? [] : [...word(BigInt(slot * 8)), op.i64Shl, op.i64Or]),
  ]);
  // The receipt is only observable in JavaScript after the whole batch ends.
  const driverBody = body(
    [
      [4, i64],
      [2 + runCount, i32],
    ],
    [
      ...imm(0),
      ...setGlobal(receipt.invalidated),
      op.block,
      0x40,
      op.loop,
      0x40,
      ...get(local.iterations),
      ...imm(1),
      op.i32Add,
      ...set(local.iterations),
      ...read(stateOffsets.repeat),
      ...set(local.repeatBefore),
      ...slots.flatMap((index) => [
        ...get(local.slot),
        ...imm(index),
        op.i32Eq,
        ...when([
          ...get(local.iterations),
          ...set(local.firstOrdinal + index),
          ...forwardedArgs,
          ...call(firstRaw + index),
          ...set(local.reason),
        ]),
      ]),
      ...read(stateOffsets.remaining),
      ...tee(local.remaining),
      ...get(local.budget),
      op.i64GtU,
      ...read(stateOffsets.repeat),
      ...tee(local.repeatNow),
      ...get(local.repeatBefore),
      op.i64GtU,
      op.i32Or,
      ...get(local.remaining),
      ...get(local.budget),
      op.i64Eq,
      ...get(local.repeatNow),
      ...get(local.repeatBefore),
      op.i64Eq,
      op.i32And,
      ...get(local.reason),
      op.i32Eqz,
      op.i32And,
      op.i32Or,
      ...when([op.trap]),
      ...get(local.reason),
      ...imm(dispatch),
      op.i32Ne,
      ...when(leave(2)),
      ...call(takeInvalidation),
      ...when([...imm(1), ...setGlobal(receipt.invalidated), ...leave(2)]),
      ...get(local.remaining),
      op.i64Eqz,
      ...when(leave(2)),
      ...get(local.iterations),
      ...imm(maxLinkedEntries),
      op.i32GeU,
      ...when(leave(2)),
      ...read(stateOffsets.rip),
      ...tee(local.rip),
      ...get(local.stop),
      op.i64Eq,
      ...get(local.hasStop),
      op.i32And,
      ...when(leave(2)),
      ...targets.flatMap(({ pc, owner, entry, slot }) => [
        ...get(local.rip),
        ...word(pc),
        op.i64Eq,
        ...when([
          ...call(prepare),
          ...when(leave(3)),
          ...word(owner),
          ...call(activate),
          ...imm(1),
          op.i32Ne,
          ...when(leave(3)),
          ...get(local.remaining),
          ...set(local.budget),
          ...imm(entry),
          ...set(local.entry),
          ...imm(slot),
          ...set(local.slot),
          op.branch,
          1,
        ]),
      ]),
      ...leave(1),
      op.end,
      op.end,
      ...get(local.iterations),
      ...setGlobal(receipt.count),
      ...writeOrdinals,
      ...setGlobal(receipt.lastUse),
      ...get(local.reason),
    ],
  );
  const wrappers = slots.map((index) =>
    body([], [...forwardedArgs, ...imm(index), ...call(driver)]),
  );
  return Uint8Array.from([
    0x00,
    0x61,
    0x73,
    0x6d,
    0x01,
    0x00,
    0x00,
    0x00,
    ...section(
      1,
      vec([
        type(signature, result),
        type([], result),
        type([i64], result),
        type([...signature, i32], result),
      ]),
    ),
    ...section(2, vec([importFromEnv('memory', 2, [0, 0]), ...coreImports, ...rawImports])),
    ...section(3, vec([3, ...slots.map(() => 0)])),
    ...section(
      6,
      vec([
        [i32, 1, ...imm(0), op.end],
        [i64, 1, ...word(0n), op.end],
        [i32, 1, ...imm(0), op.end],
      ]),
    ),
    ...section(
      7,
      vec([
        ...wrappers.map((_, index) => [...name(`run${index}`), 0, ...uleb(firstWrapper + index)]),
        ...Object.entries(receipt).map(([key, index]) => [...name(key), 3, ...uleb(index)]),
      ]),
    ),
    ...section(10, vec([driverBody, ...wrappers])),
  ]);
}
