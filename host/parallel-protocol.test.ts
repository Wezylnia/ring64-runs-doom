import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import type { createMachine as create, Machine, MachineOptions } from './machine.js';
import { SharedControl, SLOT } from './parallel/control.js';
import { bindRam, ramModule } from './parallel/ram.js';

test('a phase close between publishing and rechecking a lease prevents entry', () => {
  const memory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  const coordinator = new SharedControl(memory);
  coordinator.initialize();
  class Interrupted extends SharedControl {
    reads = 0;
    override get phase(): bigint {
      if (++this.reads === 2) {
        assert.equal(coordinator.load(0, SLOT.active), 2n);
        coordinator.transition(true);
      }
      return super.phase;
    }
  }
  const worker = new Interrupted(memory);
  assert.equal(worker.acquire(0, 0n), false);
  assert.equal(coordinator.load(0, SLOT.active), 0n);
});

test('a reopened phase cannot admit a worker before its exact invalidation arrives', () => {
  const memory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  const coordinator = new SharedControl(memory);
  coordinator.initialize();
  const worker = new SharedControl(memory);
  assert.equal(worker.load(0, SLOT.invalidations), 0n);
  coordinator.transition(true);
  coordinator.store(0, SLOT.invalidations, 1n);
  coordinator.transition(false);
  assert.equal(worker.acquire(0, 0n), false);
  assert.equal(worker.load(0, SLOT.active), 0n);
  assert.equal(worker.acquire(0, 1n), true);
  assert.equal(worker.load(0, SLOT.active), 4n);
});

test('shared helper functions access the supplied shared memory at both prescribed widths', () => {
  const privateMemory = new WebAssembly.Memory({ initial: 1 });
  const sharedMemory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  const helpers = bindRam(ramModule(), privateMemory, sharedMemory);
  helpers.store_lane(8, 0x1122334400000000n);
  assert.equal(
    helpers.compare_exchange_lane(8, 0x1122334400000000n, 0x5522334400000000n),
    0x1122334400000000n,
  );
  assert.equal(helpers.load_lane(8), 0x5522334400000000n);
  helpers.store_control(0, 0x7733);
  assert.equal(helpers.compare_exchange_control(0, 0x7733, 1), 0x7733);
  assert.equal(helpers.load_control(0), 1);
  assert.deepEqual([...new Uint8Array(privateMemory.buffer, 0, 16)], Array<number>(16).fill(0));
});

async function machine(code: number[], io?: MachineOptions['io']): Promise<Machine> {
  const entry = new URL('../build/host/machine.js', import.meta.url);
  const { createMachine }: { createMachine: typeof create } = await import(entry.href);
  const compiler = await readFile(new URL('../build/ring64-parallel.wasm', import.meta.url));
  return createMachine({
    compiler,
    memoryModel: 'linear',
    sharedRamCapacity: 0x2000,
    limits: {
      maxInstructions: 24,
      maxArtifactBytes: 65536,
      maxSourceBytes: 256,
      maxCachedArtifacts: 8,
      maxCachedBytes: 262144,
      maxTranslationsPerRun: 18,
    },
    cpus: [{ entry: 0x1000n }, { entry: 0x1000n }],
    memoryLimits: { maxBackingBytes: 8192, maxBackings: 2, maxMappings: 2 },
    memory: {
      backings: [
        { id: 'code', bytes: Uint8Array.from(code) },
        { id: 'ram', bytes: new Uint8Array(32) },
      ],
      mappings: [
        {
          base: 0x1000n,
          backing: 'code',
          offset: 0,
          length: code.length,
          permissions: { read: true, write: false, execute: true },
        },
        {
          base: 0x3000n,
          backing: 'ram',
          offset: 0,
          length: 32,
          permissions: { read: true, write: true, execute: false },
        },
      ],
    },
    ...(io ? { io } : {}),
  });
}

function registers(rdi: bigint): bigint[] {
  return Array.from({ length: 16 }, (_, index) => (index === 7 ? rdi : 0n));
}

