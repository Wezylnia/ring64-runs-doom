import { context } from '../processor/modes/fixture.js';
import { changed, exit, fault, type Scenario } from '../../support/fixture.js';
import {
  ENTRY,
  RAM,
  asBytes,
  cpuState,
  done,
  equal,
  hex,
  linearMachine,
  same,
  span,
  withMachine,
  words,
  type MachineFactory,
} from './fixture.js';
import { parallelGuardedLoopScenarios } from './guarded-loops.js';

export function parallelMemoryScenarios(
  factory: MachineFactory,
  compiler: WebAssembly.Module,
): Scenario[] {
  return [
    {
      name: 'parallel x86 workers rendezvous in guest memory with private TLS, stack and I/O replies',
      async run() {
        // Entire relocation-free worker symbols compiled from rendezvous.c with Apple clang 21
        // (-O2, -ffreestanding, no PIE), SysV i386 and x86-64. The compiled stack slot is at SP-4.
        const bodies = {
          protected32:
            '508b4c240c8b442408f0ff000f1f40008b1083fa0272f931c0648b008904248b04248901e68059c3',
          long64:
            'f0ff07666666662e0f1f8400000000008b0783f80272f931c064678b00894424fc8b4424fc8906e680c3',
        };
        for (const long of [false, true]) {
          const options = linearMachine(compiler, long ? bodies.long64 : bodies.protected32);
          const trace: number[] = [];
          options.io = {
            read() {
              throw new Error('unexpected input');
            },
            write(port, width, value) {
              equal(port, 0x80, 'guest port');
              equal(width, 8, 'output width');
              trace.push(value);
            },
          };
          options.cpus.forEach((cpu, i) => {
            const tls = RAM + 0x100n + BigInt(i) * 64n;
            if (long) cpu.context = { mode: 'long64', fsBase: tls, gsBase: 0n };
            else {
              const protected32 = context('protected');
              protected32.segments.fs.base = tls;
              cpu.context = protected32;
            }
          });
          await withMachine(factory, options, async (machine) => {
            equal(machine.cpus.length, 2, 'two independent handles');
            const stop = 0x2000n;
            for (const [i, cpu] of machine.cpus.entries()) {
              equal(cpu.id, i, 'stable id');
              const offset = BigInt(i) * 64n;
              const out = RAM + 0x200n + offset;
              const sp = RAM + 0x600n + offset;
              await machine.writeMemory(RAM + 0x100n + offset, asBytes(BigInt(0x31 + i)));
              const frame = new Uint8Array(long ? 8 : 12);
              const view = new DataView(frame.buffer);
              view.setUint32(0, Number(stop), true);
              if (!long) {
                view.setUint32(4, Number(RAM), true);
                view.setUint32(8, Number(out), true);
              }
              await machine.writeMemory(sp, frame);
              await cpu.setState(cpuState(ENTRY, { 4: sp, 6: out, 7: RAM }));
            }
            const results = await Promise.all(
              machine.cpus.map((cpu) => cpu.run({ budget: 2_000_000n, stop })),
            );
            for (const [i, result] of results.entries()) {
              const sp = RAM + 0x600n + BigInt(i) * 64n;
              done(result, stop);
              equal(
                result.state.registers[0],
                BigInt(0x31 + i) | (long ? 0n : 0x1122334400000000n),
                'thread-local return value and legacy high-half retention',
              );
              equal(
                result.state.registers[4],
                sp + (long ? 8n : 4n),
                'original ABI consumed the return slot',
              );
              same(
                [...(await machine.readMemory(sp - 4n, 4))],
                [0x31 + i, 0, 0, 0],
                'private compiled stack spill',
              );
              same(
                await words(machine, RAM + 0x200n + BigInt(i) * 64n, 1),
                [BigInt(0x31 + i)],
                'thread output',
              );
            }
            same(await words(machine, RAM, 1), [2n], 'actual guest rendezvous');
            same(trace.sort(), [0x31, 0x32], 'provider invoked once per guest effect');
          });
        }
      },
    },
    {
      name: 'separate guest read and write lose exactly one update while LOCK keeps both',
      async run() {
        // mov rax,[rbx]; inc rax; mov [rbx],rax; lock inc qword [rbx].
        const options = linearMachine(compiler, '488b0348ffc0488903f048ff03');
        const alias = 0x8000n;
        options.memory.mappings.push(span(alias, 'data', 0, 8));
        await withMachine(factory, options, async (machine) => {
          await machine.writeMemory(RAM, asBytes(7n));
          const pointers = [RAM, alias];
          await Promise.all(
            machine.cpus.map((cpu, i) => cpu.setState(cpuState(ENTRY, { 3: pointers[i]! }))),
          );
          const loaded = await Promise.all(machine.cpus.map((cpu) => cpu.run({ budget: 1n })));
          for (const read of loaded) {
            equal(read.reason, 'budget', 'read-only boundary');
            equal(read.retired, 1n, 'one old-value read');
            equal(read.state.rip, ENTRY + 3n, 'both reads precede either write');
            equal(read.state.registers[0], 7n, 'same original value from both aliases');
          }
          const writes = await Promise.all(
            machine.cpus.map((cpu) => cpu.run({ budget: 3n, stop: ENTRY + 9n })),
          );
          for (const write of writes) done(write, ENTRY + 9n, 2n);
          same(await words(machine, RAM, 1), [8n], 'exactly one ordinary update is lost');

          await machine.writeMemory(RAM, asBytes(7n));
          const locked = await Promise.all(machine.cpus.map((cpu) => cpu.run({ budget: 1n })));
          for (const update of locked) {
            equal(update.reason, 'budget', 'locked completion boundary');
            equal(update.retired, 1n, 'each actor retires one lock');
            equal(update.state.rip, ENTRY + 13n, 'exact locked successor');
          }
          same(await words(machine, alias, 1), [9n], 'both locked updates commit to the same lane');
        });
      },
    },
    {
      name: 'shared locked ADD loop rechecks a changed effective address',
      async run() {
        // LOCK ADD qword [RBX],1; LEA RBX,[RBX+8]; DEC ECX; JNZ entry.
        const code = hex('f048830301488d5b08ffc975f3');
        const stop = ENTRY + BigInt(code.length);
        const options = linearMachine(compiler, code);
        options.memory.backings[1]!.bytes.set(asBytes(0x123n, 0x20n, 0xffff_ffff_ffff_ffffn));
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          const before = cpuState(ENTRY, { 1: 3n, 3: RAM });
          await cpu.setState(before);
          exit(
            await cpu.run({ budget: 20n, stop }),
            'stop',
            20n,
            12n,
            changed(before, stop, { 1: 0n, 3: RAM + 24n }, 0x47n),
          );
          same(await words(machine, RAM, 3), [0x124n, 0x21n, 0n], 'one update per lane');
        });
      },
    },
    {
      name: 'shared locked ADD loop pauses and resumes after a peer promotes its stripe',
      async run() {
        // A: LOCK ADD qword [RBX],1; JMP entry. B waits for two updates, then reads [RBX+5].
        const code = new Uint8Array(48).fill(0x90);
        code.set(hex('f048830301ebf9'));
        code.set(hex('48833b0272fa488b4305'), 32);
        const lock = ENTRY;
        const jump = ENTRY + 5n;
        const peerEntry = ENTRY + 32n;
        const peerStop = peerEntry + 10n;
        const budget = 10_000_000_000n;
        const before = cpuState(lock, { 3: RAM });
        const flagsAfter = (value: bigint) => {
          const byte = Number(value & 0xffn);
          const even = Array.from({ length: 8 }, (_, bit) => (byte >> bit) & 1).reduce(
            (parity, bit) => parity ^ bit,
            0,
          );
          return 2n | (even ? 0n : 4n) | (value % 16n === 0n ? 16n : 0n);
        };

        await withMachine(factory, linearMachine(compiler, code), async (machine) => {
          const [busy, peer] = machine.cpus;
          await busy!.setState(before);
          await peer!.setState(cpuState(peerEntry, { 3: RAM }));
          const background = busy!.run({ budget });
          const other = await peer!.run({ budget: 2_000_000n, stop: peerStop });
          done(other, peerStop);
          equal(other.state.registers[0], 0n, 'split read spans unchanged and upper counter bytes');
          await busy!.pause();

          const paused = await background;
          const count = (paused.retired + 1n) / 2n;
          const rip = paused.retired % 2n === 0n ? lock : jump;
          equal(count >= 2n, true, 'peer witnessed two updates before promotion');
          exit(
            paused,
            'paused',
            budget,
            paused.retired,
            changed(before, rip, {}, flagsAfter(count)),
          );
          same(await words(machine, RAM, 2), [count, 0n], 'exact committed prefix at pause');

          const resumed = await busy!.run({ budget: 2n });
          exit(resumed, 'budget', 2n, 2n, changed(before, rip, {}, flagsAfter(count + 1n)));
          same(await words(machine, RAM, 2), [count + 1n, 0n], 'one more update after promotion');
        });
      },
    },
    {
      name: 'exchange through overlapping aliases snapshots once and later logical bytes win',
      async run() {
        const options = linearMachine(compiler, '48870390');
        const at = RAM + 0x40n;
        const alias = 0x8003n;
        options.memory.backings[1]!.bytes.set(asBytes(0x8877665544332211n), 32);
        options.memory.mappings.splice(
          1,
          1,
          span(at, 'data', 32, 3),
          span(at + 3n, 'data', 33, 3),
          span(at + 6n, 'data', 36, 2),
          span(alias, 'data', 32, 8),
        );
        await withMachine(factory, options, async (machine) => {
          const [writer, observer] = machine.cpus;
          await writer!.setState(cpuState(ENTRY, { 0: 0x1807f6e5d4c3b2a1n, 3: at }));
          const result = await writer!.run({ budget: 2n, stop: ENTRY + 3n });
          done(result, ENTRY + 3n, 1n);
          equal(result.state.registers[0], 0x6655443322332211n, 'original logical snapshot');
          same(
            await words(machine, alias, 1),
            [0x88771807f6e5d4a1n],
            'later guest offsets replace duplicate canonical bytes',
          );
          await observer!.setState(cpuState(ENTRY, { 0: 0x0123456789abcdefn, 3: alias }));
          const observed = await observer!.run({ budget: 2n, stop: ENTRY + 3n });
          done(observed, ENTRY + 3n, 1n);
          equal(observed.state.registers[0], 0x88771807f6e5d4a1n, 'peer sees one canonical lane');
        });
      },
    },
    {
      name: 'narrow locked updates through distinct aliases preserve neighboring bytes and exact retirements',
      async run() {
        // lock inc byte [rbx]; dec ecx; jnz entry.
        const options = linearMachine(compiler, 'f0fe03ffc975f9');
        const alias = 0x8000n;
        options.memory.mappings.push(span(alias, 'data', 0, 4096));
        await withMachine(factory, options, async (machine) => {
          const count = 65539n;
          const [a, b] = machine.cpus;
          const stop = ENTRY + 7n;
          // Warm each native loop before starting the contention case.
          await a!.setState(cpuState(ENTRY, { 1: 1n, 3: RAM }));
          done(await a!.run({ budget: 4n, stop }), stop, 3n);
          await b!.setState(cpuState(ENTRY, { 1: 1n, 3: alias + 1n }));
          done(await b!.run({ budget: 4n, stop }), stop, 3n);
          await machine.writeMemory(RAM, Uint8Array.of(0, 0, 0xad, 0xde, 0x99, 0x21, 0xaf, 0x59));
          await Promise.all([
            a!.setState(cpuState(ENTRY, { 1: count, 3: RAM })),
            b!.setState(cpuState(ENTRY, { 1: count, 3: alias + 1n })),
          ]);
          const r = await Promise.all(
            machine.cpus.map((cpu) => cpu.run({ budget: count * 3n + 1n, stop })),
          );
          for (const result of r) done(result, stop, count * 3n);
          same(
            [...(await machine.readMemory(alias, 8))],
            [3, 3, 0xad, 0xde, 0x99, 0x21, 0xaf, 0x59],
            'target bytes and never-addressed neighbors',
          );
        });
      },
    },
    {
      name: 'guest atomicity is retained for a split-backed XCHG and ordinary overlapping readers',
      async run() {
        // Writer: xchg [rbx],rax; dec ecx; jnz. Reader: mov rax,[rbx]; cmp rax,rsi;
        // je next; cmp rax,rdi; jne bad; next: dec ecx; jnz reader; nop; bad: ud2.
        const program = new Uint8Array(80).fill(0x90);
        program.set([0x48, 0x87, 0x03, 0xff, 0xc9, 0x75, 0xf9]);
        program.set(
          [
            0x48, 0x8b, 0x03, 0x48, 0x39, 0xf0, 0x74, 0x05, 0x48, 0x39, 0xf8, 0x75, 0x05, 0xff,
            0xc9, 0x75, 0xef, 0x90, 0x0f, 0x0b,
          ],
          32,
        );
        const options = linearMachine(compiler, program);
        const at = RAM + 0x40n;
        options.memory.mappings.splice(
          1,
          1,
          span(at, 'data', 61, 3),
          span(at + 3n, 'data', 192, 5),
        );
        const low = 0x7766554433221100n,
          high = 0xcafedecab00badc0n;
        await withMachine(factory, options, async (machine) => {
          const count = 20_000n,
            reader = ENTRY + 32n;
          await machine.writeMemory(at, asBytes(low));
          const [writer, observer] = machine.cpus;
          await writer!.setState(cpuState(ENTRY, { 0: high, 1: count, 3: at }));
          await observer!.setState(cpuState(reader, { 1: count, 3: at, 6: low, 7: high }));
          const [w, r] = await Promise.all([
            writer!.run({ budget: count * 3n + 1n, stop: ENTRY + 7n }),
            observer!.run({ budget: count * 7n + 1n, stop: reader + 17n }),
          ]);
          done(w, ENTRY + 7n, count * 3n);
          done(r, reader + 17n);
          equal(
            r.state.registers[1],
            0n,
            'reader consumes every iteration without torn byte diagnostic',
          );
          same(await words(machine, at, 1), [low], 'even swap count returns original full value');
          equal(w.state.registers[0], high, 'writer register holds the other complete value');
        });
      },
    },
    {
      name: 'shared MOVSS reads a coherent dword across two canonical lanes',
      async run() {
        // Writer: XCHG [RBX],RAX; DEC ECX; JNZ writer.
        const writerCode = hex('488703ffc975f9');
        const readerCode = hex(
          'f30f1003660f7ec0' + // MOVSS XMM0,[RBX]; MOVD EAX,XMM0
            '39f0740439f87505' + // CMP EAX,ESI; JE next; CMP EAX,EDI; JNE bad
            'ffc975ec900f0b', // next: DEC ECX; JNZ reader; NOP; bad: UD2
        );
        const options = linearMachine(compiler, Uint8Array.of(...writerCode, ...readerCode));
        const reader = ENTRY + BigInt(writerCode.length);
        const stop = reader + 20n;
        const at = RAM + 5n;
        const low = 0x7766554433221100n;
        const high = 0xcafedecab00badc0n;
        options.memory.backings[1]!.bytes.set(asBytes(low), 5);
        const readerState = (count: bigint) =>
          cpuState(reader, { 1: count, 3: at, 6: low & 0xffffffffn, 7: high & 0xffffffffn });
        await withMachine(factory, options, async (machine) => {
          const [writer, observer] = machine.cpus;
          // First promote the stripe and compile both loops before contending on it.
          await observer!.setState(readerState(1n));
          done(await observer!.run({ budget: 9n, stop }), stop, 6n);
          await writer!.setState(cpuState(ENTRY, { 0: high, 1: 2n, 3: at }));
          done(await writer!.run({ budget: 7n, stop: reader }), reader, 6n);

          const count = 20_000n;
          await writer!.setState(cpuState(ENTRY, { 0: high, 1: count, 3: at }));
          await observer!.setState(readerState(count));
          const [w, r] = await Promise.all([
            writer!.run({ budget: count * 3n + 1n, stop: reader }),
            observer!.run({ budget: count * 8n + 1n, stop }),
          ]);
          done(w, reader, count * 3n);
          done(r, stop);
          equal(r.state.registers[1], 0n, 'every MOVSS returned an allowed dword');
          same(await words(machine, at, 1), [low], 'even swaps restore the full qword');
          equal(w.state.registers[0], high, 'writer holds the other full qword');
        });
      },
    },
    ...parallelGuardedLoopScenarios(factory, compiler),
    {
      name: 'shared SIMD dword reads preserve lanes and precise split faults',
      async run() {
        // MOVSS XMM0,FS:[RAX]; MOVSS XMM1,FS:[RBX].
        const options = linearMachine(compiler, '64f30f100064f30f100b');
        const tls = 0x1_0000_4000n;
        options.cpus[0]!.context = { mode: 'long64', fsBase: tls, gsBase: 0n };
        options.memory.backings[1]!.bytes.set(Uint8Array.from({ length: 130 }, (_, i) => i));
        const tail = span(tls + 64n, 'data', 128, 2);
        options.memory.mappings.splice(1, 1, span(tls, 'data', 0, 64), {
          ...tail,
          permissions: { ...tail.permissions, read: false },
        });
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          const raw = Array<bigint>(16).fill(0x1234567890abcdef1122334455667788n);
          const stop = ENTRY + 10n;
          const read = async (second: bigint) => {
            await cpu.setState(cpuState(ENTRY, { 0: 1n, 3: second }));
            await cpu.setXmmState(raw);
            return cpu.run({ budget: 3n, stop });
          };
          done(await read(5n), stop, 2n);
          same(
            await cpu.readXmmState(),
            [0x04030201n, 0x08070605n, ...raw.slice(2)],
            'same-lane and cross-lane unaligned dwords both clear the upper bits',
          );

          const denied = await read(62n);
          equal(denied.reason, 'data-fault', 'denied second fragment');
          equal(denied.state.rip, ENTRY + 5n, 'retry at MOVSS');
          equal(denied.retired, 1n, 'only the first MOVSS retired');
          fault(denied, {
            instructionPc: ENTRY + 5n,
            address: tls + 62n,
            width: 4,
            access: 'read',
            cause: 'permission',
            failingAddress: tls + 64n,
            segment: 'fs',
          });
          same(
            await cpu.readXmmState(),
            [0x04030201n, ...raw.slice(1)],
            'fault preserves destination',
          );

          await machine.setMappings([...options.memory.mappings.slice(0, 2), tail]);
          done(await cpu.run({ budget: 2n, stop }), stop, 1n);
          same(
            await cpu.readXmmState(),
            [0x04030201n, 0x81803f3en, ...raw.slice(2)],
            'retry assembles both readable fragments',
          );
        });
      },
    },
    {
      name: 'split locked write fault publishes no partial bytes and exact retry retires once',
      async run() {
        // xchg [rbx],rax in the split-backed window, with only the second mapping write-protected.
        const options = linearMachine(compiler, '48870390');
        const at = RAM + 0x40n;
        const old = 0x7766554433221100n,
          value = 0x0011223344556677n;
        const data = options.memory.backings[1]!.bytes;
        const prefix = asBytes(old);
        data.set(prefix.subarray(0, 3), 61);
        data.set(prefix.subarray(3), 192);
        options.memory.mappings.splice(
          1,
          1,
          span(at, 'data', 61, 3),
          span(at + 3n, 'data', 192, 5, 'r'),
        );
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          await cpu.setState(cpuState(ENTRY, { 0: value, 3: at }));
          const refused = await cpu.run({ budget: 2n, stop: ENTRY + 3n });
          equal(refused.reason, 'data-fault', 'late permission check fault');
          equal(refused.fault?.failingAddress, at + 3n, 'precise denied fragment');
          equal(refused.retired, 0n, 'uncommitted instruction did not retire');
          equal(refused.state.rip, ENTRY, 'retry at instruction start');
          equal(
            refused.state.registers[0],
            value,
            'failed atomic read leaves accumulator unchanged',
          );
          same(await words(machine, at, 1), [old], 'first writable fragment untouched');
          await machine.setMappings(
            options.memory.mappings.map((m) =>
              m.base === at + 3n ? span(at + 3n, 'data', 192, 5) : m,
            ),
          );
          const complete = await cpu.run({ budget: 2n, stop: ENTRY + 3n });
          done(complete, ENTRY + 3n, 1n);
          equal(complete.state.registers[0], old, 'one authorized atomic exchange');
          same(await words(machine, at, 1), [value], 'both fragments written');
        });
      },
    },
  ];
}
