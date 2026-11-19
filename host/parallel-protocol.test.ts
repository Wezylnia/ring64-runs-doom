import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { Worker } from 'node:worker_threads';
import type { createMachine as create, Machine, MachineOptions } from './machine.js';
import { SharedControl, SLOT } from './parallel/control.js';
import type { FromWorker, ToWorker } from './parallel/protocol.js';
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
  'the public machine keeps shared RAM and worker initialization private',
  { timeout: 30000 },
  async () => {
    const board = await machine([0x90]);
    try {
      assert.equal(Reflect.has(board, 'memory'), false, 'the public machine exposes shared RAM');
      assert.equal(
        Reflect.has(board, 'input'),
        false,
        'the public machine exposes captured inputs',
      );
      assert.equal(
        Reflect.has(board, 'storeLane'),
        false,
        'the public machine exposes raw lane writes',
      );
      assert.equal(
        Reflect.has(board, 'initialize'),
        false,
        'the public machine exposes worker startup',
      );
      assert.equal(
        Reflect.has(board.constructor, 'start'),
        false,
        'the constructor exposes startup',
      );
      const cpus = board.cpus;
      assert.equal(Object.isFrozen(cpus), true, 'the CPU list is immutable');
      assert.equal(Reflect.set(board, 'cpus', []), false, 'the CPU list cannot be replaced');
      assert.equal(board.cpus, cpus, 'the same handles remain available');
      const copied = await board.readMemory(0x3000n, 1);
      copied[0] = 0x42;
      assert.deepEqual(await board.readMemory(0x3000n, 1), Uint8Array.of(0));
    } finally {
      await board.dispose();
    }
  },
);

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

type PortRequest = Extract<FromWorker, { kind: 'io' }>;

test(
  'a single run publishes two reached I/O effects in ticket order',
  { timeout: 30000 },
  async () => {
    const ports: number[] = [];
    const board = await machine([0xe4, 0x80, 0xe4, 0x81], {
      read(port) {
        ports.push(port);
        return port;
      },
      write() {
        assert.fail('unexpected output');
      },
    });
    try {
      const [result] = await Promise.allSettled([board.cpus[0]!.run({ budget: 2n })]);
      assert.deepEqual(ports, [0x80, 0x81], 'the reached I/O sequence lost an effect');
      assert.equal(result.status, 'fulfilled');
      assert.equal(result.value.retired, 2n);
      assert.equal(result.value.state.registers[0], 0x81n);
    } finally {
      await board.dispose();
    }
  },
);

function assertRejectedPort(result: PromiseSettledResult<unknown>): void {
  assert.equal(result.status, 'rejected');
  const error: unknown = result.reason;
  assert.ok(error instanceof Error && 'code' in error);
  assert.equal(error.code, 'failed');
  assert.ok(error.cause instanceof Error);
  assert.equal(error.cause.message, 'invalid parallel port request');
}

for (const [field, description] of [
  ['ticket', 'a duplicate reached I/O ticket'],
  ['sequence', 'a reached I/O request from a stale run'],
] as const) {
  test(`${description} fails before the provider runs`, { timeout: 30000 }, async (t) => {
    const emit = Worker.prototype.emit;
    const requests: PortRequest[] = [];
    t.mock.method(
      Worker.prototype,
      'emit',
      function (this: Worker, event: string, ...args: unknown[]) {
        const message = args[0] as FromWorker | undefined;
        if (event === 'message' && message?.kind === 'io') {
          requests.push(message);
          if (requests.length === 2) args[0] = { ...message, [field]: requests[0]![field] };
        }
        return emit.call(this, event, ...args);
      },
    );

    const ports: number[] = [];
    const board = await machine([0xe4, 0x80, 0xe4, 0x81], {
      read(port) {
        ports.push(port);
        return port;
      },
      write() {
        assert.fail('unexpected output');
      },
    });
    try {
      const cpu = board.cpus[0]!;
      const first = await cpu.run({ budget: 1n });
      assert.equal(first.retired, 1n);
      assert.equal(first.state.registers[0], 0x80n);
      const [result] = await Promise.allSettled([cpu.run({ budget: 1n })]);
      assert.deepEqual(ports, [0x80], 'rejected request reached the provider');
      assertRejectedPort(result);
      assert.equal(requests.length, 2);
      assert.equal(requests[1]!.ticket, requests[0]!.ticket + 1n);
      assert.notEqual(requests[1]!.sequence, requests[0]!.sequence);
    } finally {
      await board.dispose();
    }
  });
}

