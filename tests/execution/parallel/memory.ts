import { context } from '../modes/fixture.js';
import { changed, exit, fault, type Scenario } from '../../support/fixture.js';
import {
  ENTRY,
  RAM,
  asBytes,
  cpuState,
  done,
  equal,
  hex,
  initial,
  linearMachine,
  same,
  span,
  withMachine,
  words,
  type MachineFactory,
} from './fixture.js';

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
    {
      name: 'shared float loops preserve instruction boundaries after a guarded read',
      async run() {
        // MOVSS XMM0,[EAX]; MULSS XMM0,[EAX+4]; ADDSS XMM1,XMM0; SUB ECX,1; JNE entry.
        const code = hex('f30f1000f30f594004f30f58c883e90175ee');
        const stop = ENTRY + BigInt(code.length);
        for (const long of [false, true]) {
          const options = linearMachine(compiler, code);
          if (!long) options.cpus[0]!.context = context('protected');
          const input = new DataView(options.memory.backings[1]!.bytes.buffer);
          input.setFloat32(5, 2, true); // This dword promotes the stripe on the first run.
          input.setFloat32(9, 3, true);
          await withMachine(factory, options, async (machine) => {
            const cpu = machine.cpus[0]!;
            const upper = 0x1234567890abcdef13579bdf00000000n;
            const xmm = [upper, upper | 0x3f800000n, ...Array<bigint>(14).fill(upper)];
            const resetInputs = async () => {
              await cpu.setState(cpuState(ENTRY, { 0: RAM + 5n, 1: 2n }));
              await cpu.setXmmState(xmm);
            };
            const expectXmm = async (product: bigint, sum: bigint) => {
              same(await cpu.readXmmState(), [product, upper | sum, ...xmm.slice(2)], 'XMM state');
            };

            await resetInputs();
            done(await cpu.run({ budget: 12n, stop }), stop, 10n);
            await expectXmm(0x40c00000n, 0x41500000n);

            await resetInputs();
            const partial = await cpu.run({ budget: 6n, stop });
            equal(partial.reason, 'budget', 'one lap and one MOVSS');
            equal(partial.retired, 6n, 'six original instructions');
            equal(partial.state.rip, ENTRY + 4n, 'resume at the second MULSS');
            equal(partial.state.registers[1], 1n, 'only the first subtraction retired');
            await expectXmm(0x40000000n, 0x40e00000n);
            done(await cpu.run({ budget: 8n, stop }), stop, 4n);
            await expectXmm(0x40c00000n, 0x41500000n);

            await resetInputs();
            const interior = ENTRY + 9n;
            done(await cpu.run({ budget: 12n, stop: interior }), interior, 2n);
            await expectXmm(0x40c00000n, 0x3f800000n);
          });
        }
      },
    },
    {
      name: 'shared rotated read loops keep shift flags at a late memory fault',
      async run() {
        // The comparison at +17 can branch to +2 before the loop head at +12.
        const code = 'eb0a89c183c70483ee01740ed1e88d5001391f72ed83ee0175f290';
        const options = linearMachine(compiler, code);
        const data = RAM + 5n;
        const read = ENTRY + 17n;
        const stop = ENTRY + 26n;
        options.cpus[0] = { entry: ENTRY + 12n, context: context('protected') };
        options.memory.mappings[1] = span(data, 'data', 5, 8, 'r');
        options.memory.backings[1]!.bytes[5] = 1;
        options.memory.backings[1]!.bytes[9] = 1;
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          const high = 0x1122334400000000n;
          await cpu.setState(
            cpuState(ENTRY + 12n, {
              0: high | 0x80000007n,
              1: high,
              2: high,
              3: 2n,
              6: 3n,
              7: data,
            }),
          );
          const failed = await cpu.run({ budget: 64n, stop });
          equal(failed.reason, 'data-fault', 'third operand crosses the mapping');
          equal(failed.state.rip, read, 'fault at the original CMP');
          equal(failed.state.flags, 7n, 'preceding SHR flags');
          equal(failed.retired, 18n, 'only instructions before the fault retired');
          equal(failed.state.registers[7], data + 8n, 'earlier branch updated the address twice');
          fault(failed, {
            instructionPc: read,
            address: data + 8n,
            width: 4,
            access: 'read',
            cause: 'unmapped',
            failingAddress: data + 8n,
            segment: 'ds',
          });
          await machine.setMappings([options.memory.mappings[0]!, span(data, 'data', 5, 12, 'r')]);
          done(await cpu.run({ budget: 2n, stop: ENTRY + 19n }), ENTRY + 19n, 1n);
        });
      },
    },
    {
      name: 'shared indexed dword reads stop at the DS limit and restart after address repair',
      async run() {
        // MOV EAX,[EDI+ESI]; ADD ESI,4; DEC ECX; JNE entry.
        const code = hex('8b043783c6044975f7');
        const options = linearMachine(compiler, code);
        const top = 0xfffffff0n;
        const stop = ENTRY + BigInt(code.length);
        options.cpus[0]!.context = context('protected');
        options.memory.mappings[1] = span(top, 'data', 3, 32, 'r');
        const data = new DataView(options.memory.backings[1]!.bytes.buffer);
        // The first two reads map to backing offsets 8 and 12.
        data.setUint32(8, 0x12345678, true);
        data.setUint32(12, 0x87654321, true);
        data.setUint32(16, 0xddccbbaa, true);
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          const high = 0x1122334400000000n;
          const input = cpuState(ENTRY, { 0: high, 1: high | 3n, 6: high, 7: high | (top + 5n) });
          const startTsc = (await cpu.readTimeState()).tsc;
          await cpu.setState(input);
          const failed = await cpu.run({ budget: 16n, stop });
          equal(failed.reason, 'segment-fault', 'third dword crosses the 4 GiB segment end');
          equal(failed.state.rip, ENTRY, 'fault at the third indexed MOV');
          equal(failed.state.flags, 2n, 'second DEC flags');
          equal(failed.retired, 8n, 'only two full laps retired');
          equal(failed.remaining, 8n, 'fault did not consume budget');
          const committed = input.registers.slice();
          committed[0] = high | 0x87654321n;
          committed[1] = high | 1n;
          committed[6] = high | 8n;
          same(
            failed.state.registers,
            committed,
            'committed reader state and preserved upper dwords',
          );
          same(
            failed.segmentFault,
            {
              instructionPc: ENTRY,
              segment: 'ds',
              offset: 0xfffffffdn,
              width: 4,
              access: 'read',
              cause: 'limit',
              failingOffset: 0x100000000n,
            },
            'exact first byte beyond DS',
          );
          equal(
            (await cpu.readTimeState()).tsc,
            startTsc + 8n,
            'TSC counts only the completed laps',
          );

          const repaired = failed.state.registers.slice();
          repaired[7] = high | (top + 4n); // With ESI=8 the retry starts at 0xfffffffc.
          await cpu.setState({ ...failed.state, registers: repaired });
          const completed = await cpu.run({ budget: 5n, stop });
          done(completed, stop, 4n);
          repaired[0] = high | 0xccbbaa87n;
          repaired[1] = high;
          repaired[6] = high | 12n;
          same(completed.state.registers, repaired, 'retry loads the last valid dword once');
          equal(completed.state.flags, 0x46n, 'final DEC flags');
          equal((await cpu.readTimeState()).tsc, startTsc + 12n, 'TSC includes the repaired lap');
        });
      },
    },
    {
      name: 'shared Long64 indexed reads distinguish equal low halves and fault at the canonical boundary',
      async run() {
        // MOV EAX,[RDI+RSI]; ADD RSI,RDX; ADD RDX,5; DEC ECX; JNE entry.
        const code = hex('8b04374801d64883c205ffc975f2');
        const options = linearMachine(compiler, code);
        const stop = ENTRY + BigInt(code.length);
        const high = 0xffff_8001_ffff_fff8n;
        const low = 0x0000_0000_ffff_fff8n;
        const hole = 0x0000_8000_0000_0000n;
        const stride = BigInt.asUintN(64, low - high);
        options.memory.mappings.splice(
          1,
          1,
          span(high - 8n, 'data', 0, 16, 'r'),
          span(low - 8n, 'data', 16, 16, 'r'),
          span(hole - 8n, 'data', 32, 8, 'r'),
        );
        const data = new DataView(options.memory.backings[1]!.bytes.buffer);
        data.setUint32(8, 0x1234abcd, true);
        data.setUint32(24, 0xfedcba98, true);
        data.setUint32(36, 0x76543210, true);

        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          const input = cpuState(ENTRY, { 1: 3n, 2: stride, 6: high, 7: 0n });
          await cpu.setState(input);
          // One run visits high, low (same low 32 bits), then hole - 3.
          const failed = await cpu.run({ budget: 16n, stop });
          const committed = changed(input, ENTRY, {
            0: 0xfedcba98n,
            1: 1n,
            2: stride + 10n,
            6: hole - 3n,
          });
          exit(failed, 'data-fault', 16n, 10n, committed);
          fault(failed, {
            instructionPc: ENTRY,
            address: hole - 3n,
            width: 4,
            access: 'read',
            cause: 'noncanonical',
            failingAddress: hole,
            segment: 'ds',
          });

          const repaired = changed(failed.state, ENTRY, { 6: hole - 4n });
          await cpu.setState(repaired);
          const completed = changed(
            committed,
            stop,
            { 0: 0x76543210n, 1: 0n, 2: stride + 15n, 6: hole + stride + 6n },
            0x46n,
          );
          exit(await cpu.run({ budget: 6n, stop }), 'stop', 6n, 5n, completed);
        });
      },
    },
    {
      name: 'shared indexed byte reads extend through the last mapped byte and resume after repair',
      async run() {
        // MOVSX AX,[base+index]; MOVZX EDX,[base+index]; INC ESI; DEC ECX; JNE entry.
        const code = hex('660fbe04370fb61437ffc6ffc975f1');
        const stop = ENTRY + BigInt(code.length);
        const poison = 0x1122334400000000n;
        for (const long of [false, true]) {
          const options = linearMachine(compiler, code);
          const base = long ? 0xffff_8000_0000_4000n : RAM;
          const end = base + 16n;
          if (!long) options.cpus[0]!.context = context('protected');
          options.memory.mappings[1] = span(base, 'data', 0, 16, 'r');
          const data = options.memory.backings[1]!.bytes;
          data.set([0x4d, 0x91], 6); // A window fill followed by the last byte of its atomic lane.
          data.set([0xe2, 0x80], 15); // The last mapped byte and the byte exposed after repair.
          const upperDword = long ? 0n : poison;
          const registers = (ax: bigint, edx: bigint, count: bigint, offset: bigint) => ({
            0: poison | 0xabcd0000n | ax,
            1: upperDword | count,
            2: upperDword | edx,
            6: upperDword | offset,
            7: upperDword | base,
          });
          await withMachine(factory, options, async (machine) => {
            const cpu = machine.cpus[0]!;
            await cpu.setState(
              cpuState(ENTRY, { ...registers(0x5678n, 0n, 11n, 6n), 2: poison | 0xdeadbeefn }),
            );
            exit(
              await cpu.run({ budget: 10n, stop }),
              'budget',
              10n,
              10n,
              initial(ENTRY, registers(0xff91n, 0x91n, 9n, 8n), 6n),
            );
            const failed = await cpu.run({ budget: 48n, stop });
            exit(
              failed,
              'data-fault',
              48n,
              40n,
              cpuState(ENTRY, registers(0xffe2n, 0xe2n, 1n, 16n)),
            );
            fault(failed, {
              instructionPc: ENTRY,
              address: end,
              width: 1,
              access: 'read',
              cause: 'unmapped',
              failingAddress: end,
              segment: 'ds',
            });

            await machine.setMappings([
              options.memory.mappings[0]!,
              span(base, 'data', 0, 17, 'r'),
            ]);
            exit(
              await cpu.run({ budget: 8n, stop }),
              'stop',
              8n,
              5n,
              initial(stop, registers(0xff80n, 0x80n, 0n, 17n), 0x46n),
            );
          });
        }
      },
    },
    {
      name: 'shared indexed float reads keep exact bounds after promotion and remap',
      async run() {
        // MOVSS [RAX+RCX*4]; ADDSS [RBX+RCX*4]; accumulate; count; XCHG [RDI],RAX.
        const options = linearMachine(compiler, 'f30f100488f30f58048bf30f58c883c10139d175eb488707');
        const stop = ENTRY + 21n;
        const right = RAM + 0x100n;
        const code = options.memory.mappings[0]!;
        const lhs = span(RAM, 'data', 0, 32);
        const rhs = span(right, 'data', 256, 32, 'r');
        options.memory.mappings = [code, lhs, rhs];
        const data = new DataView(options.memory.backings[1]!.bytes.buffer);
        for (let offset = 0; offset < 32; offset += 4) {
          data.setFloat32(offset, 1, true);
          data.setFloat32(256 + offset, 2, true);
          data.setFloat32(512 + offset, 4, true);
        }
        await withMachine(factory, options, async (machine) => {
          const [reader, writer] = machine.cpus;
          const execute = async () => {
            await reader!.setState(cpuState(ENTRY, { 0: RAM, 1: 0n, 2: 8n, 3: right }));
            await reader!.setXmmState(Array<bigint>(16).fill(0n));
            return reader!.run({ budget: 64n, stop });
          };

          done(await execute(), stop, 48n);
          equal((await reader!.readXmmState())[1], 0x41c00000n, 'eight sums of three');
          await writer!.setState(cpuState(stop, { 0: data.getBigUint64(5, true), 7: RAM + 5n }));
          done(await writer!.run({ budget: 2n, stop: stop + 3n }), stop + 3n, 1n);
          done(await execute(), stop, 48n);
          equal(
            (await reader!.readXmmState())[1],
            0x41c00000n,
            'same values after stripe promotion',
          );

          await machine.setMappings([code, span(RAM, 'data', 512, 32), rhs]);
          done(await execute(), stop, 48n);
          equal((await reader!.readXmmState())[1], 0x42400000n, 'remapped values');

          const denied = span(RAM + 14n, 'data', 14, 18, 'r');
          denied.permissions.read = false;
          await machine.setMappings([code, span(RAM, 'data', 0, 14, 'r'), denied, rhs]);
          const failure = await execute();
          equal(failure.state.rip, ENTRY, 'fault at the next indexed MOVSS');
          equal(failure.retired, 18n, 'three full iterations retired');
          fault(failure, {
            instructionPc: ENTRY,
            address: RAM + 12n,
            width: 4,
            access: 'read',
            cause: 'permission',
            failingAddress: RAM + 14n,
            segment: 'ds',
          });
          equal((await reader!.readXmmState())[1], 0x41100000n, 'three committed sums');
        });
      },
    },
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
