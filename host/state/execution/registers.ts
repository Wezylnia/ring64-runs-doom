import { unsigned64 } from '../../embedding/input.js';
import { captureDebugContinuation, type DebugContinuation } from './debug.js';
import {
  captureEventState,
  captureInterruptState,
  type EventState,
  type InterruptState,
} from './events.js';

export interface State {
  debugContinuation?: DebugContinuation;
  registers: bigint[];
  flags: bigint;
  rip: bigint;
  interruptibility?: Interruptibility;
  event?: EventState;
  interrupts?: InterruptState;
}

export interface Interruptibility {
  sti: boolean;
  ss: boolean;
}

export interface StateSnapshot extends State {
  /** Private memory repair must finish on the CPU that captured this state. */
  taskSwitchIncomplete?: true;
  interruptibility: Interruptibility;
  event: EventState;
  interrupts: InterruptState;
}

export function captureState(state: State): StateSnapshot {
  if ('taskSwitchIncomplete' in state)
    throw new TypeError('incomplete task switch requires its original CPU');
  const source = state.registers;
  if (!Array.isArray(source) || source.length !== 16)
    throw new TypeError('state requires sixteen registers');
  const registers: bigint[] = [];
  for (let index = 0; index < 16; index++) {
    const register = source[index];
    unsigned64(register, 'register');
    registers.push(register);
  }
  const flags = state.flags;
  unsigned64(flags, 'flags');
  const rip = state.rip;
  unsigned64(rip, 'rip');
  const boundary = state.interruptibility;
  let interruptibility: Interruptibility = { sti: false, ss: false };
  if (boundary !== undefined) {
    if (boundary === null || typeof boundary !== 'object' || Array.isArray(boundary))
      throw new TypeError('interruptibility must contain boolean sti and ss');
    const sti = boundary.sti;
    const ss = boundary.ss;
    if (typeof sti !== 'boolean' || typeof ss !== 'boolean')
      throw new TypeError('interruptibility must contain boolean sti and ss');
    interruptibility = { sti, ss };
  }
  const event = 'event' in state ? captureEventState(state.event!) : { kind: 'ready' as const };
  const interrupts =
    'interrupts' in state
      ? captureInterruptState(state.interrupts!)
      : { irq: null, nmiPending: false, nmiBlocked: false };
  const debugContinuation =
    'debugContinuation' in state ? captureDebugContinuation(state.debugContinuation!) : undefined;
  return {
    registers,
    flags,
    rip,
    interruptibility,
    event,
    interrupts,
    ...(debugContinuation ? { debugContinuation } : {}),
  };
}

export function captureMxcsr(value: number): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > 0xffff)
    throw new TypeError('MXCSR must be an unsigned 16-bit integer number');
  return value === 0 ? 0 : value;
}

export function captureXmmState(input: bigint[]): bigint[] {
  if (!Array.isArray(input) || input.length !== 16)
    throw new TypeError('XMM state requires sixteen registers');
  const bank: bigint[] = [];
  for (let index = 0; index < 16; index++) {
    const value = input[index];
    if (typeof value !== 'bigint' || value < 0n || value >= 1n << 128n)
      throw new TypeError('XMM register must be an unsigned 128-bit bigint');
    bank.push(value);
  }
  return bank;
}