test(
  'a port request tagged to idle CPU inspection fails before the provider runs',
  { timeout: 30000 },
  async (t) => {
    const post = Worker.prototype.postMessage;
    t.mock.method(
      Worker.prototype,
      'postMessage',
      function (this: Worker, ...args: Parameters<Worker['postMessage']>) {
        const message = args[0] as ToWorker;
        if (message.kind === 'command' && message.method === 'readState') {
          const request: PortRequest = {
            kind: 'io',
            sequence: message.sequence,
            ticket: 1n,
            port: 0x80,
            width: 8,
          };
          this.emit('message', request);
        }
        return post.apply(this, args);
      },
    );

    const ports: number[] = [];
    const board = await machine([0x90], {
      read(port) {
        ports.push(port);
        return 0;
      },
      write() {
        assert.fail('unexpected output');
      },
    });
    try {
      const [result] = await Promise.allSettled([board.cpus[0]!.readState()]);
      assert.deepEqual(ports, [], 'idle inspection reached the provider');
      assertRejectedPort(result);
    } finally {
      await board.dispose();
    }
  },
);

test(
  'a reached INS retains its exclusive grant across input and a late destination fault',
  { timeout: 30000 },
  async (t) => {
    const post = Worker.prototype.postMessage;
    let control: SharedControl | undefined;
    t.mock.method(
      Worker.prototype,
      'postMessage',
      function (this: Worker, ...args: Parameters<Worker['postMessage']>) {
        const message = args[0] as ToWorker;
        if (message.kind === 'start' && message.id === 0)
          control = new SharedControl(message.shared);
        return post.apply(this, args);
      },
    );

    const reached: Array<{ phase: bigint; grant: bigint }> = [];
    const board = await machine([0x6c], {
      read(port, width) {
        assert.equal(port, 0x80);
        assert.equal(width, 8);
        assert.ok(control);
        reached.push({ phase: control.phase, grant: control.load(0, SLOT.grant) });
        return 0xa0 + reached.length;
      },
      write() {
        assert.fail('unexpected output');
      },
    });
    try {
      const cpu = board.cpus[0]!;
      const destination = 0x3020n;
      const input = registers(destination);
      input[2] = 0x80n;
      await cpu.setState({ rip: 0x1000n, flags: 2n, registers: input });
      const fault = await cpu.run({ budget: 1n });
      assert.equal(fault.reason, 'data-fault');
      assert.equal(fault.fault?.failingAddress, destination);
      assert.equal(fault.retired, 0n);
      assert.equal(fault.state.rip, 0x1000n);
      assert.equal(fault.state.registers[7], destination);
      assert.equal(reached.length, 1, 'the late fault consumed the first input');
      assert.equal(reached[0]!.phase & 1n, 1n, 'reached INS input lacks a closed phase');
      assert.notEqual(reached[0]!.grant, 0n, 'reached INS input lacks an exclusive grant');

      await board.setMappings([
        {
          base: 0x1000n,
          backing: 'code',
          offset: 0,
          length: 1,
          permissions: { read: true, write: false, execute: true },
        },
        {
          base: destination,
          backing: 'ram',
          offset: 0,
          length: 32,
          permissions: { read: true, write: true, execute: false },
        },
      ]);
      const retried = await cpu.run({ budget: 1n });
      assert.equal(retried.retired, 1n);
      assert.equal(retried.state.rip, 0x1001n);
      assert.equal(retried.state.registers[7], destination + 1n);
      assert.equal(reached.length, 2, 'the architectural retry consumed one fresh input');
      assert.equal(reached[1]!.phase & 1n, 1n);
      assert.notEqual(reached[1]!.grant, reached[0]!.grant);
      assert.equal((await board.readMemory(destination, 1))[0], 0xa2);
    } finally {
      await board.dispose();
    }
  },
);
