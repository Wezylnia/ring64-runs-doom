import type { Registers } from '../../../sdk/src/index.js';
import { context } from '../modes/fixture.js';
import { changed, exit, fault, type Scenario } from '../../support/fixture.js';
import {
  ENTRY,
  RAM,
  cpuState,
  done,
  equal,
  hex,
  initial,
  linearMachine,
  same,
  span,
  withMachine,
  type MachineFactory,
} from './fixture.js';

export function parallelGuardedLoopScenarios(
  factory: MachineFactory,
  compiler: WebAssembly.Module,
): Scenario[] {
  return [
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

          const repaired: Registers = [...failed.state.registers];
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
      name: 'shared byte reads across two windows keep distinct bases and precise faults',
      async run() {
        // MOVZX EAX,[RDI+RSI]; MOVSX EDX,[RBX]; INC ESI; INC RBX; DEC ECX; JNE entry.
        const code = hex('0fb604370fbe13ffc648ffc3ffc975f0');
        const stop = ENTRY + BigInt(code.length);
        const indexed = 0xffff_8000_0000_4000n;
        const plain = RAM;
        const options = linearMachine(compiler, code);
        const program = options.memory.mappings[0]!;
        const high = span(indexed, 'data', 0, 16, 'r');
        options.memory.mappings = [program, span(plain, 'data', 16, 15, 'r'), high];
        const bytes = options.memory.backings[1]!.bytes;
        bytes[7] = 0x71;
        bytes[15] = 0x55;
        bytes[16 + 7] = 0x91;
        bytes[16 + 14] = 0xe2;
        bytes[16 + 15] = 0x80;
        const input = cpuState(ENTRY, {
          0: 0x11223344deadbeefn,
          1: 10n,
          2: 0x55667788deadbeefn,
          3: plain + 6n,
          6: 6n,
          7: indexed,
        });

        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          await cpu.setState(input);
          const warm = changed(input, ENTRY, {
            0: 0x71n,
            1: 8n,
            2: 0xffff_ff91n,
            3: plain + 8n,
            6: 8n,
          });
          exit(await cpu.run({ budget: 12n, stop }), 'budget', 12n, 12n, warm);

          const failed = await cpu.run({ budget: 64n, stop });
          const partial = changed(warm, ENTRY + 4n, {
            0: 0x55n,
            1: 1n,
            2: 0xffff_ffe2n,
            3: plain + 15n,
            6: 15n,
          });
          exit(failed, 'data-fault', 64n, 43n, partial);
          fault(failed, {
            instructionPc: ENTRY + 4n,
            address: plain + 15n,
            width: 1,
            access: 'read',
            cause: 'unmapped',
            failingAddress: plain + 15n,
            segment: 'ds',
          });

          await machine.setMappings([program, span(plain, 'data', 16, 16, 'r'), high]);
          const completed = changed(
            partial,
            stop,
            { 1: 0n, 2: 0xffff_ff80n, 3: plain + 16n, 6: 16n },
            0x46n,
          );
          exit(await cpu.run({ budget: 8n, stop }), 'stop', 8n, 5n, completed);
        });
      },
    },
    {
      name: 'shared indexed byte stores preserve their effects and retry after stripe promotion',
      async run() {
        // MOV [RDI+RSI],DL; XCHG [RDI+RSI],EAX; MOV [RDI+RSI+1],CL;
        // MOV [RDI+RSI+4],DL; DEC EBX; JNE entry.
        const code = hex('881437870437884c370188543704ffcb75ee');
        const options = linearMachine(compiler, code);
        const base = 0xffff_8000_0000_4000n;
        const denied = base + 9n;
        const stop = ENTRY + BigInt(code.length);
        const store = ENTRY + 10n;
        const codeMap = options.memory.mappings[0]!;
        options.memory.mappings = [
          codeMap,
          span(base, 'data', 0, 9),
          span(denied, 'data', 9, 1, 'r'),
        ];
        options.memory.backings[1]!.bytes.set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          const before = cpuState(ENTRY, {
            0: 0x11223344n,
            1: 0xbbn,
            2: 0xaan,
            3: 1n,
            6: 5n,
            7: base,
          });
          await cpu.setState(before);
          const failed = await cpu.run({ budget: 8n, stop });
          // The unaligned four-byte exchange crosses two atomic lanes.
          const committed = changed(before, store, { 0: 0x080706aan });
          exit(failed, 'data-fault', 8n, 3n, committed);
          fault(failed, {
            instructionPc: store,
            address: denied,
            width: 1,
            access: 'write',
            cause: 'permission',
            failingAddress: denied,
            segment: 'ds',
          });
          const memory = [0, 1, 2, 3, 4, 0x44, 0xbb, 0x22, 0x11, 9];
          same(
            [...(await machine.readMemory(base, 10))],
            memory,
            'only the first three stores took effect',
          );

          await machine.setMappings([codeMap, span(base, 'data', 0, 10)]);
          exit(
            await cpu.run({ budget: 8n, stop }),
            'stop',
            8n,
            3n,
            changed(committed, stop, { 3: 0n }, 0x46n),
          );
          memory[9] = 0xaa;
          same([...(await machine.readMemory(base, 10))], memory, 'only the repaired byte changes');
        });
      },
    },
    {
      name: 'shared natural read-store loops preserve short budgets and checked writes',
      async run() {
        // MOVZX EAX,[RSI]; CMP EAX,2; JB store; ADD EAX,64;
        // store: MOV [RDI+RBX],AL; INC RDI; INC RSI; SUB ECX,1; JNE head.
        const options = linearMachine(
          compiler,
          '0fb60683f802720383c04088041f48ffc748ffc683e90175e790',
        );
        const table = RAM;
        const output = RAM + 0x100n;
        const store = ENTRY + 11n;
        const stop = ENTRY + 25n;
        const data = options.memory.backings[1]!.bytes;
        data.set([1, 2, 3, 0]);
        data.set([0x7a, 0x7b, 0x7c, 0x7d], 256);
        const code = options.memory.mappings[0]!;
        const input = span(table, 'data', 0, 4, 'r');
        options.memory.mappings = [
          code,
          input,
          span(output, 'data', 256, 1),
          span(output + 1n, 'data', 257, 3, 'r'),
        ];
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          const initial = {
            ...cpuState(ENTRY, { 1: 4n, 3: 0n, 6: table, 7: output }),
            flags: 0x202n,
          };
          await cpu.setState(initial);
          exit(
            await cpu.run({ budget: 20n, stop: store }),
            'stop',
            20n,
            3n,
            changed(initial, store, { 0: 1n }, 0x297n),
          );
          same(
            [...(await machine.readMemory(output, 4))],
            [0x7a, 0x7b, 0x7c, 0x7d],
            'stopped before store',
          );

          await cpu.setState(initial);
          const partial = changed(
            initial,
            ENTRY + 3n,
            { 0: 2n, 1: 3n, 6: table + 1n, 7: output + 1n },
            0x206n,
          );
          exit(await cpu.run({ budget: 9n, stop }), 'budget', 9n, 9n, partial);
          const denied = await cpu.run({ budget: 50n, stop });
          exit(denied, 'data-fault', 50n, 3n, changed(partial, store, { 0: 0x42n }));
          fault(denied, {
            instructionPc: store,
            address: output + 1n,
            width: 1,
            access: 'write',
            cause: 'permission',
            failingAddress: output + 1n,
            segment: 'ds',
          });
          same(
            [...(await machine.readMemory(output, 4))],
            [1, 0x7b, 0x7c, 0x7d],
            'only first store retired',
          );

          await machine.setMappings([code, input, span(output, 'data', 256, 4)]);
          done(await cpu.run({ budget: 50n, stop }), stop, 22n);
          same(
            [...(await machine.readMemory(output, 4))],
            [1, 0x42, 0x43, 0],
            'repair resumes the denied store',
          );
        });
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
  ];
}
