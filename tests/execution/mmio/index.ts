import { createCpu, type Cpu, type MmioProvider } from '../../../sdk/src/index.js';
import { createMachine, type MachineOptions } from '../../../sdk/src/machine.js';
import { BANK, PC, little, usingPaged, word } from '../paging/fixture.js';
import { protectedSnapshot } from '../../support/context.js';
import {
  CODE,
  LIMITS,
  check,
  equal,
  hex,
  initial,
  rejects,
  same,
  throws,
  type Scenario,
} from '../../support/fixture.js';

const DEVICE = 0xffff800000004000n;
const RAM = 0x5000n;

function linearOptions(compiler: WebAssembly.Module, code: string, mmio: MmioProvider) {
  return {
    compiler,
    limits: LIMITS,
    code: { base: CODE, bytes: hex(code) },
    ram: { base: RAM, bytes: new Uint8Array(16) },
    mmio,
  };
}

export function mmioScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'scalar MMIO MOV reports exact widths, unsigned bits and an owned provider capture',
      async run() {
        const trace: unknown[][] = [];
        const counts = { option: 0, ranges: 0, base: 0, length: 0, read: 0, write: 0 };
        const values = { 8: 0xben, 16: 0x89abn, 32: 0xdeadbeefn, 64: 0xfedcba9876543210n };
        let cpu: Cpu | undefined;
        let base = DEVICE;
        const receiver: MmioProvider = {
          get ranges() {
            counts.ranges++;
            return [
              {
                get base() {
                  counts.base++;
                  return base;
                },
                get length() {
                  counts.length++;
                  return 16;
                },
              },
            ];
          },
          get read() {
            counts.read++;
            return function (this: MmioProvider, address, width) {
              equal(this, receiver, 'original read receiver');
              throws(() => cpu?.readState(), 'provider cannot reenter a running cpu');
              trace.push(['read', address, width]);
              return values[width];
            } satisfies MmioProvider['read'];
          },
          get write() {
            counts.write++;
            return function (this: MmioProvider, address, width, value) {
              equal(this, receiver, 'original write receiver');
              trace.push(['write', address, width, value]);
            } satisfies MmioProvider['write'];
          },
        };
        const data = linearOptions(
          compiler,
          '8a03884301668b03668943028b03894304488b0348894308',
          receiver,
        );
        const creating = createCpu({
          ...data,
          get mmio() {
            counts.option++;
            return receiver;
          },
        });
        base = RAM + 0x1000n;
        Object.defineProperty(receiver, 'read', { value: () => 0n });
        cpu = await creating;
        try {
          cpu.setState(initial(CODE, { 3: DEVICE }));
          same(
            counts,
            { option: 1, ranges: 1, base: 1, length: 1, read: 1, write: 1 },
            'exact construction capture',
          );
          const stopped = await cpu.run({ budget: 8n, stop: CODE });
          equal(stopped.reason, 'stop', 'stop before first operation');
          same(trace, [], 'no speculative effects');
          const result = await cpu.run({ budget: 8n });
          equal(result.reason, 'budget', 'all eight MOV instructions execute');
          equal(result.retired, 8n, 'exact retirement');
          equal(result.state.registers[0], values[64], 'read preserves high bit');
          same(
            trace,
            [
              ['read', DEVICE, 8],
              ['write', DEVICE + 1n, 8, values[8]],
              ['read', DEVICE, 16],
              ['write', DEVICE + 2n, 16, values[16]],
              ['read', DEVICE, 32],
              ['write', DEVICE + 4n, 32, values[32]],
              ['read', DEVICE, 64],
              ['write', DEVICE + 8n, 64, values[64]],
            ],
            'one effect per instruction',
          );
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'paged scalar MMIO resolves guest physical addresses and obeys entry permissions',
      async run() {
        const physical = 0xa000n;
        const linear = 0x401000n;
        const trace: unknown[][] = [];
        const mmio: MmioProvider = {
          ranges: [{ base: physical, length: 16 }],
          read(address, width) {
            trace.push(['read', address, width]);
            return 0x1122334455667788n;
          },
          write(address, width, value) {
            trace.push(['write', address, width, value]);
          },
        };
        await usingPaged(
          compiler,
          {
            code: '488b0348894308',
            words: [[0x4008n, 0xa007n]],
            bank: { ...BANK, cr0: BANK.cr0 | (1n << 16n) },
            mmio,
          },
          async (cpu) => {
            cpu.setState(initial(PC, { 3: linear }));
            equal((await cpu.run({ budget: 2n })).retired, 2n, 'paged MOV pair retires');
            same(
              trace,
              [
                ['read', physical, 64],
                ['write', physical + 8n, 64, 0x1122334455667788n],
              ],
              'callback receives physical address',
            );
            equal(
              word(cpu, 0x4008n),
              0xa067n,
              'successful paged read and write publish guest A/D bits',
            );

            cpu.setState(initial(PC, { 3: linear + 0x1000n }));
            const denied = await cpu.run({ budget: 1n });
            check(denied.reason === 'page-fault', 'unmapped page faults before MMIO');
            equal(denied.pageFault.address, linear + 0x1000n, 'faulting linear address');
            cpu.writePhysicalMemory(0x4008n, little([0xa025n]));
            cpu.setState(initial(PC + 3n, { 3: linear }));
            const readOnly = await cpu.run({ budget: 1n });
            check(readOnly.reason === 'page-fault', 'read-only page faults before MMIO write');
            equal(readOnly.pageFault.address, linear + 8n, 'faulting store linear address');
            equal(readOnly.pageFault.errorCode, 3, 'present page denied the supervisor write');
            equal(word(cpu, 0x4008n), 0xa025n, 'denied store does not dirty the guest page');
            equal(trace.length, 2, 'page fault does not invoke device');
          },
        );
      },
    },
    {
      name: 'unpaged physical MMIO follows the guest physical plane',
      async run() {
        const code = hex('8b03894304');
        const physical = 0xc000n;
        const trace: unknown[][] = [];
        const cpu = await createCpu({
          compiler,
          limits: LIMITS,
          context: protectedSnapshot(),
          memoryModel: 'physical',
          paging: { cr0: 0x33n, cr2: 0n, cr3: 0n, cr4: 0n, efer: 0n },
          entry: CODE,
          memory: {
            backings: [{ id: 'code', bytes: code }],
            mappings: [
              {
                base: CODE,
                backing: 'code',
                offset: 0,
                length: code.length,
                permissions: { read: true, write: false, execute: true },
              },
            ],
          },
          memoryLimits: { maxBackingBytes: 64, maxBackings: 1, maxMappings: 2 },
          mmio: {
            ranges: [{ base: physical, length: 8 }],
            read(address, width) {
              trace.push(['read', address, width]);
              return 0xfedcba98n;
            },
            write(address, width, value) {
              trace.push(['write', address, width, value]);
            },
          },
        });
        try {
          cpu.setState(initial(CODE, { 3: physical }));
          const result = await cpu.run({ budget: 2n });
          equal(result.retired, 2n, 'protected-mode MOV pair retires');
          same(
            trace,
            [
              ['read', physical, 32],
              ['write', physical + 4n, 32, 0xfedcba98n],
            ],
            'guest physical callback address',
          );
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'MMIO denies host access, fetch, misalignment and read-modify-write with no device effect',
      async run() {
        let calls = 0;
        const mmio: MmioProvider = {
          ranges: [{ base: DEVICE, length: 16 }],
          read() {
            calls++;
            return 0n;
          },
          write() {
            calls++;
          },
        };
        const cpu = await createCpu({
          ...linearOptions(compiler, '488b03f048ff030fc303', mmio),
          memoryLimits: { maxBackingBytes: 256, maxBackings: 2, maxMappings: 4 },
        });
        try {
          throws(() => cpu.readMemory(DEVICE, 8), 'host reads bypass MMIO');
          throws(() => cpu.writeMemory(DEVICE, new Uint8Array(8)), 'host writes bypass MMIO');
          throws(
            () =>
              cpu.setMappings([
                {
                  base: DEVICE,
                  backing: 'ram',
                  offset: 0,
                  length: 8,
                  permissions: { read: true, write: true, execute: false },
                },
              ]),
            'replacement RAM cannot overlap a registered device',
          );
          cpu.setState(initial(CODE, { 3: DEVICE + 1n }));
          equal((await cpu.run({ budget: 1n })).reason, 'data-fault', 'misaligned MOV is refused');
          cpu.setState(initial(CODE + 3n, { 3: DEVICE }));
          equal((await cpu.run({ budget: 1n })).reason, 'data-fault', 'LOCK INC is refused');
          cpu.setState(initial(CODE + 7n, { 3: DEVICE }));
          equal((await cpu.run({ budget: 1n })).reason, 'data-fault', 'MOVNTI is refused');
          cpu.setState(initial(DEVICE));
          equal((await cpu.run({ budget: 1n })).reason, 'fetch-fault', 'device cannot execute');
          equal(calls, 0, 'refusal paths have no device effect');
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'MMIO leaves ordinary RAM MOV intact and refuses mixed-region and REP stores atomically',
      async run() {
        const device = RAM + 16n;
        const payload = 0x8877665544332211n;
        let effects = 0;
        const cpu = await createCpu(
          linearOptions(compiler, '488903488b03f3aa', {
            ranges: [{ base: device, length: 16 }],
            read() {
              effects++;
              return 0n;
            },
            write() {
              effects++;
            },
          }),
        );
        try {
          cpu.setState(initial(CODE, { 0: payload, 3: RAM }));
          const ordinary = await cpu.run({ budget: 2n });
          equal(ordinary.retired, 2n, 'ordinary RAM MOV pair retires');
          equal(ordinary.state.registers[0], payload, 'RAM MOV load returns its stored value');
          same(cpu.readMemory(RAM, 16), little([payload, 0n]), 'ordinary MOV stores in RAM');

          cpu.setState(initial(CODE, { 0: 0xffffffffffffffffn, 3: device - 4n }));
          const split = await cpu.run({ budget: 1n });
          check(split.reason === 'data-fault', 'RAM-to-device store faults');
          equal(split.fault.failingAddress, device, 'first failing byte is in the device');
          equal(split.retired, 0n, 'mixed store does not retire');

          cpu.setState(initial(CODE + 6n, { 0: 0xa5n, 1: 2n, 7: device }));
          const repeat = await cpu.run({ budget: 1n, repeatBudget: 2n });
          equal(repeat.reason, 'data-fault', 'REP STOSB to a device faults');
          equal(repeat.retired, 0n, 'faulting REP does not retire');
          equal(repeat.repeatIterations, 0n, 'failed REP element is not counted');
          equal(repeat.state.registers[1], 2n, 'failed element leaves count unchanged');
          equal(repeat.state.registers[7], device, 'failed element leaves destination unchanged');
          same(cpu.readMemory(RAM, 16), little([payload, 0n]), 'refusals do not publish any bytes');
          equal(effects, 0, 'only aligned MOV within one device may call the provider');
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'a translated RAM span falls through when the next loop iteration reaches MMIO',
      async run() {
        const device = RAM + 16n;
        const trace: unknown[][] = [];
        const cpu = await createCpu(
          linearOptions(compiler, '488b034883c308ebf7', {
            ranges: [{ base: device, length: 8 }],
            read(address, width) {
              trace.push([address, width]);
              return 42n;
            },
            write() {
              throw new Error('the guest only reads from the device');
            },
          }),
        );
        try {
          cpu.writeMemory(RAM + 8n, little([17n]));
          cpu.setState(initial(CODE, { 3: RAM + 8n }));
          const result = await cpu.run({ budget: 5n });
          equal(result.retired, 5n, 'both the RAM iteration and the device read retire');
          equal(result.state.registers[0], 42n, 'second iteration returns the device value');
          same(trace, [[device, 64]], 'the first iteration uses RAM and the second uses MMIO');
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'MMIO validates map capacity and output contracts and rejects parallel configuration',
      async run() {
        let calls = 0;
        const mmio: MmioProvider = {
          ranges: [{ base: DEVICE, length: 8 }],
          read() {
            calls++;
            return 256n;
          },
          write() {
            calls++;
          },
        };
        const options = linearOptions(compiler, '8a03', mmio);
        await rejects(
          () =>
            createCpu({
              ...options,
              memoryLimits: { maxBackingBytes: 256, maxBackings: 2, maxMappings: 2 },
            }),
          'MMIO reserves a mapping slot',
        );
        await rejects(
          () =>
            createCpu({
              ...options,
              mmio: { ...mmio, ranges: [{ base: RAM, length: 8 }] },
            }),
          'MMIO and initial RAM cannot overlap',
        );
        await rejects(
          () => createMachine({ mmio } as unknown as MachineOptions),
          'parallel rejects MMIO before startup',
        );
        equal(calls, 0, 'rejected configuration does not invoke device');
        const cpu = await createCpu(options);
        try {
          cpu.setState(initial(CODE, { 3: DEVICE }));
          await rejects(() => cpu.run({ budget: 1n }), 'read result must fit 8 bits');
          equal(calls, 1, 'invalid read is attempted exactly once');
          throws(() => cpu.readState(), 'MMIO callback failure fails the CPU');
        } finally {
          cpu.dispose();
        }
      },
    },
  ];
}
