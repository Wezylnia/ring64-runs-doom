import { SIGNAL, SLOT, STARTUP_WORD, SharedControl, cpuOffset, nextIdentity } from './control.js';
import type { IoWidth } from '../io.js';
import type {
  Binding,
  Command,
  CreateEngine,
  FromWorker,
  ParallelCore,
  ParallelEngine,
  Range,
  Start,
  ToWorker,
} from './protocol.js';
import { bindRam } from './ram.js';
import { connectWorker } from './transport.js';

class WorkerRuntime implements Binding {
  readonly privateMemory = new WebAssembly.Memory({ initial: 64 });
  readonly shared: Start['shared'];
  readonly layout: Start['layout'];
  readonly backings: Start['backings'];
  readonly ramImports: ReturnType<typeof bindRam>;
  readonly signal: WebAssembly.Global;
  readonly #control: SharedControl;
  readonly #id: number;
  #waiters: Array<() => void> = [];
  #exclusive = 0n;
  #admin = 0n;
  #scopes = new Set<number>();
  #scopeNext = 0;
  #requested = 0n;
  #command = 0n;
  #running = false;
  #irq = 0n;
  #nmi = 0n;
  #ioTicket = 0n;
  #pending: Range[] = [];
  #invalidation = 0n;
  #compileWait: (() => void) | undefined;
  #compilation: Promise<WebAssembly.Module> | undefined;

  constructor(
    data: Start,
    private readonly send: (event: FromWorker) => void,
  ) {
    this.shared = data.shared;
    this.layout = data.layout;
    this.backings = data.backings;
    this.#id = data.id;
    this.#control = new SharedControl(data.shared);
    this.signal = new WebAssembly.Global({ value: 'i32' }, cpuOffset(data.id) + SLOT.signal);
    this.ramImports = bindRam(data.ramModule, this.privateMemory, data.shared);
  }

  startup(): void {
    let previous = this.ramImports.load_lane(STARTUP_WORD);
    for (;;) {
      const found = this.ramImports.compare_exchange_lane(STARTUP_WORD, previous, previous + 1n);
      if (found === previous) return;
      previous = found;
    }
  }

