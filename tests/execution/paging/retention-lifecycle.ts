import { createCpu, type Cpu, type PagingState } from '../../../host/index.js';
import { check, equal, hex, initial, throws, type Scenario } from '../../support/fixture.js';
import {
  context,
  families,
  fixture,
  put,
  using,
  type Family,
  type Fixture,
} from '../supervisor-access/fixture.js';

const PC = 0x1000n;
const DATA = 0x6000n;
const A = 0x11223344n;
const B = 0x88776655n;
const C = 0xccbbaa99n;
const PGE = 0x80n;
const PCIDE = 0x20000n;
const NO_FLUSH = 0x8000000000000000n;
const ROOT_B = 0x11000n;
// Read, edit a PTE, MOV CR3, MOV CR4, read CR3, MOV CR0, INVLPG, NOP.
const CODE = '8b0389110f22d80f22e00f20d80f22c00f013b90';

function setup(compiler: WebAssembly.Module, family: Family): Fixture {
  const input = fixture({ createCpu, compiler }, family, CODE);
  input.leaf(Number(PC), 0x8067n);
  input.leaf(Number(DATA), 0xa067n);
  input.leaf(0x5000, BigInt(input.pt) | 0x63n);
  input.bytes.set(hex('44332211'), 0xa000);
  input.bytes.set(hex('55667788'), 0xe000);
  input.bytes.set(hex('99aabbcc'), 0xf000);
  return input;
}

async function readData(cpu: Cpu, expected: bigint, label: string, flags = 2n): Promise<void> {
  cpu.setState(initial(PC, { 3: DATA }, flags));
  const result = await cpu.run({ budget: 1n });
  equal(result.reason, 'budget', `${label} completes`);
  equal(result.retired, 1n, `${label} retirement`);
  equal(result.state.registers[0]! & 0xffffffffn, expected, label);
}

async function editLeaf(cpu: Cpu, input: Fixture, value = 0xe067n, alias = 0x5000n) {
  cpu.setState(initial(PC + 2n, { 1: alias + BigInt(6 * input.width), 2: value }, 2n));
  const result = await cpu.run({ budget: 1n });
  equal(result.reason, 'budget', 'guest PTE store completes');
  equal(result.retired, 1n, 'guest PTE store retires once');
}

async function warmRetarget(cpu: Cpu, input: Fixture): Promise<void> {
  await readData(cpu, A, 'initial mapping A');
  await editLeaf(cpu, input);
  await readData(cpu, A, 'guest PTE store retains A');
}

async function control(cpu: Cpu, offset: number, value: bigint, reason = 'budget') {
  cpu.setState(initial(PC + BigInt(offset), { 0: value, 3: DATA }, 2n));
  const result = await cpu.run({ budget: 1n });
  equal(result.reason, reason, `control at +${offset} selected outcome`);
  equal(result.retired, reason === 'budget' ? 1n : 0n, 'control retirement boundary');
  return result;
}

function secondRoot(input: Fixture): number {
  input.bytes.fill(0, 0x11000, 0x15000);
  let pt: number;
  if (input.family === 'non-pae') {
    put(input.bytes, 0x11000, 0x12027n, 4);
    pt = 0x12000;
  } else if (input.family === 'pae') {
    put(input.bytes, 0x11000, 0x12001n);
    put(input.bytes, 0x12000, 0x13027n);
    pt = 0x13000;
  } else {
    put(input.bytes, 0x11000, 0x12027n);
    put(input.bytes, 0x12000, 0x13027n);
    put(input.bytes, 0x13000, 0x14027n);
    pt = 0x14000;
  }
  put(input.bytes, pt + input.width, 0x8067n, input.width);
  put(input.bytes, pt + 5 * input.width, BigInt(input.pt) | 0x63n, input.width);
  put(input.bytes, pt + 6 * input.width, 0xe067n, input.width);
  put(input.bytes, pt + 8 * input.width, BigInt(pt) | 0x63n, input.width);
  input.leaf(0x8000, BigInt(pt) | 0x63n);
  return pt;
}

type ImportPacket = {
  name: string;
  change(cpu: Cpu, input: Fixture): void | Promise<void>;
};

