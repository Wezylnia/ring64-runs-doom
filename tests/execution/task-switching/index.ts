import { taskDebugCoincidenceScenario } from './debug-coincidence.js';
import { deferredTaskDebugScenario } from './deferred-debug.js';
import { taskDebugEntryScenario } from './debug-entry.js';
import { taskEntryFaultScenarios } from './entry-faults.js';
import { taskDeliveryRetryScenario } from './delivery-retry.js';
import { invalidTaskSelectorScenario } from './qualify.js';
import { taskJumpStorageScenarios } from './jump-storage.js';
import { roundRobinScenarios } from './round-robin.js';
import { taskRetryScenario } from './retry.js';
import { taskDebugScenario } from './debug.js';
import { gateSourceScenario } from './sources.js';
import { taskPagingScenarios } from './paging.js';
import {
  AFTER_EXIT,
  ENTRY_B,
  ENTRY_C,
  GDT,
  IRET_B,
  IRET_C,
  PAGE_A,
  PAGE_B,
  PD_A,
  PD_B,
  RESUME_A,
  TSS_A,
  TSS_B,
  TSS_C,
  check,
  dword,
  equal,
  markers,
  same,
  toCaller,
  withGuest,
  type Scenario,
} from './fixture.js';

export function taskSwitchingScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...roundRobinScenarios(compiler),
    ...taskJumpStorageScenarios(compiler),
    taskRetryScenario(compiler),
    taskDeliveryRetryScenario(compiler),
    invalidTaskSelectorScenario(compiler),
    taskDebugScenario(compiler),
    taskDebugCoincidenceScenario(compiler),
    taskDebugEntryScenario(compiler),
    ...taskEntryFaultScenarios(compiler),
    deferredTaskDebugScenario(compiler),
    gateSourceScenario(compiler),
    ...taskPagingScenarios(compiler),
    {
      name: 'protected hardware tasks execute the complete Bochs nested repair guest',
      async run() {
        await withGuest(compiler, async (cpu, ports) => {
          const r = await cpu.run({
            budget: 20000n,
            repeatBudget: 7168n,
            exceptionDelivery: { budget: 8n },
            stop: AFTER_EXIT,
          });
          equal(r.reason, 'stop', 'completed three-task program');
          equal(r.state.rip, AFTER_EXIT, 'exit port instruction completed');
          same(ports, [...markers('SCRBAP'), [0xf4, 32, 1]], 'all guest port writes');
          equal(r.deliveryAttempts, 1n, 'one delivered #TS');
          equal(r.remainingDeliveryBudget, 7n, 'one delivery allowance spent');
          equal(r.repeatIterations, 7168n, 'only bootstrap used REP');
          equal(cpu.readContext().tr.selector, 0x18, 'restored A.TR');
          equal(cpu.readPagingState().cr3, PD_A, 'restored A address space');
          equal(r.state.registers[4], 0x90000n, 'restored A.ESP');
          equal(dword(cpu, TSS_A + 32n), Number(RESUME_A), 'A saved the post-CALL EIP');
          equal(dword(cpu, PAGE_A + 8n), 0x1234ab56, 'A data kept in its own page');
          equal(
            dword(cpu, PAGE_B + 8n),
            0x897acbde,
            'B data updated through the shared linear address',
          );
        });
      },
    },
    {
      name: 'protected hardware task CALL keeps an early #NP in A and reports late #TS in B',
      async run() {
        await withGuest(compiler, async (cpu, ports) => {
          await toCaller(cpu);
          const oldState = cpu.readState(),
            oldContext = cpu.readContext(),
            oldTime = cpu.readTimeState();
          const save = cpu.readPhysicalMemory(TSS_A, 3 * 104);
          const bAccess = GDT + 0x20n + 5n;
          cpu.writePhysicalMemory(bAccess, Uint8Array.of(0x09));
          const early = await cpu.run({ budget: 2n, repeatBudget: 7n });
          equal(early.reason, 'protection-fault', 'absent TSS fault');
          equal(early.protectionFault?.vector, 11, 'early #NP');
          equal(early.protectionFault?.errorCode, 0x20, 'target selector');
          equal(early.retired, 0n, 'early failure does not retire CALL');
          same(early.state, oldState, 'outgoing scalar and event state');
          same(cpu.readContext(), oldContext, 'old task and caches');
          same(cpu.readTimeState(), oldTime, 'no early time charge');
          same(cpu.readPhysicalMemory(TSS_A, 3 * 104), save, 'no early TSS publication');
          cpu.writePhysicalMemory(bAccess, Uint8Array.of(0x89));
          const late = await cpu.run({ budget: 2n, repeatBudget: 7n });
          equal(late.reason, 'protection-fault', 'bad incoming DS');
          equal(late.protectionFault?.vector, 10, 'late #TS');
          equal(late.protectionFault?.errorCode, 0x38, 'invalid DS selector');
          equal(late.retired, 1n, 'switching CALL commits once');
          equal(late.remaining, 1n, 'one ordinary instruction budget remains');
          equal(late.remainingRepeatBudget, 7n, 'no REP work');
          equal(late.state.rip, ENTRY_B, 'fault belongs to incoming B');
          equal(late.state.registers[0], 0x55667788n, 'incoming live EAX');
          equal(late.state.registers[4], 0x98000n, 'incoming live ESP');
          equal(cpu.readContext().tr.selector, 0x20, 'B current');
          equal(cpu.readContext().segments.ds.selector, 0x38, 'invalid incoming selector visible');
          equal(cpu.readContext().segments.ds.usable, false, 'invalid incoming DS cache unusable');
          equal(cpu.readPagingState().cr3, PD_B, 'incoming page directory');
          equal(cpu.readTimeState().tsc, oldTime.tsc + 1n, 'one architectural tick');
          equal(dword(cpu, TSS_A + 40n), 0x11223344, 'dirty A.EAX saved');
          equal(dword(cpu, TSS_A + 32n), Number(RESUME_A), 'resume after the direct CALL');
          same(ports, markers('S'), 'B has executed no guest instruction');
        });
      },
    },
    {
      name: 'nested task error delivery consumes its own budget and returns through both resident tasks',
      async run() {
        await withGuest(compiler, async (cpu, ports) => {
          await toCaller(cpu);
          const staged = await cpu.run({
            budget: 2n,
            repeatBudget: 0n,
            eventDelivery: { budget: 0n },
          });
          equal(staged.reason, 'event-budget', 'incoming fault retained');
          equal(staged.retired, 1n, 'one charged CALL');
          equal(staged.deliveryAttempts, 0n, 'no delivery allowance');
          check(staged.state.event.kind === 'pending', 'pending incoming #TS');
          equal(staged.state.event.exception.vector, 10, 'retained #TS');
          equal(staged.state.event.exception.errorCode, 0x38, 'retained error code');
          equal(staged.state.event.exception.returnPc, ENTRY_B, 'new task EIP for delivery');
          const c = await cpu.run({ budget: 1n, stop: ENTRY_C, eventDelivery: { budget: 1n } });
          equal(c.reason, 'stop', 'stopped before first handler instruction');
          equal(c.retired, 0n, 'delivery executes no guest instruction');
          equal(c.remainingDeliveryBudget, 0n, 'task-gate delivery charged once');
          equal(c.deliveryAttempts, 1n, 'one delivered pending event');
          equal(c.state.registers[4], 0x9bffcn, 'C error code stack address');
          equal(dword(cpu, 0x9bffcn), 0x38, 'error pushed to C stack');
          equal(
            dword(cpu, TSS_B + 32n),
            Number(ENTRY_B),
            'B saved incoming EIP on transition to C',
          );
          equal(cpu.readPhysicalMemory(TSS_B, 1)[0], 0x18, 'B backlink A');
          equal(cpu.readPhysicalMemory(TSS_C, 1)[0], 0x20, 'C backlink B');
          const repaired = await cpu.run({ budget: 300n, stop: IRET_C });
          equal(repaired.reason, 'stop', 'C repaired DS');
          equal(dword(cpu, TSS_B + 84n) & 0xffff, 0x10, 'repaired TSS word');
          const returnedB = await cpu.run({ budget: 1n });
          equal(returnedB.reason, 'budget', 'only nested IRET executed');
          equal(returnedB.state.rip, ENTRY_B, 'resumed B.EIP');
          equal(returnedB.retired, 1n, 'one IRET charge');
          equal(cpu.readContext().tr.selector, 0x20, 'B restored');
          const b = await cpu.run({ budget: 300n, stop: IRET_B });
          equal(b.reason, 'stop', 'B reached second IRET');
          same(ports, markers('SCRB'), 'no duplicate markers on resume');
          // A and C previously executed this physical page. Change the immediate while B is live.
          cpu.writePhysicalMemory(PAGE_A + 1n, Uint8Array.of(0xef, 0xbe, 0xad, 0xde));
          const returnedA = await cpu.run({ budget: 1n });
          equal(returnedA.state.rip, RESUME_A, 'resumed outgoing A');
          equal(returnedA.state.registers[0], 0x11223344n, 'restored dirty outgoing EAX');
          const observed = await cpu.run({ budget: 300n, stop: 0x1001a6n });
          equal(observed.reason, 'stop', 'A re-executed its alias');
          equal(observed.state.registers[0], 0xdeadbeefn, 'changed dormant code fetched under A');
          equal(dword(cpu, PAGE_B + 1n), 0xb1b2b3b4, 'B code insulated from A alias write');
        });
      },
    },
  ];
}
