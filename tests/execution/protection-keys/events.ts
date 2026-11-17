import type { ExecutionContextSnapshot } from '../../../host/index.js';
import { gate } from '../software-events/fixture.js';
import {
  AC,
  AUX,
  DATA,
  PC,
  createCpu,
  fixture,
  hex,
  key,
  memory,
  pair,
  put,
  same,
  state,
  using,
  word,
  type Scenario,
} from './fixture.js';

export function eventScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'protection keys implicit user GDT under SMAP and AC1 combines ordinary and key denial independently',
      async run() {
        for (const ad of [false, true]) {
          const f = fixture(compiler, '8ed8');
          f.paging.cr4 |= 0x200000n;
          f.leaf(0x7000, key(3) | 0xb067n);
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: pair(3, ad ? 1 : 0), pkrs: 0 });
            const before = state(2n | AC);
            before.registers[0] = 0x10n;
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'page-fault', 'implicit SMAP ignores explicit AC bypass');
            same(
              result.pageFault,
              {
                vector: 14,
                address: 0x7010n,
                access: 'read',
                errorCode: ad ? 0x21 : 1,
                table: 'gdt',
                systemPhase: 'descriptor-read',
                selector: 0x10,
                instructionPc: PC,
              },
              'combined implicit SMAP literal packet',
            );
            same(result.state, before, 'implicit denial preserves segment input');
            memory(cpu, f.bytes, 'implicit denial no descriptor or stack publication');
          });
        }
      },
    },
    {
      name: 'protection keys implicit GDT LDT and TSS reads use supervisor provenance and the address-selected bank',
      async run() {
        for (const table of ['gdt', 'ldt', 'tss'] as const)
          for (const user of [false, true]) {
            const f = fixture(compiler, table === 'tss' ? 'ec' : '8ed8', {
              cpl: table === 'tss' ? 3 : 0,
            });
            f.leaf(0x7000, key(3) | 0xb063n | (user ? 4n : 0n));
            const context = f.options.context as ExecutionContextSnapshot;
            if (table === 'ldt')
              context.ldtr = {
                selector: 0x28,
                valid: true,
                base: 0x7000n,
                limit: 0x3f,
                attributes: 0x82,
              };
            if (table === 'tss')
              context.tr = {
                selector: 0x28,
                valid: true,
                base: 0x7000n,
                limit: 0x100,
                attributes: 0x8b,
              };
            let calls = 0;
            f.options.io = {
              read() {
                calls++;
                return 0x7b;
              },
              write() {
                calls++;
              },
            };
            await using(side, f, async (cpu) => {
              cpu.setProtectionKeysState(
                user ? { pkru: pair(3, 1), pkrs: 0 } : { pkru: 0, pkrs: pair(3, 1) },
              );
              const before = state(2n | AC);
              before.registers[0] = table === 'ldt' ? 0xcn : 0x10n;
              before.registers[2] = 0x80n;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              same(result.reason, 'page-fault', 'implicit metadata key refusal');
              same(result.pageFault?.table, table, 'implicit table owner');
              same(
                result.pageFault?.errorCode,
                0x21,
                'implicit requester remains supervisor even for CPL3',
              );
              same(
                result.pageFault?.address,
                table === 'tss' ? 0x7066n : table === 'gdt' ? 0x7010n : 0x7008n,
                'first metadata byte',
              );
              same(result.state, before, 'implicit read no cached segment/output');
              same(calls, 0, 'TSS refusal precedes provider');
              memory(cpu, f.bytes, 'metadata read publishes no payload');
            });
          }
      },
    },
    {
      name: 'protection keys implicit descriptor accessed-bit write observes WD with WP1 after its permitted read',
      async run() {
        const f = fixture(compiler, '8ed8');
        f.leaf(0x7000, key(3) | 0xb063n);
        put(f.bytes, 0xb010, 0x00cf92000000ffffn);
        await using(side, f, async (cpu) => {
          cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(3, 2) });
          const before = state();
          before.registers[0] = 0x10n;
          cpu.setState(before);
          const result = await cpu.run({ budget: 1n });
          same(result.reason, 'page-fault', 'descriptor candidate refuses accessed write');
          same(result.pageFault?.access, 'write', 'descriptor write rather than read');
          same(result.pageFault?.address, 0x7015n, 'accessed byte');
          same(result.pageFault?.errorCode, 0x23, 'descriptor supervisor WD fault');
          same(result.state, before, 'segment candidate not published');
          same(word(cpu, 0xb010), 0x00cf92000000ffffn, 'accessed remains clear');
        });
      },
    },
    {
      name: 'protection keys denied IDT metadata folds secondary PF into DF then bounded shutdown without frames',
      async run() {
        const f = fixture(compiler, 'cd40');
        // A valid long handler avoids masking a missing key check with an
        // unrelated destination-segment fault that would also select #DF.
        f.bytes.set(hex('ffff0000009baf00'), 0xb008);
        f.leaf(0x8000, key(3) | 0xc063n);
        for (const vector of [8, 14, 0x40])
          f.bytes.set(hex(gate(64, 8, 0x4000n, 0)), 0xc000 + vector * 16);
        await using(side, f, async (cpu) => {
          cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(3, 1) });
          const before = state(2n | AC);
          cpu.setState(before);
          const reported = await cpu.run({ budget: 1n });
          same(reported.reason, 'page-fault', 'IDT explicit source report');
          same(reported.pageFault?.errorCode, 0x21, 'IDT key PF');
          same(reported.pageFault?.table, 'idt', 'IDT provenance');
          same(reported.pageFault?.address, 0x8400n, 'long gate source');
          const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
          same(staged.reason, 'event-budget', 'primary historical source captured');
          const nested = await cpu.run({ budget: 1n, eventDelivery: { budget: 1n } });
          same(nested.reason, 'event-budget', 'secondary PF selects bounded DF');
          same(
            cpu.readPagingState().cr2,
            0x80e0n,
            'PF gate14 key denial publishes its exact address',
          );
          same(
            nested.state.event,
            {
              kind: 'pending',
              exception: {
                vector: 8,
                instructionPc: PC,
                returnPc: PC,
                savedFlags: 0x50002n,
                errorCode: 0,
              },
            },
            'DF source literal',
          );
          const shutdown = await cpu.run({ budget: 1n, eventDelivery: { budget: 1n } });
          same(shutdown.reason, 'shutdown', 'DF entry denial shuts down');
          same(shutdown.retired, 0n, 'no failed entry retirement');
          same(
            cpu.readPagingState().cr2,
            0x8080n,
            'DF gate8 key denial publishes its exact address',
          );
          memory(cpu, f.bytes, 'nested entries never publish frame');
        });
      },
    },
    {
      name: 'protection keys real PF handlers repair PKRU PKRS and IRET retries source with exact PK frame and retained bank',
      async run() {
        for (const user of [false, true])
          for (const hostRepairBeforeEntry of [false, true]) {
            const f = fixture(compiler, '8b03');
            f.leaf(Number(DATA), key(3) | 0xa063n | (user ? 4n : 0n));
            f.bytes.set(hex('ffff0000009baf00'), 0xb008);
            f.bytes.set(hex(gate(64, 8, 0x4000n, 0)), 0xc000 + 14 * 16);
            // Preserve all three input registers; clear selected bank, discard error,
            // return with IRETQ. There are twelve handler instructions.
            f.bytes.set(
              hex(
                user
                  ? '50515231c031d231c90f01ef5a59584883c40848cf'
                  : '50515231c031d2b9e10600000f305a59584883c40848cf',
              ),
              0x9000,
            );
            await using(side, f, async (cpu) => {
              const repaired = user ? { pkru: 0, pkrs: 0x12345678 } : { pkru: 0x12345678, pkrs: 0 };
              cpu.setProtectionKeysState(
                user
                  ? { pkru: pair(3, 1), pkrs: 0x12345678 }
                  : { pkru: 0x12345678, pkrs: pair(3, 1) },
              );
              const before = state();
              cpu.setState(before);
              cpu.setTimeState({ tsc: 100n, tscAux: AUX });
              const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
              const pending = {
                kind: 'pending',
                exception: {
                  vector: 14,
                  instructionPc: PC,
                  returnPc: PC,
                  savedFlags: 0x10002n,
                  errorCode: 0x21,
                  pageFaultAddress: DATA,
                },
              };
              same(staged.state.event, pending, 'literal PK historical pending source');
              if (hostRepairBeforeEntry) {
                cpu.setProtectionKeysState(repaired);
                same(cpu.readState().event, pending, 'bank repair preserves historical source');
              }
              const entered = await cpu.run({
                budget: 20n,
                stop: 0x4000n,
                eventDelivery: { budget: 1n },
              });
              same(entered.reason, 'stop', 'handler entry succeeds');
              same(entered.retired, 0n, 'automatic PF entry no retirement');
              same(word(cpu, 0xdfc0), 0x21n, 'frame exact PK error word');
              same(word(cpu, 0xdfc8), PC, 'frame retry PC');
              same(word(cpu, 0xdfd8), 0x10002n, 'frame RF fault flags');
              const result = await cpu.run({
                budget: 20n,
                stop: PC + 2n,
                eventDelivery: { budget: 1n },
              });
              same(result.reason, 'stop', 'repaired source successor reached');
              same(result.retired, 13n, 'twelve handler instructions and one retried load');
              same(result.deliveryAttempts, 0n, 'source no repeated fault');
              const expected = structuredClone(before);
              expected.rip += 2n;
              expected.registers[0] = 0x78563412n;
              same(
                result.state,
                expected,
                'IRET restores scalar boundary then one successful load',
              );
              same(
                cpu.readProtectionKeysState(),
                repaired,
                'entry and IRET do not restore obsolete key bank',
              );
              same(cpu.readPagingState().cr2, DATA, 'repair retains CR2');
              same(
                cpu.readTimeState(),
                { tsc: 113n, tscAux: AUX },
                'handler exact retirement ledger',
              );
            });
          }
      },
    },
    {
      name: 'protection keys prospective interrupt frame follows supervisor bank and commits nothing on denied stack',
      async run() {
        for (const split of [false, true]) {
          const f = fixture(compiler, 'cd40');
          f.bytes.set(hex('ffff0000009baf00'), 0xb008);
          f.bytes.set(hex(gate(64, 8, 0x4000n, 0)), 0xc000 + 0x40 * 16);
          f.leaf(0x9000, key(3) | 0xd063n);
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(3, 1) });
            const before = state();
            // The first two slots atA008/A000 are admitted, then FLAGS at9ff8
            // is denied. All slots still belong to one compound frame barrier.
            if (split) before.registers[4] = 0xa010n;
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'page-fault', 'prospective frame key denial');
            same(result.pageFault?.errorCode, 0x23, 'prospective supervisor write PK');
            if (split)
              same(result.pageFault?.address, 0x9ff8n, 'later compound slot is the first refusal');
            same(result.state, before, 'no prospective SP or context publication');
            memory(cpu, f.bytes, 'no partial interrupt frame');
          });
        }
      },
    },
  ];
}
