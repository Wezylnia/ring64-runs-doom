import { check, type Scenario } from '../../support/fixture.js';
import {
  ENTRY,
  RAM,
  cpuState,
  done,
  hex,
  linearMachine,
  same,
  withMachine,
  type MachineFactory,
} from './fixture.js';

type Operation = 'read-x' | 'read-y' | 'write-x' | 'write-y' | 'exchange-y';
type Streams = readonly (readonly Operation[])[];

function outcomes(
  streams: Streams,
  positions = streams.map(() => 0),
  ram = [0, 0],
  reads = streams.map((): number[] => []),
): Set<string> {
  const seen = new Set<string>();
  for (const [cpu, operations] of streams.entries()) {
    const operation = operations[positions[cpu]!];
    if (operation === undefined) continue;
    const slot = operation.endsWith('x') ? 0 : 1;
    const nextPositions = positions.slice();
    const nextRam = ram.slice();
    const nextReads = reads.slice();
    nextPositions[cpu]!++;
    if (operation !== 'write-x' && operation !== 'write-y')
      nextReads[cpu] = [...reads[cpu]!, ram[slot]!];
    if (operation !== 'read-x' && operation !== 'read-y') nextRam[slot] = 1;
    for (const key of outcomes(streams, nextPositions, nextRam, nextReads)) seen.add(key);
  }
  if (!seen.size) seen.add(encode(reads));
  return seen;
}

function encode(reads: readonly (readonly number[])[]): string {
  return reads.map((values) => values.join('')).join('/');
}

function program(operations: readonly Operation[]) {
  let observations = 0;
  let code = '';
  for (const operation of operations) {
    switch (operation) {
      case 'read-x':
        code += ['8b07', '8b1f'][observations++]!;
        break;
      case 'read-y':
        code += ['8b4740', '8b5f40'][observations++]!;
        break;
      case 'write-x':
        code += 'c70701000000';
        break;
      case 'write-y':
        code += 'c7474001000000';
        break;
      case 'exchange-y':
        check(observations++ === 0, 'implicit LOCK XCHG uses the initial EAX value');
        code += '874740';
        break;
    }
  }
  return { code: hex(code), registers: [0, 3].slice(0, observations) };
}

const litmus = [
  {
    name: 'message passing',
    streams: [
      ['write-x', 'write-y'],
      ['read-y', 'read-x'],
    ],
    reordered: [
      ['write-y', 'write-x'],
      ['read-y', 'read-x'],
    ],
    forbidden: '/10',
  },
  {
    name: 'store buffering',
    streams: [
      ['write-x', 'read-y'],
      ['write-y', 'read-x'],
    ],
    reordered: [
      ['read-y', 'write-x'],
      ['read-x', 'write-y'],
    ],
    forbidden: '0/0',
  },
  {
    name: 'IRIW',
    streams: [['write-x'], ['write-y'], ['read-x', 'read-y'], ['read-y', 'read-x']],
    reordered: [['write-x'], ['write-y'], ['read-y', 'read-x'], ['read-y', 'read-x']],
    forbidden: '//10/10',
    reorderedWitness: '//01/10',
  },
  {
    name: 'locked publication',
    streams: [
      ['write-x', 'exchange-y'],
      ['read-y', 'read-x'],
    ],
    reordered: [
      ['exchange-y', 'write-x'],
      ['read-y', 'read-x'],
    ],
    forbidden: '0/10',
  },
] as const;

export function parallelLitmusScenarios(
  factory: MachineFactory,
  compiler: WebAssembly.Module,
): Scenario[] {
  return litmus.map((probe) => ({
    name: `shared publication matches the sequentially consistent ${probe.name} litmus`,
    async run() {
      const allowed = outcomes(probe.streams);
      const weak = outcomes(probe.reordered);
      check(!allowed.has(probe.forbidden), 'independent SC model rejects the forbidden outcome');
      const witness = 'reorderedWitness' in probe ? probe.reorderedWitness : probe.forbidden;
      check(weak.has(witness), 'reorder control allows the forbidden outcome');

      const programs = probe.streams.map(program);
      const code = new Uint8Array(programs.length * 32).fill(0x90);
      const options = linearMachine(compiler, code, programs.length);
      for (const [index, { code: bytes }] of programs.entries()) {
        const offset = index * 32;
        code.set(bytes, offset);
        options.cpus[index]!.entry = ENTRY + BigInt(offset);
      }
      await withMachine(factory, options, async (machine) => {
        for (let sample = 0; sample < 12; sample++) {
          await machine.writeMemory(RAM, new Uint8Array(68));
          const ordered = [...machine.cpus];
          if (sample % 2) ordered.reverse();
          for (const cpu of ordered) {
            await cpu.setState(cpuState(options.cpus[cpu.id]!.entry, { 0: 1n, 3: 0n, 7: RAM }));
          }
          const reads: number[][] = [];
          await Promise.all(
            ordered.map(async (cpu) => {
              const { code: bytes, registers } = programs[cpu.id]!;
              const stop = options.cpus[cpu.id]!.entry + BigInt(bytes.length);
              const retired = BigInt(probe.streams[cpu.id]!.length);
              const result = await cpu.run({ budget: retired + 1n, stop });
              done(result, stop, retired);
              reads[cpu.id] = registers.map((reg) => Number(result.state.registers[reg]!));
            }),
          );
          const key = encode(reads);
          check(allowed.has(key), `SC model rejects observed outcome ${key}`);
          const bytes = await machine.readMemory(RAM, 68);
          const view = new DataView(bytes.buffer, bytes.byteOffset);
          same(
            [view.getUint32(0, true), view.getUint32(64, true)],
            [1, 1],
            'both writes reached RAM',
          );
        }
      });
    },
  }));
}
