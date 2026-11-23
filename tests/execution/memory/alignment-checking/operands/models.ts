import { using as usingModel, type Profile } from '../../../identity-time/oracle.js';
import { stack, ONE } from '../../../numeric/x87/fixture.js';
import { banks } from '../../../numeric/simd/fixture.js';
import { packets } from './classes.js';
import {
  AC,
  AUX,
  createCpu,
  state,
  scalar,
  checkFault,
  same,
  bytes,
  preserved,
  equal,
  clock,
  type Scenario,
} from '../fixture.js';

const models: Profile[] = [
  { mode: 'protected', bits: 32, stack: 32, cpl: 3, family: 'pg0' },
  { mode: 'protected', bits: 32, stack: 32, cpl: 3, family: 'legacy32' },
  { mode: 'protected', bits: 32, stack: 32, cpl: 3, family: 'pae' },
  { mode: 'compatibility', bits: 32, stack: 32, cpl: 3, family: 'ia32e' },
  { mode: 'long64', bits: 64, cpl: 3, family: 'ia32e' },
  { mode: 'compatibility', bits: 32, stack: 32, cpl: 3, family: 'fixed' },
  { mode: 'long64', bits: 64, cpl: 3, family: 'fixed' },
];

export function modelScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'alignment checking representative semantic carriers run across all seven active model and mode combinations',
      async run() {
        equal(models.length, 7, 'independently authored active model representatives');
        for (const profile of models) {
          const long = profile.mode === 'long64';
          for (const literal of packets.filter((p) => !p.long || long)) {
            // The same SGDT/SIDT encoding has a selected eight-byte base component
            // in Long64. Compatibility retains its four-byte base component.
            const packet =
              long && literal.field !== undefined ? { ...literal, width: 8, granule: 8 } : literal;
            await usingModel({ createCpu, compiler }, packet.code, profile, async (cpu) => {
              const bank = cpu.readPagingState();
              cpu.setPagingState({ ...bank, cr0: bank.cr0 | AC });
              const before = state({
                3: packet.at,
                6: packet.si ? packet.at : 0x6001n,
                7: packet.di ? packet.at : 0x7001n,
              });
              const numeric = stack({ 0: ONE }, 4);
              numeric.registers[1] = 0x8080808080808080n;
              cpu.setX87State(numeric);
              cpu.setXmmState(banks());
              cpu.setMxcsr(0x1f80);
              cpu.setState(before);
              cpu.setTimeState({ tsc: 100n, tscAux: AUX });
              const ram = cpu.readPhysicalMemory(0n, 0x50000),
                context = cpu.readContext(),
                controls = cpu.readPagingState(),
                unchanged = preserved(cpu);
              const result = await cpu.run({ budget: 1n, repeatBudget: 7n });
              checkFault(
                result,
                packet.field ?? packet.at,
                packet.width,
                packet.write ? 'write' : 'read',
                packet.granule,
              );
              scalar(result, before);
              unchanged();
              bytes(
                cpu.readPhysicalMemory(0n, 0x50000),
                ram,
                `${profile.family}/${profile.mode}/${packet.name} full RAM`,
              );
              same(cpu.readContext(), context, 'complete carrier context');
              same(cpu.readPagingState(), controls, 'carrier CR2 and control bank');
              clock(cpu);
            });
          }
          for (const call of [false, true]) {
            const width = long ? 8 : 4,
              address = call ? (long ? 0x8ff9n : 0x8ffdn) : 0x9001n;
            await usingModel(
              { createCpu, compiler },
              call ? 'e800000000' : 'c3',
              profile,
              async (cpu) => {
                const bank = cpu.readPagingState();
                cpu.setPagingState({ ...bank, cr0: bank.cr0 | AC });
                const before = state({ 4: 0x9001n }),
                  ram = cpu.readPhysicalMemory(0n, 0x50000),
                  context = cpu.readContext(),
                  controls = cpu.readPagingState();
                cpu.setState(before);
                cpu.setTimeState({ tsc: 100n, tscAux: AUX });
                const unchanged = preserved(cpu),
                  result = await cpu.run({ budget: 1n, repeatBudget: 7n });
                checkFault(result, address, width, call ? 'write' : 'read', width, 'ss');
                scalar(result, before);
                unchanged();
                bytes(cpu.readPhysicalMemory(0n, 0x50000), ram, 'near stack full RAM');
                same(cpu.readContext(), context, 'near target not published');
                same(cpu.readPagingState(), controls, 'near stack CR2/control bank');
                clock(cpu);
              },
            );
          }
        }
      },
    },
  ];
}
