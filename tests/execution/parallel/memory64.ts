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
  const pauseLoop = hex('c7070100000048ffc0837f040074f7'); // Publish readiness, then increment until released.
  const waitReady = hex('833f0074fb'); // Wait until the other worker publishes readiness.
  const pauseStop = writerStop + BigInt(pauseLoop.length);
  const waitStop = pauseStop + BigInt(waitReady.length);
  const program = Uint8Array.from([...start, ...pauseLoop, ...waitReady]);
  const spare = RAM + 0x4000n;
  const options = linearMachine(compiler, program);
  options.sharedRamCapacity = boundary + 3 * 4096;
  options.memoryLimits.maxBackingBytes = boundary + 4096 + program.length + 32;
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

    const status = RAM + 8n;
    const release = asBytes(1n << 32n);
    await machine.writeMemory(status, release);
    await writer!.setState(cpuState(writerStop, { 7: status }));
    done(await writer!.run({ budget: 8n, stop: pauseStop }), pauseStop, 4n);
    await reader!.setState(cpuState(pauseStop, { 7: status }));
    done(await reader!.run({ budget: 8n, stop: waitStop }), waitStop, 2n);

    await machine.writeMemory(status, asBytes(0n));
    await writer!.setState(cpuState(writerStop, { 7: status }));
    await reader!.setState(cpuState(pauseStop, { 7: status }));
    const budget = 1_000_000_000n;
    const running = writer!.run({ budget, stop: pauseStop });
    done(await reader!.run({ budget: 2_000_000n, stop: waitStop }), waitStop);
    await writer!.pause();
    const paused = await running;
    equal(paused.reason, 'paused', 'worker 1 observes its own pause signal');
    equal(paused.retired > 1n && paused.remaining > 0n, true, 'pause interrupts the native loop');
    equal(paused.remaining, budget - paused.retired, 'pause preserves the remaining budget');
    await machine.writeMemory(status, release);
    done(await writer!.run({ budget: 8n, stop: pauseStop }), pauseStop);

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
