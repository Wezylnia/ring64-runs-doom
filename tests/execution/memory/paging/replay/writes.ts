import { observeArtifactImports } from '../../../../support/artifact-imports.js';
import { context } from '../../../processor/modes/fixture.js';
import {
  BANK,
  check,
  equal,
  initial,
  little,
  same,
  usingPaged,
  word,
  type Scenario,
} from '../fixture.js';

// MOV EAX,[ESP+4]; CMP EAX,[ESP+8]; JBE spill; INC EAX; MOV [ESP+4],EAX;
// MOVZX EDX,[EDI]; ADD EDX,EAX; MOV [ESI],DL; INC ESI; INC EDI; DEC ECX; JNZ head.
const PAGED_STACK_SPILL_LOOP = '8b4424043b442408760140894424040fb61701c2881646474975e5';
// MOVZX EAX,[RDI]; CMP AL,40h; JBE store; INC AL; MOV [RSI],AL;
// INC RDI; INC RSI; DEC ECX; JNZ head.
const PAGED_BYTE_STORE_LOOP = '0fb6073c407602fec0880648ffc748ffc6ffc975eb';

export function writeReplayScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'paged 32-bit stack spills preserve dirty, segment, and partial budget boundaries',
      async run() {
        const head = 0x400000n;
        const data = 0x401000n;
        const spill = 0x402004n;
        const dest = 0x403000n;
        for (const layout of ['flat', 'read-only-pte', 'nonflat', 'expand-down'] as const) {
          const execution = context('compatibility');
          if (layout === 'nonflat') execution.segments.ss.base = 0x400000n;
          if (layout === 'expand-down') {
            execution.segments.ss.limit = 0x402000;
            execution.segments.ss.type = 'expand-down-read-write';
          }
          const leaf = layout === 'read-only-pte' ? 0xb025n : 0xb027n;
          await usingPaged(
            compiler,
            {
              code: PAGED_STACK_SPILL_LOOP,
              context: execution,
              bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
              words: [
                [0x4008n, 0xa027n],
                [0x4010n, leaf],
                [0x4018n, 0xc067n],
              ],
              spans: [
                { physical: 0xa000n, bytes: Uint8Array.of(0xff, 2, 3) },
                { physical: 0xb004n, bytes: little([0xffn, 0n], 4) },
                { physical: 0xc000n, bytes: new Uint8Array(8) },
              ],
            },
            async (cpu) => {
              const start = initial(
                head,
                {
                  0: 0n,
                  1: 3n,
                  2: 0n,
                  4: spill - 4n - execution.segments.ss.base,
                  6: dest,
                  7: data,
                },
                2n,
              );
              cpu.setState(start);
              const warm = await cpu.run({ budget: 1n });
              equal(warm.state.registers[0], 0xffn, layout);
              equal(word(cpu, 0x4010n), leaf, 'the preceding read leaves D clear');
              cpu.setState(start);
              const prefix = await cpu.run({ budget: 16n });
              equal(prefix.state.rip, head + 11n, 'continues at the original dword spill');
              if (layout === 'read-only-pte') {
                equal(prefix.reason, 'page-fault', layout);
                equal(prefix.retired, 4n, 'read and increment are the full committed prefix');
                equal(prefix.pageFault?.address, spill, 'the checked SS write faults at the spill');
                equal(prefix.state.registers[0], 0x100n, 'increment commits before the fault');
                equal(word(cpu, 0x4010n), leaf, 'the refused store leaves D clear');
                equal(word(cpu, 0xb004n), 0xffn, 'the refused store leaves the slot intact');
                equal(cpu.readPhysicalMemory(0xc000n, 1)[0], 0, 'later writes do not run');
                return;
              }
              equal(prefix.reason, 'budget', layout);
              equal(prefix.retired, 16n, 'one lap and exactly four more instructions');
              equal(prefix.state.registers[0], 0x101n, 'the next dword has been computed');
              equal(word(cpu, 0x4010n), 0xb067n, 'checked retry commits the guest dirty bit');
              equal(word(cpu, 0xb004n), 0x100n, 'only the first lap wrote the slot');
              same([...cpu.readPhysicalMemory(0xc000n, 3)], [0xff, 0, 0], 'one byte committed');
              const end = await cpu.run({ budget: 40n, stop: head + 27n });
              equal(end.reason, 'stop', layout);
              equal(end.retired, 20n, 'resume completes exactly the unpaid instructions');
              equal(end.state.flags, 0x46n, 'the final decrement supplies flags');
              equal(word(cpu, 0xb004n), 0x102n, 'the dword is little endian and not replayed');
              same([...cpu.readPhysicalMemory(0xc000n, 3)], [0xff, 3, 5], 'all bytes committed');
              if (layout === 'flat') {
                cpu.setState(start);
                const retained = await cpu.run({ budget: 40n, stop: head + 27n });
                equal(retained.reason, 'stop', 'dirty-ready retained spill');
                equal(retained.retired, 36n, 'three more full laps retire');
                equal(word(cpu, 0xb004n), 0x105n, 'the retained stores reach the stack backing');
                same([...cpu.readPhysicalMemory(0xc000n, 3)], [2, 6, 8], 'retained DS byte output');
              }
            },
          );
        }
      },
    },
    {
      name: 'paged 32-bit dword spill through an executable alias republishes the next opcode',
      async run() {
        const head = 0x400000n;
        await usingPaged(
          compiler,
          {
            code: PAGED_STACK_SPILL_LOOP,
            context: context('compatibility'),
            words: [
              [0x4008n, 0xa067n],
              [0x4010n, 0x9067n],
              [0x4018n, 0xc067n],
            ],
            spans: [
              { physical: 0xa000n, bytes: Uint8Array.of(0xff) },
              { physical: 0xc000n, bytes: new Uint8Array(8) },
            ],
          },
          async (cpu) => {
            // The spill replaces DEC ECX at code offset 24 with DEC EDX through SS.
            const start = initial(
              head,
              { 0: 0n, 1: 3n, 2: 0n, 4: 0x402014n, 6: 0x403000n, 7: 0x401000n },
              2n,
            );
            cpu.setState(start);
            equal((await cpu.run({ budget: 1n })).reason, 'budget', 'warm the alias translation');
            cpu.setState(start);
            const result = await cpu.run({ budget: 12n });
            equal(result.reason, 'budget', 'a complete lap reaches its successor');
            equal(result.retired, 12n, 'the source-changing instruction retires once');
            equal(result.state.rip, head, 'the replaced decrement feeds JNZ');
            equal(result.state.registers[1], 3n, 'the original DEC ECX does not execute');
            equal(result.state.registers[2], 0x82e57648n, 'the newly decoded DEC EDX executes');
            equal(result.state.flags, 0x86n, 'new decrement flags are visible');
            same(
              [...cpu.readPhysicalMemory(0x9018n, 4)],
              [0x4a, 0x75, 0xe5, 0x82],
              'modified code',
            );
          },
        );
      },
    },
    {
      name: 'paged byte store loops commit ordinary bytes and replay a refused write precisely',
      async run() {
        const head = 0x400000n;
        const input = 0x401000n;
        const output = 0x402000n;
        const data = new Uint8Array(0x1000);
        data.set([0x20, 0x44, 0x41, 0x17]);
        await usingPaged(
          compiler,
          {
            code: PAGED_BYTE_STORE_LOOP,
            bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
            words: [
              [0x4008n, 0xa067n],
              [0x4010n, 0xb027n],
              [0x4018n, 0xc025n],
            ],
            spans: [
              { physical: 0xa000n, bytes: data },
              { physical: 0xb000n, bytes: new Uint8Array(0x1000) },
              { physical: 0xc000n, bytes: Uint8Array.of(0x99) },
            ],
          },
          async (cpu) => {
            const start = (source: bigint, dest: bigint, count = 4n) =>
              initial(head, { 1: count, 6: dest, 7: source }, 2n);
            for (let run = 0; run < 2; run++) {
              cpu.setState(start(input, output));
              const result = await cpu.run({ budget: 40n, stop: head + 21n });
              equal(result.reason, 'stop', 'both conditional paths reach the successor');
              equal(result.retired, 34n, 'the two selected increments retire');
              equal(result.state.flags, 0x47n, 'the final comparison carry survives DEC');
              same(
                [...cpu.readPhysicalMemory(0xb000n, 4)],
                [0x20, 0x45, 0x42, 0x17],
                'one output byte per source byte',
              );
              equal(word(cpu, 0x4010n), 0xb067n, 'the first checked write set the dirty bit');
            }

            const untouched = output + 16n;
            cpu.setState(start(input, untouched));
            const stopped = await cpu.run({ budget: 40n, stop: head + 9n });
            equal(stopped.reason, 'stop', 'the taken edge stops at the canonical store label');
            equal(stopped.retired, 3n, 'the selected branch skips the increment');
            equal(cpu.readPhysicalMemory(0xb010n, 1)[0], 0, 'the stopped store is unstarted');
            const resumed = await cpu.run({ budget: 1n });
            equal(resumed.reason, 'budget', 'direct reentry retires exactly the store');
            equal(resumed.retired, 1n, 'the prior branch does not replay');
            equal(resumed.state.rip, head + 11n, 'the store commits its true successor');
            equal(cpu.readPhysicalMemory(0xb010n, 1)[0], 0x20, 'the reentered store commits once');

            cpu.setState(start(input, untouched));
            const partial = await cpu.run({ budget: 10n });
            equal(partial.reason, 'budget', 'the next lap runs only its funded prefix');
            equal(partial.retired, 10n, 'the clone and checked prefix share the budget');
            equal(partial.state.rip, head + 5n, 'partial replay reaches the second branch');
            equal(partial.state.registers[0], 0x44n, 'the next source byte was loaded');
            equal(partial.state.registers[1], 3n, 'the second latch is unstarted');

            cpu.setState({ ...start(input, untouched), flags: 0x102n });
            const stepped = await cpu.run({ budget: 40n });
            equal(stepped.reason, 'debug-exception', 'TF takes the checked entry at a warm head');
            equal(stepped.retired, 1n, 'single-step sees the first source load only');
            equal(stepped.state.rip, head + 3n, 'single-step resumes at the comparison');

            // Retain the read-only translation before trying the guarded store.
            cpu.setState(start(0x403000n, output + 8n, 1n));
            equal((await cpu.run({ budget: 12n, stop: head + 21n })).reason, 'stop', 'warm read');
            cpu.setState(start(input, output + 0xffen));
            const fault = await cpu.run({ budget: 40n });
            equal(fault.reason, 'page-fault', 'the third write retains guest write protection');
            equal(fault.retired, 21n, 'completed prefix excludes the refused store');
            equal(fault.state.rip, head + 9n, 'the fault is attributed to the original store');
            equal(
              fault.state.registers[0],
              0x42n,
              'the increment preceding the failed store is live',
            );
            equal(fault.state.registers[1], 2n, 'only the completed laps decrement ECX');
            equal(fault.state.flags, 6n, 'the original store sees the increment flags');
            equal(
              fault.pageFault?.address,
              0x403000n,
              'the checked store publishes its linear address',
            );
            same([...cpu.readPhysicalMemory(0xbffen, 2)], [0x20, 0x45], 'earlier stores survive');
            equal(cpu.readPhysicalMemory(0xc000n, 1)[0], 0x99, 'the denied byte stays intact');
            equal(word(cpu, 0x4018n), 0xc025n, 'the refused store does not set guest dirty state');
          },
        );
      },
    },
    {
      name: 'paged byte store loop re-enters after a fresh read and preserves a later write fault',
      async run() {
        const head = 0x400000n;
        const src = 0x401fffn;
        const denied = 0x404000n;
        const first = new Uint8Array(0x1000);
        first[0xfff] = 0x20;
        const second = new Uint8Array(32).fill(0x41);
        let replayReady = false;
        let guardedWrites = 0;
        const guardRefusals: bigint[] = [];
        const restore = observeArtifactImports((env) => {
          const ready = env.paged_loop_read_ready;
          const write = env.paged_loop_write_byte;
          if (typeof ready === 'function' && typeof write === 'function') {
            env.paged_loop_read_ready = (address: bigint, width: number) => {
              const result = ready(address, width);
              if (result === 1) replayReady = true;
              return result;
            };
            env.paged_loop_write_byte = (address: bigint, value: number) => {
              const result = write(address, value);
              if (replayReady && result === 1) guardedWrites++;
              if (replayReady && result === 0) guardRefusals.push(address);
              return result;
            };
          }
        });
        try {
          await usingPaged(
            compiler,
            {
              code: PAGED_BYTE_STORE_LOOP,
              bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
              words: [
                [0x4008n, 0xa027n],
                [0x4010n, 0xb007n],
                [0x4018n, 0xc067n],
                [0x4020n, 0xd025n],
              ],
              spans: [
                { physical: 0xa000n, bytes: first },
                { physical: 0xb000n, bytes: second },
                { physical: 0xc000n, bytes: new Uint8Array(0x1000) },
                { physical: 0xd000n, bytes: Uint8Array.of(0xa5) },
              ],
            },
            async (cpu) => {
              cpu.setState(initial(head, { 1: 20n, 6: denied - 16n, 7: src }, 2n));
              const fault = await cpu.run({ budget: 200n });
              check(replayReady, 'the cold source page arms re-entry after its checked read');
              check(guardedWrites > 0, 'the reentered guard writes to the retained destination');
              same(guardRefusals, [denied], 'the later write exits the guarded path once');
              equal(fault.reason, 'page-fault', 'the checked write reports protection');
              equal(fault.retired, 147n, 'only the completed laps and faulting prefix retire');
              equal(fault.state.rip, head + 9n, 'the refused store remains unretired');
              equal(fault.state.registers[0], 0x42n, 'the faulting prefix computes its value');
              equal(fault.state.registers[1], 4n, 'only completed laps decrement the count');
              equal(fault.state.flags, 6n, 'replay preserves the preceding increment flags');
              equal(fault.pageFault?.address, denied, 'the checked write publishes its address');
              same(
                [...cpu.readPhysicalMemory(0xcff0n, 16)],
                [0x20, ...Array<number>(15).fill(0x42)],
                'all successful bytes commit before the fault',
              );
              equal(cpu.readPhysicalMemory(0xd000n, 1)[0], 0xa5, 'the denied byte is untouched');
              equal(word(cpu, 0x4010n), 0xb027n, 'the source read sets only the accessed bit');
              equal(word(cpu, 0x4020n), 0xd025n, 'the denied write leaves the PTE intact');
            },
          );
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged Long64 scalar writes rearm checked reads and writes before a partial lap',
      async run() {
        const head = 0x400000n;
        const output = 0x403fd0n;
        // MOVZX EAX,[RDI]; ADD R8D,[RSI]; LEA RDI,[RDI+1]; DEC ECX; JZ done;
        // TEST EAX,EAX; JZ head; MOV [RDX],R8D; LEA RDX,[RDX+4]; JMP head; done: NOP.
        const code = '0fb607440306488d7f01ffc9740d85c074ee448902488d5204ebe590';
        const input = new Uint8Array(40).fill(1);
        input[0] = 0;
        const routes: string[] = [];
        const restore = observeArtifactImports((env) => {
          for (const name of [
            'paged_loop_read_ready',
            'paged_loop_read_dword',
            'paged_loop_write_ready',
            'paged_loop_write_dword',
          ]) {
            const helper = env[name];
            if (typeof helper !== 'function') continue;
            env[name] = (...args: (bigint | number)[]) => {
              const result = helper(...args);
              if (name === 'paged_loop_read_dword' ? result !== -1n : result === 1)
                routes.push(name);
              return result;
            };
          }
        });
        try {
          await usingPaged(
            compiler,
            {
              code,
              words: [
                [0x4008n, 0xa007n],
                [0x4010n, 0xb007n],
                [0x4018n, 0xc007n],
                [0x4020n, 0xd007n],
              ],
              spans: [
                { physical: 0xa000n, bytes: input },
                { physical: 0xb000n, bytes: little([3n], 4) },
                { physical: 0xc000n, bytes: new Uint8Array(8192) },
              ],
            },
            async (cpu) => {
              const start = initial(
                head,
                { 1: 40n, 2: output, 6: 0x402000n, 7: 0x401000n, 8: 0n },
                2n,
              );
              cpu.setState(start);
              equal(
                (await cpu.run({ budget: 1n })).reason,
                'budget',
                'warm only fetch and byte input',
              );

              cpu.setState(start);
              const result = await cpu.run({ budget: 254n });
              const readReady = routes.indexOf('paged_loop_read_ready');
              const firstWrite = routes.indexOf('paged_loop_write_dword');
              const writeReady = routes.lastIndexOf('paged_loop_write_ready');
              check(
                readReady >= 0 && routes.indexOf('paged_loop_read_dword') > readReady,
                'the cold coefficient retries through the checked ADD, then uses retained reads',
              );
              check(
                firstWrite >= 0 &&
                  firstWrite < writeReady &&
                  routes.lastIndexOf('paged_loop_write_dword') > writeReady,
                'guarded stores succeed on both sides of the checked page transition',
              );
              equal(result.reason, 'budget', 'the last lap stops before its store');
              equal(result.retired, 254n, 'one zero lap, 24 writing laps, and the final prefix');
              equal(result.state.rip, head + 0x12n, 'the last store remains unstarted');
              equal(result.state.registers[8], 78n, 'the final dword addition has retired');
              same(
                [...cpu.readPhysicalMemory(0xcfd0n, 100)],
                [...little([...Array.from({ length: 24 }, (_, i) => BigInt(6 + 3 * i)), 0n], 4)],
                'both backedges preserve the exact stores and untouched successor',
              );
            },
          );
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged Long64 adjacent dword additions preserve the first ADD on a second-page fault',
      async run() {
        const head = 0x400000n;
        const data = 0x401000n;
        // MOVZX EAX,[RDI]; ADD R8D,[RSI]; ADD R9D,[RSI+4]; LEA RDI,[RDI+1]; DEC ECX;
        // JZ done; TEST EAX,EAX; JZ head; MOV [RDX],R8D; LEA RDX,[RDX+4]; JMP head; done: NOP.
        const code = '0fb60744030644034e04488d7f01ffc9740d85c074ea448902488d5204ebe190';
        const bytes = new Uint8Array(4096);
        bytes.set(little([7n, 13n], 4), 0x100);
        bytes.set(little([7n], 4), 0xffc);
        const spans: boolean[] = [];
        const restore = observeArtifactImports((env) => {
          const span = env.paged_loop_read_span;
          if (typeof span === 'function') {
            env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
              const pointer = span(address, width, segment);
              if (width === 8) spans.push(pointer !== 0);
              return pointer;
            };
          }
        });
        try {
          for (const split of [false, true]) {
            await usingPaged(
              compiler,
              {
                code,
                words: [[0x4008n, 0xa007n]],
                spans: [{ physical: 0xa000n, bytes }],
              },
              async (cpu) => {
                const address = data + (split ? 0xffcn : 0x100n);
                const start = initial(
                  head,
                  { 1: 1n, 2: data, 6: address, 7: data, 8: 0xffff_ffffn, 9: 0xffff_fffen },
                  2n,
                );
                cpu.setState(start);
                const prime = await cpu.run({ budget: 2n });
                equal(prime.retired, 2n, 'warm the first load through the checked path');
                cpu.setState(start);
                const result = await cpu.run({ stop: head + 0x1fn, budget: 21n });
                equal(result.state.registers[8], 6n, 'the first ADD commits in both cases');
                if (split) {
                  equal(result.reason, 'page-fault', 'the unmapped next page faults');
                  equal(result.state.rip, head + 6n, 'RIP points to the faulting ADD');
                  equal(result.retired, 2n, 'only the prefix retires');
                  equal(
                    result.state.registers[9],
                    0xffff_fffen,
                    'the second destination is intact',
                  );
                  equal(result.state.flags & 0x841n, 1n, 'the first ADD leaves carry set');
                  equal(cpu.readPagingState().cr2, address + 4n, 'CR2 names the second operand');
                } else {
                  equal(result.reason, 'stop', 'the same-leaf pair completes');
                  equal(result.retired, 6n, 'both ADDs retire independently');
                  equal(result.state.registers[9], 11n, 'the second ADD reads the next dword');
                }
              },
            );
          }
          same(spans, [true, false], 'the guard admits one leaf and refuses the crossing pair');
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged packed SSE re-enters after a checked split and preserves a later split fault',
      async run() {
        const head = 0x400000n;
        const source = 0x401ff8n;
        const denied = 0x403000n;
        const output = 0x404000n;
        // MOVUPS; CMPNLEPS; MOVMSKPS; TEST; LEA ESI,[ESI+16]; JNZ head; store; load; JMP head.
        const code = '0f100e0fc2c8060f50c185c08d761075ef890f8b13ebe9';
        const input = new Uint8Array(0x2000);
        input.fill(0xff, 0x1ff8);
        let splitReady = false;
        let retainedAfterSplit = 0;
        const refused: bigint[] = [];
        const restore = observeArtifactImports((env) => {
          const ready = env.paged_loop_split_read_ready;
          const read = env.paged_loop_block_host;
          const window = env.paged_loop_block_window;
          if (
            typeof ready === 'function' &&
            typeof read === 'function' &&
            typeof window === 'function'
          ) {
            env.paged_loop_split_read_ready = (address: bigint, width: number) => {
              const result = ready(address, width);
              if (address === source && width === 16 && result === 1) splitReady = true;
              return result;
            };
            env.paged_loop_block_window = (address: bigint) => {
              const result = window(address) as bigint;
              if (splitReady && result !== 0n) retainedAfterSplit++;
              return result;
            };
            env.paged_loop_block_host = (address: bigint) => {
              const result = read(address);
              if (splitReady && result === 0) refused.push(address);
              return result;
            };
          }
        });
        const replay = (mode: 'long64' | 'compatibility') =>
          usingPaged(
            compiler,
            {
              code,
              ...(mode === 'compatibility' ? { context: context(mode) } : {}),
              words: [
                [0x4008n, 0xa007n],
                [0x4010n, 0xb007n],
                [0x4020n, 0xc007n],
              ],
              spans: [
                { physical: 0xa000n, bytes: input },
                { physical: 0xc000n, bytes: little([0x1234_0000_0000n]) },
              ],
            },
            async (cpu) => {
              cpu.setState(
                initial(head, { 1: 0x12345678n, 3: output + 4n, 6: source, 7: output }, 2n),
              );
              const fault = await cpu.run({ budget: 2400n });
              check(
                splitReady && retainedAfterSplit > 100,
                mode + ' checked split permits windows, which must refill after the scalar arm',
              );
              same(refused, [denied - 8n], 'one later split returns to the checked operand');
              equal(fault.reason, 'page-fault', 'the second leaf is missing');
              equal(fault.retired, 2304n, '256 complete laps retire');
              equal(fault.state.rip, head, 'the original MOVUPS remains unretired');
              equal(fault.state.registers[6], denied - 8n, 'the faulting lap does not advance');
              equal(fault.state.flags, 0x46n, 'the previous TEST flags survive');
              equal(cpu.readXmmState()[1], 0n, 'the first half does not publish on a split fault');
              equal(cpu.readPagingState().cr2, denied, 'CR2 points to the denied half');
              equal(word(cpu, 0xc000n), 0x1234_1234_5678n, 'preceding stores and metadata survive');
              same(
                [word(cpu, 0x4008n), word(cpu, 0x4010n), word(cpu, 0x4018n), word(cpu, 0x4020n)],
                [0xa027n, 0xb027n, 0n, 0xc067n],
                'only successfully used leaves acquire accessed and dirty bits',
              );
            },
          );
        try {
          await replay('long64');
          splitReady = false;
          retainedAfterSplit = 0;
          refused.length = 0;
          await replay('compatibility');
        } finally {
          restore();
        }
      },
    },
  ];
}
