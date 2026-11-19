import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createCpu, type Cpu, type Options } from '../index.js';
import { longSnapshot } from '../../tests/support/context.js';
import { captureProtectionKeysState, validateProtectionKeysLayout } from './protection-keys.js';

const compiler = await WebAssembly.compile(
  new Uint8Array(await readFile(new URL('../../build/ring64.wasm', import.meta.url))),
);

function options(
  model: 'linear' | 'ia32e-paged' | 'physical' = 'linear',
  code = Uint8Array.of(0x90, 0x90),
): Options {
  const common = {
    compiler,
    entry: 0x1000n,
    limits: {
      maxInstructions: 32,
      maxSourceBytes: 256,
      maxArtifactBytes: 65536,
      maxCachedArtifacts: 8,
      maxCachedBytes: 1048576,
      maxTranslationsPerRun: 8,
    },
    memoryLimits: { maxBackings: 1, maxBackingBytes: 64, maxMappings: 1 },
    memory: {
      backings: [{ id: 'code', bytes: code }],
      mappings: [
        {
          base: 0x1000n,
          backing: 'code',
          offset: 0,
          length: code.length,
          permissions: { read: true, write: true, execute: true },
        },
      ],
    },
  };
  const paging = { cr0: 0x80000033n, cr2: 0n, cr3: 0n, cr4: 0x628n, efer: 0x500n };
  if (model === 'physical')
    return { ...common, memoryModel: model, paging, context: longSnapshot() };
  return model === 'ia32e-paged' ? { ...common, memoryModel: model, paging } : common;
}

function otherState(cpu: Cpu, model: 'linear' | 'ia32e-paged' | 'physical' = 'linear') {
  return {
    scalar: cpu.readState(),
    context: cpu.readContext(),
    debug: cpu.readDebugState(),
    xmm: cpu.readXmmState(),
    x87: cpu.readX87State(),
    mxcsr: cpu.readMxcsr(),
    time: cpu.readTimeState(),
    paging: model === 'linear' ? undefined : cpu.readPagingState(),
    fastSystem: model === 'linear' ? undefined : cpu.readFastSystemState(),
    artifacts: cpu.inspectTranslations(),
  };
}

test('protection-key capture owns two primitive uint32 words without enumerating input', () => {
  assert.deepEqual(captureProtectionKeysState({ pkru: 0xffffffff, pkrs: 0x80000000 }), {
    pkru: 0xffffffff,
    pkrs: 0x80000000,
  });
  const zero = captureProtectionKeysState({ pkru: -0, pkrs: -0 });
  assert.equal(Object.is(zero.pkru, -0), false);
  assert.equal(Object.is(zero.pkrs, -0), false);
  const invalid: unknown[] = [
    undefined,
    null,
    [],
    Object.assign([], { pkru: 0, pkrs: 0 }),
    {},
    { pkru: 0 },
    { pkrs: 0 },
  ];
  for (const word of [-1, 2 ** 32, 0.5, NaN, Infinity, 0n, '0', Object(0), null, undefined])
    invalid.push({ pkru: word, pkrs: 0 }, { pkru: 0, pkrs: word });
  for (const input of invalid)
    assert.throws(() => captureProtectionKeysState(input as never), TypeError);
  const reads: PropertyKey[] = [];
  const input = new Proxy(
    { pkru: 1, pkrs: 0xffffffff },
    {
      get(target, key, receiver) {
        reads.push(key);
        return Reflect.get(target, key, receiver);
      },
      ownKeys() {
        return assert.fail('capture must not enumerate unrelated properties');
      },
    },
  );
  assert.deepEqual(captureProtectionKeysState(input), { pkru: 1, pkrs: 0xffffffff });
  assert.deepEqual(reads, ['pkru', 'pkrs']);
});