const retainedImports: ImportPacket[] = [
  { name: 'equal paging bank', change: (cpu) => cpu.setPagingState(cpu.readPagingState()) },
  {
    name: 'CR2 only',
    change: (cpu) => cpu.setPagingState({ ...cpu.readPagingState(), cr2: 0x123456789abcdef0n }),
  },
  {
    name: 'AM only',
    change: (cpu) => {
      const bank = cpu.readPagingState();
      cpu.setPagingState({ ...bank, cr0: bank.cr0 ^ 0x40000n });
    },
  },
  {
    name: 'CR0 numeric only',
    change: (cpu) => {
      const bank = cpu.readPagingState();
      cpu.setPagingState({ ...bank, cr0: bank.cr0 ^ 8n });
    },
  },
  {
    name: 'CR4 numeric only',
    change: (cpu) => {
      const bank = cpu.readPagingState();
      cpu.setPagingState({ ...bank, cr4: bank.cr4 ^ 0x200n });
    },
  },
  {
    name: 'TSD only',
    change: (cpu) => {
      const bank = cpu.readPagingState();
      cpu.setPagingState({ ...bank, cr4: bank.cr4 ^ 4n });
    },
  },
  {
    name: 'SCE only',
    change: (cpu) => {
      const bank = cpu.readPagingState();
      cpu.setPagingState({ ...bank, efer: bank.efer ^ 1n });
    },
  },
  {
    name: 'scalar and raw AC',
    change: (cpu) => cpu.setState(initial(PC, { 10: 0x1234n }, 0x40002n)),
  },
  { name: 'equal context', change: (cpu) => cpu.setContext(cpu.readContext()) },
  { name: 'changed CPL', change: (cpu, input) => cpu.setContext(context(input.family, 3)) },
  {
    name: 'XMM bank',
    change: (cpu) => {
      const bank = cpu.readXmmState();
      bank[0] = 0x123456789abcdef0n;
      cpu.setXmmState(bank);
    },
  },
  { name: 'MXCSR', change: (cpu) => cpu.setMxcsr(cpu.readMxcsr() ^ 0x2000) },
  {
    name: 'x87 bank',
    change: (cpu) => {
      const bank = cpu.readX87State();
      cpu.setX87State({ ...bank, controlWord: bank.controlWord ^ 0x400 });
    },
  },
  { name: 'time bank', change: (cpu) => cpu.setTimeState({ tsc: 100n, tscAux: 0x12345678 }) },
  {
    name: 'debug bank',
    change: (cpu) => cpu.setDebugState({ ...cpu.readDebugState(), addresses: [DATA, 0n, 0n, 0n] }),
  },
  {
    name: 'fast-system bank',
    change: (cpu) =>
      cpu.setFastSystemState({ ...cpu.readFastSystemState(), kernelGsBase: 0x1234000n }),
  },
  {
    name: 'physical observation and zero budget',
    async change(cpu, input) {
      const bytes = cpu.readPhysicalMemory(BigInt(input.pt + 6 * input.width), 4);
      equal(
        new DataView(bytes.buffer, bytes.byteOffset, 4).getUint32(0, true),
        0xe067,
        'raw PTE B',
      );
      const result = await cpu.run({ budget: 0n });
      equal(result.retired, 0n, 'zero budget does not reach an instruction');
    },
  },
];

export function pagingRetentionLifecycleScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    ...families.map((family) => ({
      name: `retained translation ${family} irrelevant imports and observations preserve stale mapping authority`,
      async run() {
        for (const packet of retainedImports) {
          const input = setup(compiler, family);
          await using(side, input, async (cpu) => {
            await warmRetarget(cpu, input);
            await packet.change(cpu, input);
            await readData(cpu, A, `${family}/${packet.name} retains A`);
          });
        }
      },
    })),
    {
      name: 'retained translation same host bank preserves while same guest CR3 and relevant imports refresh',
      async run() {
        for (const family of families) {
          const input = setup(compiler, family);
          await using(side, input, async (cpu) => {
            await warmRetarget(cpu, input);
            cpu.setPagingState(cpu.readPagingState());
            await readData(cpu, A, 'equal host CR3 import preserves A');
            await control(cpu, 4, 0x1000n);
            await readData(cpu, B, 'same-value guest CR3 flushes A');
          });
          for (const bit of [
            0x10000n,
            ...(family === 'fixed' || family === 'ia32e' ? [0x800n] : []),
          ]) {
            const changed = setup(compiler, family);
            await using(side, changed, async (cpu) => {
              await warmRetarget(cpu, changed);
              const bank = cpu.readPagingState();
              cpu.setPagingState(
                bit === 0x800n
                  ? { ...bank, efer: bank.efer ^ bit }
                  : { ...bank, cr0: bank.cr0 ^ bit },
              );
              await readData(cpu, B, 'translation-relevant host bank refreshes B');
            });
          }
        }
      },
    },
    {
      name: 'retained translation rejected controls and administrative candidates preserve old authority',
      async run() {
        for (const family of families) {
          const input = setup(compiler, family);
          await using(side, input, async (cpu) => {
            await warmRetarget(cpu, input);
            const bank = cpu.readPagingState();
            throws(
              () => cpu.setPagingState({ ...bank, cr3: 1n << 63n }),
              'invalid stored CR3 hint',
            );
            throws(
              () => cpu.addBacking({ id: 'ram', bytes: Uint8Array.of(1) }),
              'duplicate backing',
            );
            throws(
              () =>
                cpu.setMappings([
                  {
                    base: 0n,
                    backing: 'missing',
                    offset: 0,
                    length: 1,
                    permissions: { read: true, write: true, execute: true },
                  },
                ]),
              'unknown mapped backing',
            );
            throws(
              () => cpu.writePhysicalMemory(1n << 52n, Uint8Array.of(1)),
              'out-of-domain write',
            );
            await control(cpu, 7, bank.cr4 | 0x8000n, 'protection-fault');
            await readData(cpu, A, 'failed candidates retain A');
          });
        }
      },
    },
    {
      name: 'retained translation successful memory administration revokes even equal PTE writes and reused backing names',
      async run() {
        for (const action of [
          'equal PTE write',
          'equal remap',
          'replacement mapping',
          'backing incarnation',
        ]) {
          const input = setup(compiler, 'fixed');
          await using(side, input, async (cpu) => {
            if (action === 'replacement mapping') {
              const replacement = new Uint8Array(0x1000);
              replacement.set(hex('99aabbcc'));
              cpu.addBacking({ id: 'replacement', bytes: replacement });
            }
            await warmRetarget(cpu, input);
            const memory = input.options.memory;
            check(memory, 'mapped fixture memory');
            if (action === 'equal PTE write')
              cpu.writePhysicalMemory(BigInt(input.pt + 6 * input.width), hex('67e0000000000000'));
            else if (action === 'equal remap') cpu.setMappings(memory.mappings);
            else if (action === 'replacement mapping')
              cpu.setMappings([
                {
                  base: 0n,
                  backing: 'ram',
                  offset: 0,
                  length: 0xa000,
                  permissions: { read: true, write: true, execute: true },
                },
                {
                  base: 0xa000n,
                  backing: 'replacement',
                  offset: 0,
                  length: 0x1000,
                  permissions: { read: true, write: true, execute: false },
                },
                {
                  base: 0xb000n,
                  backing: 'ram',
                  offset: 0xb000,
                  length: 0x35000,
                  permissions: { read: true, write: true, execute: true },
                },
              ]);
            else {
              const replacement = cpu.readPhysicalMemory(0n, 0x40000);
              replacement.set(hex('99aabbcc'), 0xa000);
              cpu.setMappings([]);
              cpu.removeBacking('ram');
              cpu.addBacking({ id: 'ram', bytes: replacement });
              cpu.setMappings(memory.mappings);
            }
            await readData(cpu, B, `${action} revokes stale A rather than exposing replacement C`);
          });
        }
      },
    },
    {
      name: 'retained translation failing outer import preserves independently committed nested administration',
      async run() {
        const input = setup(compiler, 'fixed');
        await using(side, input, async (cpu) => {
          await warmRetarget(cpu, input);
          const bank = cpu.readPagingState();
          const candidate: PagingState = {
            ...bank,
            get cr3() {
              cpu.writePhysicalMemory(0x4030n, hex('67e0000000000000'));
              return 1n << 63n;
            },
          };
          throws(() => cpu.setPagingState(candidate), 'outer stored hint rejection');
          equal(cpu.readPagingState().cr3, 0x1000n, 'failed outer bank stays canonical');
          await readData(cpu, B, 'completed nested PTE administration remains effective');
        });
      },
    },
    {
      name: 'retained global pages survive new roots across all families while PGE0 does not create globals',
      async run() {
        for (const family of families)
          for (const enabled of [false, true]) {
            const input = setup(compiler, family);
            input.leaf(Number(DATA), 0xa167n);
            input.paging.cr4 |= enabled ? PGE : 0n;
            secondRoot(input);
            await using(side, input, async (cpu) => {
              await readData(cpu, A, 'root A global candidate');
              await control(cpu, 4, ROOT_B);
              await readData(cpu, enabled ? A : B, 'selected cross-root global retention');
              await control(cpu, 16, 0n);
              await readData(cpu, B, 'INVLPG removes a global from the prior root');
            });
          }
      },
    },
    {
      name: 'retained INVLPG uses the FS based global address without data matching and checks privilege before a noncanonical no-op',
      async run() {
        const input = setup(compiler, 'fixed');
        input.paging.cr4 |= PGE;
        input.leaf(Number(DATA), 0xa167n);
        input.bytes.set(hex('640f013b'), 0x8080);
        const execution = context('fixed', 0);
        execution.segments.fs.base = DATA;
        input.options.context = execution;
        await using(side, input, async (cpu) => {
          await warmRetarget(cpu, input);
          const debug = cpu.readDebugState();
          cpu.setDebugState({ ...debug, addresses: [DATA + 1n, 0n, 0n, 0n], dr7: 0x30001n });
          cpu.setState(initial(PC + 0x80n, { 3: 1n }, 2n));
          const invalidated = await cpu.run({ budget: 1n });
          equal(invalidated.reason, 'budget', 'FS INVLPG forms an address without a data hit');
          equal(invalidated.retired, 1n, 'FS invalidation retires once');
          equal(invalidated.state.rip, PC + 0x84n, 'only FS INVLPG encoding consumed');
          equal(cpu.readDebugState().dr6, debug.dr6, 'invalidation performs no watched data read');
          cpu.setDebugState({ ...cpu.readDebugState(), dr7: 0n });
          await readData(
            cpu,
            B,
            'FS base selects the retained global rather than raw offset page0',
          );
        });
        const user = setup(compiler, 'fixed');
        user.bytes.set(hex('0f013b'), 0x8080);
        user.options.context = context('fixed', 3);
        await using(side, user, async (cpu) => {
          const before = initial(PC + 0x80n, { 3: 0x800000000000n }, 2n);
          cpu.setState(before);
          const refused = await cpu.run({ budget: 1n });
          equal(refused.reason, 'protection-fault', 'user INVLPG cannot use noncanonical no-op');
          equal(refused.protectionFault?.phase, 'privilege', 'privilege precedes target handling');
          equal(refused.retired, 0n, 'privilege failure does not retire');
          equal(refused.state.rip, before.rip, 'privilege failure keeps reached boundary');
          equal(cpu.readPagingState().cr2, 0xdeadn, 'privilege refusal performs no target walk');
        });
      },
    },
    {
      name: 'retained global restrictive rights fault once then retry through the different current root',
      async run() {
        for (const family of families) {
          const input = setup(compiler, family);
          input.paging.cr0 |= 0x10000n;
          input.paging.cr4 |= PGE;
          input.leaf(Number(DATA), 0xa161n);
          secondRoot(input);
          await using(side, input, async (cpu) => {
            await readData(cpu, A, 'readable D1 global has cached RW0');
            await control(cpu, 4, ROOT_B);
            await readData(cpu, A, 'restrictive global survives the root switch');
            const before = initial(PC + 2n, { 1: DATA, 2: 0xdeadbeefn }, 2n);
            cpu.setState(before);
            const denied = await cpu.run({ budget: 1n });
            equal(denied.reason, 'page-fault', 'cached global denies the first write');
            equal(denied.retired, 0n, 'denied global write does not retire');
            equal(denied.remaining, 1n, 'denied write keeps its allowance');
            equal(denied.state.rip, PC + 2n, 'retry boundary remains the write');
            check(denied.pageFault, 'cached global page fault metadata');
            equal(denied.pageFault.errorCode, 3, 'supervisor present write fault');
            equal(denied.pageFault.address, DATA, 'fault address is the cached global LA');
            equal(cpu.readPagingState().cr2, DATA, 'CR2 records the selected architectural PF');
            for (const [physical, expected] of [
              [0xa000n, A],
              [0xe000n, B],
            ] as const) {
              const bytes = cpu.readPhysicalMemory(physical, 4);
              equal(
                BigInt(new DataView(bytes.buffer, bytes.byteOffset, 4).getUint32(0, true)),
                expected,
                'denied cached write publishes no payload',
              );
            }
            cpu.setState(before);
            const retried = await cpu.run({ budget: 1n });
            equal(retried.reason, 'budget', 'PF eviction forces a fresh root-B write admission');
            equal(retried.retired, 1n, 'repair-free retry retires once');
            await readData(cpu, 0xdeadbeefn, 'fresh writable B receives the captured payload');
            const old = cpu.readPhysicalMemory(0xa000n, 4);
            equal(
              BigInt(new DataView(old.buffer, old.byteOffset, 4).getUint32(0, true)),
              A,
              'old global A remains unmodified',
            );
          });
        }
      },
    },
    {
      name: 'retained restrictive user and NX facts each fault once before a repair-free retry uses the newly granted mapping',
      async run() {
        for (const execute of [false, true]) {
          const input = setup(compiler, 'fixed');
          if (execute) {
            input.paging.efer |= 0x800n;
            input.options.context = context('fixed', 3);
            input.leaf(Number(DATA), 0x800000000000a067n);
            input.leaf(0x5000, 0x4067n);
            input.bytes.set(hex('488911'), 0x8090);
            input.bytes.set(hex('b82222222282'), 0xe000);
          } else input.leaf(Number(DATA), 0xa063n);
          await using(side, input, async (cpu) => {
            await readData(cpu, A, 'read fills a restrictive but readable translation');
            if (execute) {
              cpu.setState(initial(PC + 0x90n, { 1: 0x5030n, 2: 0xe067n }, 2n));
              const edited = await cpu.run({ budget: 1n });
              equal(edited.reason, 'budget', 'guest qword store clears XD and retargets');
              equal(edited.retired, 1n, 'permission-granting store retires once');
            } else {
              await editLeaf(cpu, input);
              cpu.setContext(context('fixed', 3));
            }
            cpu.setState(initial(execute ? DATA : PC, { 3: DATA }, 2n));
            const denied = await cpu.run({ budget: 1n });
            equal(denied.reason, 'page-fault', 'cached restrictive rights select one PF');
            equal(denied.pageFault?.address, DATA, 'cached denial reports the requested LA');
            equal(denied.pageFault?.errorCode, execute ? 21 : 5, 'live user/fetch PF provenance');
            equal(denied.retired, 0n, 'denied operation does not retire');
            const retried = await cpu.run({ budget: 1n });
            equal(retried.reason, 'budget', 'PF eviction prevents stale-permission refault');
            equal(retried.retired, 1n, 'fresh mapping completes once');
            equal(
              retried.state.registers[0]! & 0xffffffffn,
              execute ? 0x22222222n : B,
              'fresh mapping supplies the new payload or instruction',
            );
            equal(retried.state.rip, execute ? DATA + 5n : PC + 2n, 'new exact successor');
            equal(cpu.readPagingState().cr2, DATA, 'successful retry preserves the PF address');
          });
        }
      },
    },
    {
      name: 'retained PCIDs preserve old root facts and CR3 flushes the destination context alone',
      async run() {
        for (const family of ['ia32e', 'fixed'] as const) {
          const input = setup(compiler, family);
          secondRoot(input);
          input.paging.cr3 = 0x1001n;
          input.paging.cr4 |= PCIDE;
          await using(side, input, async (cpu) => {
            await readData(cpu, A, 'PCID1 root A');
            await editLeaf(cpu, input, 0xf067n);
            await control(cpu, 4, ROOT_B | 2n);
            await readData(cpu, B, 'PCID2 root B');
            await control(cpu, 4, NO_FLUSH | 0x1001n);
            await readData(cpu, A, 'PCID1 keeps A despite changed old-root PTE');
            await control(cpu, 4, NO_FLUSH | ROOT_B | 1n);
            await readData(cpu, A, 'same PCID new root does not defeat no-flush');
            await control(cpu, 4, ROOT_B | 1n);
            await readData(cpu, B, 'normal CR3 refreshes the same PCID');
            await editLeaf(cpu, input, 0xf067n, 0x8000n);
            await control(cpu, 4, NO_FLUSH | ROOT_B | 2n);
            await readData(cpu, B, 'PCID2 remains stale after a guest PTE edit');
            await control(cpu, 4, ROOT_B | 1n);
            await readData(cpu, C, 'different destination PCID1 is flushed');
            await control(cpu, 4, NO_FLUSH | ROOT_B | 2n);
            await readData(cpu, B, 'old current PCID2 survived the PCID1 flush');
            equal(cpu.readPagingState().cr3, ROOT_B | 2n, 'no-flush command is not stored');
          });
        }
      },
    },
    {
      name: 'retained control PGE toggles flush but PCIDE enabling alone preserves PCID0',
      async run() {
        for (const family of families) {
          const input = setup(compiler, family);
          await using(side, input, async (cpu) => {
            await warmRetarget(cpu, input);
            await control(cpu, 7, cpu.readPagingState().cr4 | PGE);
            await readData(cpu, B, 'PGE toggle invalidates nonglobal A');
          });
        }
        for (const family of ['ia32e', 'fixed'] as const) {
          const input = setup(compiler, family);
          await using(side, input, async (cpu) => {
            await warmRetarget(cpu, input);
            await control(cpu, 7, cpu.readPagingState().cr4 | PCIDE);
            await readData(cpu, A, 'PCIDE0 to1 preserves PCID0 A');
          });
        }
      },
    },
    {
      name: 'retained raw CR3 roundtrips through PCIDE disable while re-enable and compatibility PG clear fault atomically',
      async run() {
        for (const family of ['ia32e', 'fixed'] as const) {
          const input = setup(compiler, family);
          await using(side, input, async (cpu) => {
            await control(cpu, 7, 0x20628n);
            await control(cpu, 4, NO_FLUSH | 0x1abcn);
            equal(cpu.readPagingState().cr3, 0x1abcn, 'tag retained and hint stripped');
            await control(cpu, 7, 0x628n);
            equal(cpu.readPagingState().cr3, 0x1abcn, 'MOV CR4 preserves old raw CR3');
            cpu.setPagingState(cpu.readPagingState());
            const read = await control(cpu, 10, 0n);
            equal(read.state.registers[0], 0x1abcn, 'MOV from CR3 reads raw low bits');
            await control(cpu, 7, 0x20628n, 'protection-fault');
            equal(cpu.readPagingState().cr4, 0x628n, 'failed enabling preserves CR4');
            await control(cpu, 4, 0x1abcn);
            equal(cpu.readPagingState().cr3, 0x1018n, 'actual MOV CR3 normalizes ignored bits');
            await control(cpu, 7, 0x20628n, 'protection-fault');
            await control(cpu, 4, 0x1000n);
            await control(cpu, 7, 0x20628n);
            cpu.setContext(context(family, 0, 32));
            await control(cpu, 4, NO_FLUSH | 0x1002n);
            equal(cpu.readPagingState().cr3, 0x1002n, 'non64 operand cannot supply bit63');
            await control(cpu, 13, 0x33n, 'protection-fault');
            equal(cpu.readPagingState().cr0, 0x80000033n, 'PCIDE blocks compatibility PG clear');
          });
        }
      },
    },
    {
      name: 'retained physical family exit and reset execute identity reads without reusing an old paged GPA',
      async run() {
        for (const reset of [false, true]) {
          const input = setup(compiler, 'ia32e');
          const memory = input.options.memory;
          check(memory, 'physical mapped fixture');
          if (reset) {
            memory.backings.push({ id: 'reset', bytes: hex('67668b03') });
            memory.mappings.push({
              base: 0xfffffff0n,
              backing: 'reset',
              offset: 0,
              length: 4,
              permissions: { read: true, write: false, execute: true },
            });
          }
          await using(side, input, async (cpu) => {
            await warmRetarget(cpu, input);
            if (reset) cpu.reset();
            else {
              await control(cpu, 7, 0x20628n);
              await control(cpu, 4, NO_FLUSH | 0x1abcn);
              await control(cpu, 7, 0x628n);
              cpu.setContext(context('ia32e', 0, 32));
              await control(cpu, 13, 0x33n);
              equal(cpu.readPagingState().cr3, 0x1abcn, 'raw CR3 survives PG clear');
              cpu.setPagingState(cpu.readPagingState());
            }
            const bank = cpu.readPagingState();
            equal(bank.cr0 & 0x80000000n, 0n, 'unpaged family selected');
            equal(bank.cr4 & PCIDE, 0n, 'PCID disabled');
            cpu.setState(initial(reset ? 0xfff0n : 0x8000n, { 3: DATA }, 2n));
            const result = await cpu.run({ budget: 1n });
            equal(result.reason, 'budget', 'unpaged identity read completes');
            equal(result.retired, 1n, 'unpaged identity read retires once');
            equal(
              result.state.registers[0]! & 0xffffffffn,
              0xa5a5a5a5n,
              'physical DATA replaces old GPA A',
            );
          });
        }
      },
    },
  ];
}
