import {
  AC,
  DATA,
  PC,
  PKE,
  PKS,
  WP,
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
  word,
  type Scenario,
} from './fixture.js';

export function rightsScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    ...(['ia32e', 'fixed'] as const).map((family) => ({
      name: `protection keys ${family} all sixteen keys and four pairs select address bank with literal PFEC`,
      async run() {
        for (let selected = 0; selected < 16; selected++)
          for (const rights of [0, 1, 2, 3])
            for (const userAddress of [false, true])
              for (const write of [false, true]) {
                const f = fixture(compiler, write ? '8903' : '8b03', { family, cpl: 3 });
                f.leaf(Number(DATA), key(selected) | 0xa063n | (userAddress ? 4n : 0n));
                await using(side, f, async (cpu) => {
                  const bank = pair(selected, rights);
                  // Poison the other bank: only aggregate address U/S selects it.
                  cpu.setProtectionKeysState(
                    userAddress
                      ? { pkru: bank, pkrs: 0xffffffff }
                      : { pkru: 0xffffffff, pkrs: bank },
                  );
                  const before = state();
                  cpu.setState(before);
                  const result = await cpu.run({ budget: 1n });
                  const keyDenied = rights === 1 || rights === 3 || (rights === 2 && write);
                  const allowed = userAddress && !keyDenied;
                  same(
                    result.reason,
                    allowed ? 'budget' : 'page-fault',
                    'literal matrix admission',
                  );
                  same(result.retired, allowed ? 1n : 0n, 'literal matrix retirement');
                  const expected = structuredClone(before);
                  if (allowed) {
                    expected.rip = PC + 2n;
                    if (!write) expected.registers[0] = 0x78563412n;
                  }
                  same(result.state, expected, 'literal matrix scalar publication');
                  if (!allowed)
                    pageFault(
                      result,
                      DATA,
                      write ? 'write' : 'read',
                      (write ? 7 : 5) | (keyDenied ? 0x20 : 0),
                    );
                  same(
                    word(cpu, f.pt + 6 * 8),
                    key(selected) | 0xa063n | (userAddress ? 4n : 0n),
                    'pre-set A/D leaf unchanged',
                  );
                  if (write)
                    same(
                      word(cpu, 0xa000, 4),
                      allowed ? 0xcafebaben : 0x78563412n,
                      'matrix payload',
                    );
                });
              }
      },
    })),
    {
      name: 'protection keys supervisor CPL0 CPL1 CPL2 WD follows WP while AD and AC never bypass',
      async run() {
        for (const cpl of [0, 1, 2])
          for (const wp of [false, true])
            for (const ac of [false, true])
              for (const userAddress of [false, true])
                for (const rights of [1, 2, 3]) {
                  const f = fixture(compiler, '8903', { cpl });
                  if (!wp) f.paging.cr0 &= ~WP;
                  f.leaf(Number(DATA), key(15) | 0xa063n | (userAddress ? 4n : 0n));
                  await using(side, f, async (cpu) => {
                    cpu.setProtectionKeysState(
                      userAddress
                        ? { pkru: pair(15, rights), pkrs: 0 }
                        : { pkru: 0, pkrs: pair(15, rights) },
                    );
                    const before = state(2n | (ac ? AC : 0n));
                    cpu.setState(before);
                    const result = await cpu.run({ budget: 1n });
                    const allowed = rights === 2 && !wp;
                    same(result.reason, allowed ? 'budget' : 'page-fault', 'supervisor WD policy');
                    same(result.retired, allowed ? 1n : 0n, 'supervisor key retirement');
                    if (!allowed) {
                      pageFault(result, DATA, 'write', 0x23);
                      same(result.state, before, 'supervisor denied scalar');
                      memory(cpu, f.bytes, 'supervisor denied entire image');
                    }
                  });
                }
      },
    },
    {
      name: 'protection keys aggregate upper U selects PKRS and nonleaf key bits do not select a key',
      async run() {
        for (const aggregateUser of [false, true]) {
          const f = fixture(compiler, '8b03');
          // Separate data subtree leaves source identity and privilege unchanged.
          f.leaf(Number(DATA), key(7) | 0xa067n);
          put(f.bytes, 0x3000, key(15) | (aggregateUser ? 0x4027n : 0x4023n));
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: pair(15, 3), pkrs: pair(7, 1) });
            cpu.setState(state());
            const result = await cpu.run({ budget: 1n });
            same(
              result.reason,
              aggregateUser ? 'budget' : 'page-fault',
              'aggregate U address bank',
            );
            if (!aggregateUser) pageFault(result, DATA, 'read', 0x21);
          });
        }
      },
    },
    {
      name: 'protection keys fetch ignores both deny-all banks while NX nonpresent reserved keep PK clear',
      async run() {
        for (const [leaf, fault] of [
          [key(15) | 0x8063n, undefined],
          [key(15) | 0x8000000000008063n, 0x11],
          [key(15) | 0x8062n, 0x10],
          [key(15) | 0x8063n, 0x19],
        ] as const) {
          const f = fixture(compiler, '90');
          f.paging.efer |= 0x800n;
          f.leaf(Number(PC), leaf);
          // PML4.PS is reserved. A 4K PTE has ignored bits58:52, so those
          // cannot serve as a reserved-bit witness in this 52-bit profile.
          if (fault === 0x19) put(f.bytes, 0x1000, 0x20a7n);
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 0xffffffff, pkrs: 0xffffffff });
            cpu.setState(state());
            const result = await cpu.run({ budget: 1n });
            if (fault === undefined) same(result.reason, 'budget', 'keys never gate fetch');
            else pageFault(result, PC, 'execute', fault);
          });
        }
      },
    },
    {
      name: 'protection keys disabled controls and legacy families leave nonzero banks dormant',
      async run() {
        for (const family of ['non-pae', 'pae', 'ia32e', 'fixed'] as const) {
          const f = fixture(compiler, '8b03', { family });
          if (family === 'ia32e' || family === 'fixed') f.paging.cr4 &= ~(PKE | PKS);
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 0xffffffff, pkrs: 0xffffffff });
            cpu.setState(state());
            same((await cpu.run({ budget: 1n })).reason, 'budget', 'dormant key permissions');
            same(
              cpu.readProtectionKeysState(),
              { pkru: 0xffffffff, pkrs: 0xffffffff },
              'dormant words persist',
            );
          });
        }
      },
    },
    {
      name: 'protection keys terminating 2MiB and 1GiB leaves use key15 and allow physical pagewalk A writes',
      async run() {
        for (const size of [21, 30])
          for (const user of [false, true]) {
            const f = fixture(compiler, '8b03');
            f.paging.cr3 = 0x10000n;
            put(f.bytes, 0x10000, key(7) | 0x11027n);
            put(
              f.bytes,
              0x11000,
              size === 30 ? key(15) | 0xe3n | (user ? 4n : 0n) : key(7) | 0x12027n,
            );
            if (size === 21) put(f.bytes, 0x12000, key(15) | 0xe3n | (user ? 4n : 0n));
            f.bytes.set([0x8b, 0x03], Number(PC));
            await using(side, f, async (cpu) => {
              cpu.setProtectionKeysState(
                user ? { pkru: pair(15, 1), pkrs: 0 } : { pkru: 0, pkrs: pair(15, 1) },
              );
              cpu.setState(state());
              pageFault(await cpu.run({ budget: 1n }), DATA, 'read', 0x21);
              // Host physical administration is independent of denied linear data.
              cpu.writePhysicalMemory(0x6000n, new Uint8Array([0x42]));
              same(cpu.readPhysicalMemory(0x6000n, 1)[0], 0x42, 'physical bypass');
            });
          }
      },
    },
  ];
}