test('protection-key layout accepts descriptor ordering and rejects overlap or truncated spans', () => {
  const layout = { offset: 784, size: 8, pkru: 0, pkrs: 4 };
  validateProtectionKeysLayout(layout, 792, 784);
  validateProtectionKeysLayout({ ...layout, pkru: 4, pkrs: 0 }, 792, 784);
  for (const changed of [
    { offset: 783 },
    { offset: 785 },
    { size: 7 },
    { size: 9 },
    { pkru: 1 },
    { pkrs: 3 },
    { pkrs: 5 },
    { pkru: -1 },
    { pkru: 0.5 },
    { offset: NaN },
    { pkrs: Infinity },
    { offset: 2 ** 32 },
  ])
    assert.throws(
      () => validateProtectionKeysLayout({ ...layout, ...changed }, 792, 784),
      /protection keys layout/,
    );
  for (const invalid of [null, undefined, [], Object.assign([], layout), {}])
    assert.throws(
      () => validateProtectionKeysLayout(invalid as never, 792, 784),
      /protection keys layout/,
    );
  assert.throws(() => validateProtectionKeysLayout(layout, 791, 784), /protection keys layout/);
});

test('protection-key bank starts with its own zero pair in every memory model', async () => {
  for (const model of ['linear', 'ia32e-paged', 'physical'] as const) {
    const cpu = await createCpu(options(model));
    try {
      assert.equal(typeof cpu.readProtectionKeysState, 'function', 'public key bank is available');
      assert.deepEqual(cpu.readProtectionKeysState(), { pkru: 0, pkrs: 0 });
    } finally {
      cpu.dispose();
    }
  }
});

test('protection-key imports own detached words and preserve other banks in every model', async () => {
  for (const model of ['linear', 'ia32e-paged', 'physical'] as const) {
    const cpu = await createCpu(options(model));
    try {
      const before = otherState(cpu, model);
      const source = { pkru: 0x80000001, pkrs: 0xffffffff };
      cpu.setProtectionKeysState(source);
      source.pkru = 0;
      const detached = cpu.readProtectionKeysState();
      detached.pkrs = 0;
      assert.deepEqual(cpu.readProtectionKeysState(), { pkru: 0x80000001, pkrs: 0xffffffff });
      assert.deepEqual(otherState(cpu, model), before);
      cpu.setState(cpu.readState());
      cpu.setContext(cpu.readContext());
      cpu.setDebugState(cpu.readDebugState());
      cpu.setXmmState(cpu.readXmmState());
      cpu.setX87State(cpu.readX87State());
      cpu.setMxcsr(cpu.readMxcsr());
      cpu.setTimeState({ tsc: 123n, tscAux: 7 });
      if (model !== 'linear') {
        cpu.setPagingState(cpu.readPagingState());
        cpu.setFastSystemState(cpu.readFastSystemState());
      }
      assert.deepEqual(cpu.readProtectionKeysState(), { pkru: 0x80000001, pkrs: 0xffffffff });
      cpu.setProtectionKeysState({ pkru: -0, pkrs: -0 });
      assert.equal(Object.is(cpu.readProtectionKeysState().pkru, -0), false);
      assert.equal(Object.is(cpu.readProtectionKeysState().pkrs, -0), false);
    } finally {
      cpu.dispose();
    }
  }
});

test('protection-key rejection is atomic and preserves a later accessor exception', async () => {
  const cpu = await createCpu(options());
  try {
    const original = { pkru: 0x80000001, pkrs: 0x55555555 };
    const reads: string[] = [];
    cpu.setProtectionKeysState({
      get pkru() {
        reads.push('pkru');
        return original.pkru;
      },
      get pkrs() {
        reads.push('pkrs');
        return original.pkrs;
      },
    });
    assert.deepEqual(reads, ['pkru', 'pkrs']);
    const before = otherState(cpu);
    for (const input of [{ pkru: 0 }, { pkru: 0, pkrs: 2 ** 32 }, { pkru: -1, pkrs: 0 }]) {
      assert.throws(() => cpu.setProtectionKeysState(input as never), TypeError);
      assert.deepEqual(cpu.readProtectionKeysState(), original);
      assert.deepEqual(otherState(cpu), before);
    }
    const marker = { source: 'pkrs accessor' };
    assert.throws(
      () =>
        cpu.setProtectionKeysState({
          get pkru() {
            cpu.setProtectionKeysState({ pkru: 9, pkrs: 10 });
            return 0;
          },
          get pkrs(): number {
            throw marker;
          },
        }),
      (error) => error === marker,
    );
    assert.deepEqual(cpu.readProtectionKeysState(), { pkru: 9, pkrs: 10 });
  } finally {
    cpu.dispose();
  }
});

