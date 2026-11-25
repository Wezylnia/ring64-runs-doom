import {
  ENTRY,
  RAM,
  asBytes,
  cpuState,
  done,
  equal,
  hex,
  linearMachine,
  same,
  span,
  withMachine,
  words,
  type MachineFactory,
} from './fixture.js';

export async function checkParallelMemory64(factory: MachineFactory, compiler: WebAssembly.Module) {
  const boundary = 2 ** 32;
  const high = RAM + 0x1000n;
  const lowCode = RAM + 0x2000n;
  const highCode = RAM + 0x3000n;
  const patch = ENTRY + 4n;
  const start = hex('f048ff03c60702'); // lock inc qword [rbx]; mov byte [rdi],2
  const target = hex('b801000000'); // mov eax,1
  const writerStop = ENTRY + BigInt(start.length);
  const spare = RAM + 0x4000n;
  const options = linearMachine(compiler, start);
  options.sharedRamCapacity = boundary + 3 * 4096;
  options.memoryLimits.maxBackingBytes = boundary + 4096 + start.length + 32;
  options.memory.backings[1] = {
    id: 'data',
    bytes: new Uint8Array(0),
    byteLength: boundary + 4096,
  };
  options.memory.mappings = [
    options.memory.mappings[0]!,
    span(RAM, 'data', 0, 32),
    span(high, 'data', boundary, 32),
    span(lowCode, 'data', 16, target.length, 'rx'),
    span(highCode, 'data', boundary + 16, target.length, 'rx'),
  ];

  await withMachine(factory, options, async (machine) => {
    await machine.writeMemory(RAM, asBytes(41n));
    await machine.writeMemory(high, asBytes(7n));
    await Promise.all(machine.cpus.map((cpu) => cpu.setState(cpuState(ENTRY, { 3: high }))));
    const locked = await Promise.all(
      machine.cpus.map((cpu) => cpu.run({ budget: 2n, stop: patch })),
    );
    for (const result of locked) done(result, patch, 1n);
    same(await words(machine, high, 1), [9n], 'both workers commit to the high qword');
    same(await words(machine, RAM, 1), [41n], 'matching low address is untouched');

    await machine.writeMemory(RAM + 16n, target);
    await machine.writeMemory(high + 16n, target);
    const [reader, writer] = machine.cpus;
    const runTarget = async (entry: bigint, expected: bigint) => {
      const stop = entry + BigInt(target.length);
      await reader!.setState(cpuState(entry));
      const result = await reader!.run({ budget: 2n, stop });
      done(result, stop, 1n);
      equal(result.state.registers[0], expected, 'correct executable alias');
      return result.resources.compilations;
    };
    await runTarget(lowCode, 1n);
    await runTarget(highCode, 1n);
    equal(await runTarget(highCode, 1n), 0, 'high source is already cached');

    await writer!.setState(cpuState(patch, { 7: high + 17n }));
    done(await writer!.run({ budget: 2n, stop: writerStop }), writerStop, 1n);
    await runTarget(highCode, 2n);
    equal(await runTarget(lowCode, 1n), 0, 'patching high source preserves matching low owner');

    const mappings = [...options.memory.mappings, span(spare, 'spare', 0, 32)];
    for (const seed of [0x11, 0x22]) {
      await machine.addBacking({ id: 'spare', bytes: Uint8Array.of(seed), byteLength: 32 });
      await machine.setMappings(mappings);
      same(await words(machine, spare, 2), [BigInt(seed), 0n], 'high backing starts zero-filled');
      await machine.writeMemory(spare + 8n, asBytes(0x1234n));
      await machine.setMappings(options.memory.mappings);
      await machine.removeBacking('spare');
    }
  });
}