test(
  'running IRQ intake frees only a staged slot and NMI wakes a sibling with IF clear',
  { timeout: 30000 },
  async () => {
    let bothReached!: () => void;
    const ready = new Promise<void>((resolve) => {
      bothReached = resolve;
    });
    const reached = new Set<number>();
    // Both workers must execute OUT before the coordinator injects into the running streams.
    const board = await machine([0xe6, 0x80, 0xeb, 0xfe], {
      read() {
        assert.fail('unexpected readiness input');
      },
      write(port, width, value) {
        assert.equal(port, 0x80);
        assert.equal(width, 8);
        assert.ok(value === 1 || value === 2);
        assert.equal(reached.has(value), false);
        reached.add(value);
        if (reached.size === 2) bothReached();
      },
    });
    try {
      const [first, second] = board.cpus;
      assert.ok(first && second);
      const firstRegisters = registers(0n);
      firstRegisters[0] = 1n;
      const secondRegisters = registers(0n);
      secondRegisters[0] = 2n;
      await first.setState({ rip: 0x1000n, flags: 0x202n, registers: firstRegisters });
      await second.setState({ rip: 0x1000n, flags: 2n, registers: secondRegisters });
      const options = { budget: 100000000n, eventDelivery: { budget: 0n } };
      const firstRun = first.run(options);
      const secondRun = second.run(options);
      await Promise.race([
        ready,
        firstRun.then(() => {
          throw new Error('first CPU exited before readiness');
        }),
        secondRun.then(() => {
          throw new Error('second CPU exited before readiness');
        }),
      ]);
      assert.equal(await second.requestInterrupt(0x43), true);
      assert.equal(await second.requestInterrupt(0x44), false);
      assert.equal(await first.requestInterrupt(0x40), true);
      const irq = await firstRun;
      assert.equal(irq.reason, 'event-budget');
      assert.deepEqual(irq.state.interrupts, { irq: null, nmiPending: false, nmiBlocked: false });
      const firstEvent = irq.state.event;
      assert.equal(firstEvent.kind, 'pending-interrupt');
      assert.deepEqual(firstEvent.interrupt.source, { kind: 'irq', vector: 0x40 });
      assert.equal(
        await first.requestInterrupt(0x41),
        true,
        'staged frame does not occupy the queued slot',
      );
      assert.equal((await first.readState()).interrupts.irq, 0x41);
      await second.requestNmi();
      const nmi = await secondRun;
      assert.equal(nmi.reason, 'event-budget');
      assert.deepEqual(nmi.state.interrupts, { irq: 0x43, nmiPending: false, nmiBlocked: true });
      const secondEvent = nmi.state.event;
      assert.equal(secondEvent.kind, 'pending-interrupt');
      assert.deepEqual(secondEvent.interrupt.source, { kind: 'nmi' });
    } finally {
      await board.dispose();
    }
  },
);

test(
  'a synchronous port provider rejects matching machine reentry before any command queues',
  { timeout: 30000 },
  async () => {
    let calls = 0;
    const board = await machine([0xe4, 0x80], {
      read(port, width) {
        assert.equal(port, 0x80);
        assert.equal(width, 8);
        calls++;
        assert.throws(() => board.pause(), { code: 'busy' });
        assert.throws(() => board.readMemory(0x1000n, 1), { code: 'busy' });
        assert.throws(() => board.cpus[1]!.requestInterrupt(0x20), { code: 'busy' });
        return 0x61;
      },
      write() {
        assert.fail('unexpected output');
      },
    });
    try {
      const result = await board.cpus[0]!.run({ budget: 1n });
      assert.equal(result.retired, 1n);
      assert.equal(result.state.registers[0], 0x61n);
      assert.equal(calls, 1);
      assert.equal((await board.cpus[1]!.readState()).interrupts.irq, null);
    } finally {
      await board.dispose();
    }
  },
);

test(
  'provider failure terminates peer runs and does not retry the external effect',
  { timeout: 30000 },
  async () => {
    let calls = 0;
    const board = await machine([0xe4, 0x80, 0xeb, 0xfe], {
      read() {
        calls++;
        throw new Error('failed after consuming input');
      },
      write() {
        assert.fail('unexpected output');
      },
    });
    try {
      const [first, second] = board.cpus;
      assert.ok(first && second);
      await second.setState({ rip: 0x1002n, flags: 2n, registers: registers(0n) });
      const results = await Promise.allSettled([
        second.run({ budget: 100000000n }),
        first.run({ budget: 1n }),
      ]);
      for (const result of results) {
        assert.equal(result.status, 'rejected');
        assert.equal(result.reason.code, 'failed');
      }
      assert.equal(calls, 1);
      assert.throws(() => first.run({ budget: 1n }), { code: 'failed' });
      assert.throws(() => second.readState(), { code: 'failed' });
    } finally {
      await board.dispose();
    }
  },
);