test('protection-key capture rejects a completed accessor run without overwriting its effects', async () => {
  const cpu = await createCpu(options());
  try {
    await cpu.run({ budget: 1n });
    cpu.setState({ ...cpu.readState(), rip: 0x1000n });
    cpu.setTimeState({ tsc: 100n, tscAux: 7 });
    cpu.setProtectionKeysState({ pkru: 3, pkrs: 12 });
    let nested: Promise<unknown> | undefined;
    assert.throws(
      () =>
        cpu.setProtectionKeysState({
          get pkru() {
            nested = cpu.run({ budget: 1n });
            return 0;
          },
          pkrs: 0,
        }),
      /run.*capture/,
    );
    await nested;
    assert.deepEqual(cpu.readProtectionKeysState(), { pkru: 3, pkrs: 12 });
    assert.deepEqual(cpu.readTimeState(), { tsc: 101n, tscAux: 7 });
  } finally {
    cpu.dispose();
  }
});

test('protection-key reset during input capture wins and new instances start fresh', async () => {
  const first = await createCpu(options('physical'));
  try {
    first.setProtectionKeysState({ pkru: 0xffffffff, pkrs: 0x80000000 });
    assert.throws(
      () =>
        first.setProtectionKeysState({
          get pkru() {
            first.reset();
            return 9;
          },
          pkrs: 10,
        }),
      /reset.*capture/,
    );
    assert.deepEqual(first.readProtectionKeysState(), { pkru: 0, pkrs: 0 });
    first.setProtectionKeysState({ pkru: 1, pkrs: 2 });
    const second = await createCpu(options('physical'));
    try {
      assert.deepEqual(second.readProtectionKeysState(), { pkru: 0, pkrs: 0 });
    } finally {
      second.dispose();
    }
  } finally {
    first.dispose();
  }
  const disposed = await createCpu(options());
  assert.throws(
    () =>
      disposed.setProtectionKeysState({
        get pkru() {
          disposed.dispose();
          return 9;
        },
        pkrs: 10,
      }),
    /disposed/,
  );
});

test('protection-key imports preserve historical fault, IRQ, NMI, shadow and halt state', async () => {
  const cpu = await createCpu(options('ia32e-paged'));
  try {
    for (const event of [
      {
        kind: 'pending',
        exception: {
          vector: 14,
          instructionPc: 0xff0n,
          returnPc: 0x1000n,
          savedFlags: 0x10202n,
          errorCode: 6,
          pageFaultAddress: 0x501000n,
        },
      },
      { kind: 'halted' },
    ] as const) {
      cpu.setState({
        ...cpu.readState(),
        flags: 0x202n,
        interruptibility: { sti: false, ss: event.kind !== 'halted' },
        event,
        interrupts: { irq: 0x31, nmiPending: true, nmiBlocked: true },
      });
      const before = otherState(cpu, 'ia32e-paged');
      cpu.setProtectionKeysState({ pkru: 0xffffffff, pkrs: 0xaaaaaaaa });
      assert.deepEqual(otherState(cpu, 'ia32e-paged'), before);
    }
  } finally {
    cpu.dispose();
  }
});