  acknowledge(): void {
    const phase = this.#control.phase;
    if (!(phase & 1n) || this.#control.load(this.#id, SLOT.active) !== 0n) return;
    if (this.#control.load(this.#id, SLOT.acknowledgement) === phase) return;
    this.#control.store(this.#id, SLOT.acknowledgement, phase);
    this.send({ kind: 'ack', phase });
  }

  phaseChanged(): void {
    this.acknowledge();
    if (this.#pauseRequested()) this.#compileWait?.();
    const waiters = this.#waiters;
    this.#waiters = [];
    for (const awake of waiters) awake();
  }

  invalidate(sequence: bigint, ranges: Range[]): void {
    if (sequence !== nextIdentity(this.#invalidation))
      throw new Error('invalid source invalidation sequence');
    for (const range of ranges) this.#pending.push(range);
    this.#invalidation = sequence;
    this.phaseChanged();
  }

  #applyInvalidation(core: ParallelCore): void {
    for (const range of this.#pending) core.parallelInvalidate(range.offset, range.length);
    this.#pending.length = 0;
  }

  #pauseRequested(): boolean {
    return this.#running && this.#control.load(this.#id, SLOT.pause) === this.#command;
  }

  async activate(core: ParallelCore): Promise<void> {
    for (;;) {
      const received = this.#control.load(this.#id, SLOT.invalidations) === this.#invalidation;
      if (
        received &&
        (this.#admin !== 0n ||
          this.#exclusive !== 0n ||
          this.#control.load(this.#id, SLOT.active) === this.#control.phase ||
          this.#control.acquire(this.#id, this.#invalidation))
      ) {
        if (this.#control.load(this.#id, SLOT.invalidations) === this.#invalidation) {
          this.#applyInvalidation(core);
          return;
        }
        this.#control.leave(this.#id);
      }
      this.acknowledge();
      await new Promise<void>((resolve) => this.#waiters.push(resolve));
    }
  }

  async checkpoint(core: ParallelCore): Promise<boolean> {
    this.reconcile(core);
    this.suspend();
    if (this.#pauseRequested()) return true;
    this.#control.storeWord(this.#id, SLOT.ready, 1);
    await this.activate(core);
    this.#control.clearSignal(this.#id, SIGNAL.phase | SIGNAL.ingress | SIGNAL.pause);
    const irq = this.#control.load(this.#id, SLOT.irq);
    const nmi = this.#control.load(this.#id, SLOT.nmi);
    this.#irq = irq;
    this.#nmi = nmi;
    core.parallelSetIngress(irq === 0n ? null : Number((irq & 0x1ffn) - 1n), nmi !== 0n);
    if (this.#control.phase & 1n) this.#control.signal(this.#id, SIGNAL.phase);
    return this.#pauseRequested();
  }

  async compile(bytes: Uint8Array<ArrayBuffer>): Promise<WebAssembly.Module | undefined> {
    this.#notReady();
    this.suspend();
    let cancel!: () => void;
    const paused = new Promise<undefined>((resolve) => {
      cancel = () => resolve(undefined);
    });
    this.#compileWait = cancel;
    try {
      if (this.#pauseRequested()) return undefined;
      if (this.#compilation) await Promise.race([this.#compilation.catch(() => {}), paused]);
      if (this.#pauseRequested()) return undefined;
      const job = WebAssembly.compile(bytes);
      this.#compilation = job;
      const settled = () => {
        if (this.#compilation === job) this.#compilation = undefined;
      };
      void job.then(settled, settled);
      return await Promise.race([job, paused]);
    } finally {
      this.#compileWait = undefined;
    }
  }

  sliceLimit(budget: bigint): bigint {
    const round = this.#control.load(this.#id, SLOT.round);
    return round === this.#control.phase &&
      this.#control.load(this.#id, SLOT.roundAck) !== round &&
      budget > 64n
      ? 64n
      : budget;
  }

  finishSlice(): void {
    const round = this.#control.load(this.#id, SLOT.round);
    if (
      round === 0n ||
      round !== this.#control.phase ||
      this.#control.load(this.#id, SLOT.roundAck) === round
    )
      return;
    this.#control.store(this.#id, SLOT.roundAck, round);
    this.send({ kind: 'round' });
  }

  #notReady(): void {
    this.#control.storeWord(this.#id, SLOT.ready, 0);
    if (this.#control.load(this.#id, SLOT.round) !== 0n) this.send({ kind: 'round' });
  }

  reconcile(core: ParallelCore): void {
    if (!this.#running) return;
    const local = core.parallelGetIngress();
    if (this.#irq && local.irq === null) {
      this.#control.compareExchange(this.#id, SLOT.irq, this.#irq, 0n);
      this.#irq = 0n;
    }
    if (this.#nmi && !local.nmiPending) {
      this.#control.compareExchange(this.#id, SLOT.nmi, this.#nmi, 0n);
      this.#nmi = 0n;
    }
  }

  projection(core: ParallelCore): void {
    const irq = this.#control.load(this.#id, SLOT.irq);
    const nmi = this.#control.load(this.#id, SLOT.nmi);
    core.parallelSetIngress(irq ? Number((irq & 0x1ffn) - 1n) : null, nmi !== 0n);
  }

  suspend(): void {
    if (this.#scopes.size !== 0) throw new Error('parallel boundary in a live exclusive scope');
    if (this.#exclusive !== 0n) {
      const request = this.#exclusive;
      this.#exclusive = 0n;
      this.send({ kind: 'released', request });
    }
    this.#control.leave(this.#id);
    this.acknowledge();
  }

  #addScope(): number {
    if (this.#scopeNext === 0xffffffff) throw new Error('parallel scope identity exhausted');
    const token = ++this.#scopeNext;
    this.#scopes.add(token);
    return token;
  }

  readonly imports = {
    enter_exclusive: (): number => {
      if (this.#scopes.size !== 0 || this.#admin !== 0n || this.#exclusive !== 0n)
        return this.#addScope();
      this.#notReady();
      this.#control.leave(this.#id);
      this.#requested = nextIdentity(this.#requested);
      this.send({ kind: 'exclusive', request: this.#requested });
      for (;;) {
        const seen = this.#control.loadWord(this.#id, SLOT.wake);
        if (this.#control.load(this.#id, SLOT.grant) === this.#requested) break;
        this.acknowledge();
        this.#control.wait(this.#id, seen);
      }
      this.#exclusive = this.#requested;
      return this.#addScope();
    },
    leave_exclusive: (token: number): void => {
      if (!this.#scopes.delete(token >>> 0)) throw new Error('invalid parallel exclusive token');
      // The grant stays live until the reached compound effect leaves native execution.
    },
    note_write: (offset: number, length: number): void => {
      if (this.#scopes.size === 0 && this.#exclusive === 0n && this.#admin === 0n)
        throw new Error('watched RAM write outside parallel exclusivity');
      this.send({ kind: 'write', range: { offset: offset >>> 0, length: length >>> 0 } });
    },
  };

  watch(core: ParallelCore, owner: bigint): void {
    const ranges = core.parallelProof(owner);
    const words = new Int32Array(this.shared.buffer);
    for (const { offset, length } of ranges) {
      const begin = offset - this.layout.ramBase;
      if (begin < 0 || length <= 0 || begin + length > this.layout.ramCapacity)
        throw new Error('invalid parallel source range');
      for (let i = begin; i < begin + length; i++) {
        Atomics.or(words, this.layout.watchBase / 4 + Math.floor(i / 32), 1 << (i % 32));
      }
    }
    this.send({ kind: 'watch', owner, ranges });
  }

  unwatch(owner: bigint): void {
    this.send({ kind: 'unwatch', owner });
  }

  port(port: number, width: IoWidth, value?: number): number {
    this.#ioTicket = nextIdentity(this.#ioTicket);
    this.#control.storeWord(this.#id, SLOT.ioStatus, 0);
    this.send({
      kind: 'io',
      sequence: this.#command,
      ticket: this.#ioTicket,
      port,
      width,
      ...(value === undefined ? {} : { value }),
    });
    for (;;) {
      const observed = this.#control.loadWord(this.#id, SLOT.wake);
      const status = this.#control.loadWord(this.#id, SLOT.ioStatus);
      if (status === 1) return this.#control.loadWord(this.#id, SLOT.ioResult) >>> 0;
      if (status !== 0) throw new Error('machine port provider failed');
      this.#control.wait(this.#id, observed);
    }
  }

  async invoke(cpu: ParallelEngine, command: Command): Promise<unknown> {
    if (command.permit !== undefined) {
      if (
        command.permit !== this.#control.phase ||
        !(command.permit & 1n) ||
        this.#control.load(this.#id, SLOT.active) !== 0n ||
        this.#admin !== 0n
      )
        throw new Error('unauthorized parallel administration');
      this.#admin = command.permit;
      try {
        this.#applyInvalidation(cpu.parallelOwner);
        if (command.method === 'reset') {
          if (this.#command !== 0n) throw new Error('reset overlapped an active CPU command');
          return cpu.reset();
        }
        return cpu.parallelAdmin(command.method, command.args);
      } finally {
        this.#admin = 0n;
      }
    }
    if (this.#command !== 0n) throw new Error('overlapping commands at parallel worker');
    this.#command = command.sequence;
    this.#running = command.method === 'run';
    if (this.#running && this.#control.load(this.#id, SLOT.run) !== command.sequence)
      throw new Error('unreserved parallel run');
    this.#admin = command.permit ?? 0n;
    const core = cpu.parallelOwner;
    try {
      await this.activate(core);
      if (this.#running || command.method === 'readState') this.projection(core);
      if (command.method === 'hostMemoryChanged') throw new Error('missing administrative permit');

      const method = cpu[command.method] as (...args: unknown[]) => unknown;
      return await Reflect.apply(method, cpu, command.args);
    } finally {
      this.reconcile(core);
      this.#notReady();
      this.#running = false;
      this.#command = 0n;
      this.#admin = 0n;
      this.suspend();
    }
  }
}

let runtime: WorkerRuntime | undefined;
let cpu: ParallelEngine | undefined;

async function receive(data: ToWorker): Promise<void> {
  if (data.kind === 'phase') {
    runtime?.phaseChanged();
    return;
  }
  if (data.kind === 'invalidate') {
    runtime?.invalidate(data.sequence, data.ranges);
    return;
  }
  if (data.kind === 'start') {
    if (runtime) throw new Error('parallel worker initialized twice');
    const current = new WorkerRuntime(data, send);
    runtime = current;
    const adapter = './engine.generated.js';
    const { createCpu } = (await import(adapter)) as { createCpu: CreateEngine };
    cpu = await createCpu(
      {
        compiler: data.compiler,
        memory: {
          memoryModel: data.model,
          entry: data.state.entry,
          backings: [],
          ...data.memory,
          ...(data.state.paging ? { paging: data.state.paging } : {}),
          ...(data.state.fastSystem ? { fastSystem: data.state.fastSystem } : {}),
        },
        ...(data.state.context ? { context: data.state.context } : {}),
        limits: data.limits,
        ...(data.hasIo
          ? {
              io: {
                read: (port, width) => current.port(port, width),
                write: (port, width, value) => {
                  current.port(port, width, value);
                },
              },
            }
          : {}),
      },
      current,
    );
    current.startup();
    send({ kind: 'ready' });
    return;
  }
  if (!runtime || !cpu) throw new Error('parallel command preceded initialization');
  try {
    const result = await runtime.invoke(cpu, data);
    if (data.method === 'setState' || data.method === 'reset') {
      const interrupts = cpu.parallelOwner.parallelGetIngress();
      send({ kind: 'result', sequence: data.sequence, result: interrupts });
      return;
    }
    send({ kind: 'result', sequence: data.sequence, result });
  } catch (error) {
    if (error instanceof TypeError || error instanceof RangeError) {
      send({
        kind: 'rejected',
        sequence: data.sequence,
        error: error.name as 'TypeError' | 'RangeError',
        message: error.message,
      });
      return;
    }
    throw error;
  }
}

const send = await connectWorker((message) => {
  void receive(message as ToWorker).catch((error: unknown) =>
    send({
      kind: 'fatal',
      message: error instanceof Error ? error.message : String(error),
    }),
  );
});
