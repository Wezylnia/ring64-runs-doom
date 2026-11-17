import {
  AC,
  DATA,
  PC,
  PKE,
  PKS,
  createCpu,
  fixture,
  key,
  memory,
  pageFault,
  pair,
  put,
  same,
  state,
  using,
  type Scenario,
} from './fixture.js';

export function boundaryScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'protection keys completed getter WRPKRU publication survives rejected outer bank capture',
      async run() {
        const f = fixture(compiler, '0f01ef');
        await using(side, f, async (cpu) => {
          const before = state();
          before.registers[0] = 0x40n;
          before.registers[1] = 0n;
          before.registers[2] = 0n;
          cpu.setState(before);
          same((await cpu.run({ budget: 1n })).reason, 'budget', 'warm actual WRPKRU entry');
          cpu.setProtectionKeysState({ pkru: 0, pkrs: 0 });
          cpu.setState(before);
          let nested: Promise<unknown> | undefined;
          let rejected = false;
          try {
            cpu.setProtectionKeysState({
              get pkru() {
                nested = cpu.run({ budget: 1n });
                return 0;
              },
              pkrs: 0,
            });
          } catch {
            rejected = true;
          }
          same(rejected, true, 'outer capture rejects completed nested run');
          await nested;
          same(
            cpu.readProtectionKeysState(),
            { pkru: 0x40, pkrs: 0 },
            'nested guest key write is never overwritten',
          );
          same(cpu.readState().rip, PC + 3n, 'nested instruction completes exactly once');
        });
      },
    },
    {
      name: 'protection keys literal remaining fetch readonly MSR privilege and control frontiers',
      async run() {
        for (const [cpl, user] of [
          [0, true],
          [3, true],
          [3, false],
        ] as const) {
          const f = fixture(compiler, '90', { cpl });
          f.paging.efer |= 0x800n;
          f.leaf(Number(PC), key(15) | 0x8063n | (user ? 4n : 0n));
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 0xffffffff, pkrs: 0xffffffff });
            cpu.setState(state());
            const result = await cpu.run({ budget: 1n });
            if (user) same(result.reason, 'budget', 'user-address fetch ignores AD');
            else pageFault(result, PC, 'execute', 0x15);
          });
        }
        const readOnly = fixture(compiler, '8b03', { cpl: 3 });
        readOnly.leaf(Number(DATA), key(3) | 0xa065n);
        await using(side, readOnly, async (cpu) => {
          cpu.setProtectionKeysState({ pkru: pair(3, 2), pkrs: 0 });
          cpu.setState(state());
          const result = await cpu.run({ budget: 1n });
          same(result.reason, 'budget', 'WD and ordinary RW0 do not deny a read');
          same(result.state.registers[0], 0x78563412n, 'read-only WD payload');
        });
        for (const code of ['0f32', '0f30']) {
          const f = fixture(compiler, code, { cpl: 3 });
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 0x11223344, pkrs: 0x55667788 });
            const before = state();
            before.registers[1] = 0x6e1n;
            before.registers[2] = 1n;
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'protection-fault', 'MSR CPL3 denied');
            same(
              result.protectionFault,
              { vector: 13, errorCode: 0, phase: 'privilege', instructionPc: PC },
              'privilege before PKRS reserved upper word',
            );
            same(result.state, before, 'privileged MSR no register publication');
            same(
              cpu.readProtectionKeysState(),
              { pkru: 0x11223344, pkrs: 0x55667788 },
              'privileged MSR no bank publication',
            );
          });
        }
        const identity = fixture(compiler, '0fa2');
        identity.paging.cr4 &= ~PKE;
        await using(side, identity, async (cpu) => {
          const before = state();
          before.registers[0] = 7n;
          before.registers[1] = 0n;
          cpu.setState(before);
          const result = await cpu.run({ budget: 1n });
          same(result.state.registers[1], 0x80000008n, 'PKS alone never sets OSPKE');
        });
        for (const reserved of [false, true]) {
          const f = fixture(compiler, '0f22e0');
          f.paging.cr0 &= ~0x10000n;
          f.paging.cr4 &= ~(PKE | PKS);
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 17, pkrs: 34 });
            const before = state();
            before.registers[0] = f.paging.cr4 | (reserved ? PKE | 0x8000n : PKS);
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(
              result.reason,
              reserved ? 'protection-fault' : 'budget',
              'reserved PKE versus legal PKS WP0',
            );
            same(result.retired, reserved ? 0n : 1n, 'control own retirement');
            same(
              cpu.readPagingState(),
              reserved ? f.paging : { ...f.paging, cr4: before.registers[0] },
              'invalid control atomicity',
            );
            same(
              cpu.readProtectionKeysState(),
              { pkru: 17, pkrs: 34 },
              'control write preserves key words',
            );
          });
        }
      },
    },
    {
      name: 'protection keys ordinary SMAP and RW denial independently retain or combine literal PK fault bits',
      async run() {
        for (const ac of [false, true])
          for (const denied of [false, true]) {
            const f = fixture(compiler, '8b03');
            f.paging.cr4 |= 0x200000n;
            f.leaf(Number(DATA), key(3) | 0xa067n);
            await using(side, f, async (cpu) => {
              cpu.setProtectionKeysState({ pkru: pair(3, denied ? 1 : 0), pkrs: 0 });
              cpu.setState(state(2n | (ac ? AC : 0n)));
              const result = await cpu.run({ budget: 1n });
              if (ac && !denied) same(result.reason, 'budget', 'SMAP AC bypass alone');
              else pageFault(result, DATA, 'read', denied ? 0x21 : 1);
            });
          }
        for (const denied of [false, true]) {
          const f = fixture(compiler, '8903', { cpl: 3 });
          f.leaf(Number(DATA), key(3) | 0xa065n); // present user read-only.
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: pair(3, denied ? 2 : 0), pkrs: 0 });
            cpu.setState(state());
            pageFault(await cpu.run({ budget: 1n }), DATA, 'write', denied ? 0x27 : 7);
          });
        }
      },
    },
    {
      name: 'protection keys PKE PKS-only writes preserve captured PAE quartet without reading edited physical root',
      async run() {
        for (const change of [0n, PKE, PKS, PKE | PKS]) {
          const f = fixture(compiler, '0f22e0', { family: 'pae' });
          f.paging.cr4 &= ~(PKE | PKS);
          put(f.bytes, 0x1008, 0x5001n);
          await using(side, f, async (cpu) => {
            const before = state();
            before.registers[0] = f.paging.cr4 | change;
            cpu.setState(before);
            same((await cpu.run({ budget: 1n })).reason, 'budget', 'dormant control write legal');
            same(cpu.readPagingState().pdpte, [0x2001n, 0n, 0n, 0n], 'no PAE quartet reload');
            memory(cpu, f.bytes, 'control does not mutate physical quartet');
          });
        }
      },
    },
    {
      name: 'protection keys denied data and INVPCID descriptors discard watchpoint candidates and do not leak hits',
      async run() {
        for (const code of ['8b03', '8903', '660f388203']) {
          const f = fixture(compiler, code + '90');
          f.leaf(Number(DATA), key(3) | 0xa063n);
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(3, 1) });
            const before = state();
            before.registers[0] = 2n;
            cpu.setState(before);
            cpu.setDebugState({ addresses: [DATA, 0n, 0n, 0n], dr6: 0n, dr7: 0xf0401n });
            const debug = cpu.readDebugState();
            const result = await cpu.run({ budget: 1n });
            pageFault(
              result,
              DATA,
              code === '8903' ? 'write' : 'read',
              code === '8903' ? 0x23 : 0x21,
            );
            same(cpu.readDebugState(), debug, 'failed operand has no DR6 data hit');
            cpu.setState({ ...result.state, rip: PC + BigInt(code.length / 2) });
            same(
              (await cpu.run({ budget: 1n })).reason,
              'budget',
              'faulted hit cannot leak to next NOP',
            );
          });
        }
      },
    },
    {
      name: 'protection keys stop budget and execute breakpoint precede an unreached WRPKRU and unavailable following byte',
      async run() {
        for (const gate of ['stop', 'budget', 'debug'] as const) {
          const f = fixture(compiler, '0f01ef');
          await using(side, f, async (cpu) => {
            const before = state();
            before.registers[0] = 0xffffffffn;
            before.registers[1] = 0n;
            before.registers[2] = 0n;
            cpu.setState(before);
            if (gate === 'debug')
              cpu.setDebugState({ addresses: [PC, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
            const result = await cpu.run({
              budget: gate === 'budget' ? 0n : 1n,
              ...(gate === 'stop' ? { stop: PC } : {}),
            });
            same(
              result.reason,
              gate === 'debug' ? 'debug-exception' : gate,
              'pre-instruction gate',
            );
            same(result.retired, 0n, 'unreached writer no retirement');
            same(
              cpu.readProtectionKeysState(),
              { pkru: 0, pkrs: 0 },
              'unreached writer no bank publication',
            );
          });
        }
      },
    },
  ];
}