test('protection-key banks remain independent across concurrently alive CPUs', async () => {
  for (const model of ['linear', 'ia32e-paged', 'physical'] as const) {
    const first = await createCpu(options(model));
    let second: Cpu | undefined;
    try {
      first.setProtectionKeysState({ pkru: 0xffffffff, pkrs: 0x80000000 });
      second = await createCpu(options(model));
      assert.deepEqual(first.readProtectionKeysState(), {
        pkru: 0xffffffff,
        pkrs: 0x80000000,
      });
      assert.deepEqual(second.readProtectionKeysState(), { pkru: 0, pkrs: 0 });
      second.setProtectionKeysState({ pkru: 0xaaaaaaaa, pkrs: 0x55555555 });
      first.setProtectionKeysState({ pkru: 3, pkrs: 12 });
      assert.deepEqual(first.readProtectionKeysState(), { pkru: 3, pkrs: 12 });
      assert.deepEqual(second.readProtectionKeysState(), {
        pkru: 0xaaaaaaaa,
        pkrs: 0x55555555,
      });
      if (model === 'physical') first.reset();
      first.dispose();
      assert.deepEqual(second.readProtectionKeysState(), {
        pkru: 0xaaaaaaaa,
        pkrs: 0x55555555,
      });
    } finally {
      first.dispose();
      second?.dispose();
    }
  }
});

test('running, failed and disposed CPUs reject key access before touching input', async () => {
  for (const fatal of [false, true]) {
    let cpu: Cpu | undefined;
    const setup = options('linear', Uint8Array.of(0xe4, 0x22));
    const first = await createCpu({
      ...setup,
      io: {
        read() {
          assert.throws(() => cpu!.readProtectionKeysState(), /running/);
          assert.throws(() => cpu!.readXcr0(), /running/);
          assert.throws(() => cpu!.setXcr0(0n), /running/);
          let touched = false;
          assert.throws(
            () =>
              cpu!.setProtectionKeysState({
                get pkru() {
                  touched = true;
                  return 0;
                },
                pkrs: 0,
              }),
            /running/,
          );
          assert.equal(touched, false);
          if (fatal) throw new Error('provider failed');
          return 0x55;
        },
        write() {},
      },
    });
    cpu = first;
    try {
      if (fatal) await assert.rejects(first.run({ budget: 1n }), /engine failed/);
      else assert.equal((await first.run({ budget: 1n })).retired, 1n);
      if (fatal) assert.throws(() => first.readProtectionKeysState(), /failed/);
      if (!fatal) first.dispose();
      assert.throws(() => first.readXcr0(), fatal ? /failed/ : /disposed/);
      assert.throws(() => first.setXcr0(0n), fatal ? /failed/ : /disposed/);
      let touched = false;
      assert.throws(
        () =>
          first.setProtectionKeysState({
            get pkru() {
              touched = true;
              return 0;
            },
            pkrs: 0,
          }),
        fatal ? /failed/ : /disposed/,
      );
      assert.equal(touched, false);
    } finally {
      first.dispose();
    }
  }
});

test('XCR0 checkpoints are per CPU and reject invalid imports without changing managed state', async () => {
  const cpu = await createCpu(options('physical'));
  const peer = await createCpu(options());
  try {
    assert.equal(cpu.readXcr0(), 1n);
    const original = otherState(cpu, 'physical');
    for (const value of [3n, 0x201n, 0x203n, 1n]) {
      cpu.setXcr0(value);
      assert.equal(cpu.readXcr0(), value);
      assert.equal(peer.readXcr0(), 1n);
      assert.deepEqual(otherState(cpu, 'physical'), original);
    }
    cpu.setXcr0(0x203n);
    for (const bad of [0n, 2n, 5n, -1n, 1n << 64n, 1, Object(1n)]) {
      assert.throws(() => cpu.setXcr0(bad as bigint), TypeError);
      assert.equal(cpu.readXcr0(), 0x203n);
    }
    cpu.setState(cpu.readState());
    cpu.setPagingState(cpu.readPagingState());
    assert.equal(cpu.readXcr0(), 0x203n);
    cpu.reset();
    assert.equal(cpu.readXcr0(), 1n);
  } finally {
    cpu.dispose();
    peer.dispose();
  }
  assert.throws(() => cpu.readXcr0(), /disposed/);
  assert.throws(() => cpu.setXcr0(1n), /disposed/);
});
