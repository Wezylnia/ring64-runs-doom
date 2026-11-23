import {
  DATA,
  PC,
  PKE,
  PKS,
  createCpu,
  fixture,
  key,
  pageFault,
  pair,
  put,
  same,
  state,
  using,
  type Cpu,
  type Scenario,
} from './fixture.js';

// Read, guest 64-bit PTE edit, INVLPG, MOV CR4, WRPKRU, WRMSR, MOV CR3,
// INVPCID, store. Each literal offset below is an independent entry boundary.
const CODE = '8b034889110f013b0f22e00f01ef0f300f22d8660f3882018903';

async function at(cpu: Cpu, offset: number, registers: Record<number, bigint> = {}) {
  const before = state(2n, PC + BigInt(offset));
  for (const [index, value] of Object.entries(registers)) before.registers[Number(index)] = value;
  cpu.setState(before);
  return cpu.run({ budget: 1n });
}

export function cacheScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'protection keys retained key survives guest PTE edit host bank imports and control toggles until INVLPG',
      async run() {
        for (const user of [false, true])
          for (const mutation of [
            'bank',
            'guest-bank',
            'same-control',
            'toggle-control',
          ] as const) {
            const f = fixture(compiler, CODE);
            const leaf = 0xa063n | (user ? 4n : 0n);
            f.leaf(Number(DATA), key(3) | leaf);
            f.leaf(0x5000, BigInt(f.pt) | 0x63n);
            await using(side, f, async (cpu) => {
              same((await at(cpu, 0)).reason, 'budget', 'warm key3 translation');
              same(
                (await at(cpu, 2, { 1: 0x5030n, 2: key(7) | leaf })).reason,
                'budget',
                'guest edits key7',
              );
              const controls = cpu.readPagingState().cr4;
              if (mutation === 'same-control')
                same((await at(cpu, 8, { 0: controls })).reason, 'budget', 'same CR4 legal');
              if (mutation === 'toggle-control') {
                same(
                  (await at(cpu, 8, { 0: controls & ~(user ? PKE : PKS) })).reason,
                  'budget',
                  'disable selected keys',
                );
                same(
                  (await at(cpu, 8, { 0: controls })).reason,
                  'budget',
                  'reenable selected keys',
                );
              }
              cpu.setProtectionKeysState(
                user ? { pkru: pair(7, 1), pkrs: 0 } : { pkru: 0, pkrs: pair(7, 1) },
              );
              if (mutation === 'guest-bank') {
                cpu.setProtectionKeysState({ pkru: 0, pkrs: 0 });
                same(
                  (
                    await at(cpu, user ? 11 : 14, {
                      0: BigInt(pair(7, 1)),
                      1: user ? 0n : 0x6e1n,
                      2: 0n,
                    })
                  ).reason,
                  'budget',
                  'guest bank writer retains old key fact',
                );
              }
              const stale = await at(cpu, 0);
              same(stale.reason, 'budget', 'deny new key7 still authorizes retained key3');
              same(stale.resources.compilations, 0, 'bank/control preserve compiled owner');
              same((await at(cpu, 5)).reason, 'budget', 'INVLPG admitted');
              pageFault(await at(cpu, 0), DATA, 'read', 0x21);
              cpu.setProtectionKeysState({ pkru: 0, pkrs: 0 });
              same((await at(cpu, 0)).reason, 'budget', 'repair existing fault and fill key7');
              cpu.setProtectionKeysState(
                user ? { pkru: pair(7, 1), pkrs: 0 } : { pkru: 0, pkrs: pair(7, 1) },
              );
              pageFault(await at(cpu, 0), DATA, 'read', 0x21);
            });
          }
      },
    },
    {
      name: 'protection keys captured D1 write keeps old key while D0 promotion legitimately refreshes changed key',
      async run() {
        for (const dirty of [false, true]) {
          const f = fixture(compiler, CODE);
          const leaf = dirty ? 0xa063n : 0xa023n;
          f.leaf(Number(DATA), key(3) | leaf);
          f.leaf(0x5000, BigInt(f.pt) | 0x63n);
          await using(side, f, async (cpu) => {
            same((await at(cpu, 0)).reason, 'budget', 'warm read captures D state');
            same(
              (await at(cpu, 2, { 1: 0x5030n, 2: key(7) | leaf })).reason,
              'budget',
              'guest edits leaf key',
            );
            cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(7, 1) });
            const write = await at(cpu, 24, { 0: 0x44332211n });
            same(write.reason, dirty ? 'budget' : 'page-fault', 'D residency selected path');
            if (!dirty) pageFault(write, DATA, 'write', 0x23, PC + 24n);
            same(
              cpu.readPhysicalMemory(0xa000n, 4),
              dirty
                ? new Uint8Array([0x11, 0x22, 0x33, 0x44])
                : new Uint8Array([0x12, 0x34, 0x56, 0x78]),
              'D refresh payload boundary',
            );
          });
        }
      },
    },
    {
      name: 'protection keys same-unit and cached-middle WRPKRU changes the immediate following data operation',
      async run() {
        for (const middle of [false, true]) {
          const f = fixture(compiler, (middle ? '90' : '') + '0f01ef8b03');
          f.leaf(Number(DATA), key(3) | 0xa067n);
          await using(side, f, async (cpu) => {
            const before = state();
            before.registers[0] = BigInt(pair(3, 1));
            before.registers[1] = 0n;
            before.registers[2] = 0n;
            cpu.setState(before);
            if (middle) {
              const warm = await cpu.run({ budget: 1n });
              same(warm.reason, 'budget', 'NOP warms middle entry');
              same(warm.state.rip, PC + 1n, 'middle exact boundary');
            }
            const result = await cpu.run({ budget: 2n });
            pageFault(result, DATA, 'read', 0x21, PC + (middle ? 4n : 3n));
            same(result.retired, 1n, 'bank writer alone retires before load refusal');
            same(
              cpu.readProtectionKeysState(),
              { pkru: pair(3, 1), pkrs: 0 },
              'writer visible immediately',
            );
            same(
              result.state.registers,
              before.registers,
              'WR preserves all source GPRs and denied load destination',
            );
          });
        }
      },
    },
    {
      name: 'protection keys PKRS WRMSR changes live resident access without an enable or owner fence',
      async run() {
        const f = fixture(compiler, CODE);
        f.leaf(Number(DATA), key(3) | 0xa063n);
        await using(side, f, async (cpu) => {
          same((await at(cpu, 0)).reason, 'budget', 'resident supervisor key3');
          same(
            (await at(cpu, 14, { 0: BigInt(pair(3, 1)), 1: 0x6e1n, 2: 0n })).reason,
            'budget',
            'guest PKRS write',
          );
          const result = await at(cpu, 0);
          pageFault(result, DATA, 'read', 0x21);
          same(result.resources.compilations, 0, 'PKRS did not prune source owner');
          same(
            cpu.readProtectionKeysState(),
            { pkru: 0, pkrs: pair(3, 1) },
            'PKRS resident policy',
          );
        });
      },
    },
    {
      name: 'protection keys INVPCID types and global scopes retain or expose a key exactly with the existing mapping',
      async run() {
        for (const global of [false, true])
          for (const kind of [0n, 1n, 2n, 3n]) {
            const f = fixture(compiler, CODE);
            f.paging.cr3 |= 1n;
            f.paging.cr4 |= 0x20080n;
            const leaf = 0xa063n | (global ? 0x100n : 0n);
            f.leaf(Number(DATA), key(3) | leaf);
            f.leaf(0x5000, BigInt(f.pt) | 0x63n);
            put(f.bytes, 0xb000, kind < 2n ? 1n : 0n);
            put(f.bytes, 0xb008, DATA);
            await using(side, f, async (cpu) => {
              same((await at(cpu, 0)).reason, 'budget', 'PCID1 warms key3');
              same(
                (await at(cpu, 2, { 1: 0x5030n, 2: key(7) | leaf })).reason,
                'budget',
                'PCID1 changes backing key7',
              );
              same(
                (await at(cpu, 16, { 0: 0x8000000000001002n })).reason,
                'budget',
                'PCID2 without flush',
              );
              same((await at(cpu, 0)).reason, 'budget', 'PCID2 resident or global data');
              same(
                (await at(cpu, 16, { 0: 0x8000000000001001n })).reason,
                'budget',
                'PCID1 retained switch',
              );
              cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(7, 1) });
              same((await at(cpu, 0)).reason, 'budget', 'no-flush return keeps key3');
              same((await at(cpu, 19, { 0: kind, 1: 0x7000n })).reason, 'budget', 'scoped INVPCID');
              const result = await at(cpu, 0);
              const survives = global && kind !== 2n;
              same(
                result.reason,
                survives ? 'budget' : 'page-fault',
                'global and selected PCID scope',
              );
              if (!survives) pageFault(result, DATA, 'read', 0x21);
            });
          }
      },
    },
  ];
}
