import type { Cpu } from 'ring64';

export function setFirstArgument(cpu: Cpu, value: bigint) {
  const state = cpu.readState();
  state.registers[7] = value; // rdi
  cpu.setState(state);
}
