import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu } from '../../../../src/index.js';
import { Core } from '../../../../src/wasm/core.js';
import { protectedSnapshot } from '../../../../../tests/support/context.js';
import { LIMITS, hex, initial } from '../../../../../tests/support/fixture.js';
import { compiler, head, mapping, readOnly } from './fixture.js';

test('repeated cached flat loop owners link and respect stop and remap boundaries', async () => {
  const codeMap = (backing: string) => ({
    ...mapping(head, backing, 4),
    permissions: { ...readOnly, execute: true },
  });
  // DEC ECX or EDX; JNZ head; HLT. A one-instruction cap forces two owners.
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: { ...LIMITS, maxInstructions: 1, maxTranslationsPerRun: 4 },
    context: protectedSnapshot(),
    memory: {
      backings: [
        { id: 'ecx', bytes: hex('4975fdf4') },
        { id: 'edx', bytes: hex('4a75fdf4') },
      ],
      mappings: [codeMap('ecx')],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 1, maxBackingBytes: 8 },
  });
  const execute = Core.prototype.execute;
  let physicalEntries = 0;
  Core.prototype.execute = function (...args) {
    physicalEntries++;
    return execute.apply(this, args);
  };
  const run = async (stop = head + 3n) => {
    physicalEntries = 0;
    cpu.setState(initial(head, { 1: 192n, 2: 192n }));
    return cpu.run({ budget: 512n, stop });
  };
  try {
    const first = await run();
    assert.equal(first.resources.cachedArtifacts, 2);
    const profiled = await run();
    assert.equal(profiled.resources.cachedArtifacts, 3);
    const linked = await run();
    assert.equal(linked.reason, 'stop');
    assert.equal(linked.retired, 384n);
    assert.equal(linked.state.registers[1], 0n);
    assert.equal(linked.resources.nativeEntries, 384);
    assert.equal(linked.resources.dispatches, 383);
    assert.equal(linked.resources.cacheHits, 384);
    assert.ok(physicalEntries < linked.resources.nativeEntries);

    const stopped = await run(head + 1n);
    assert.equal(stopped.reason, 'stop');
    assert.equal(stopped.retired, 1n);
    assert.equal(stopped.state.registers[1], 191n);
    assert.equal(physicalEntries, 1);

    cpu.setMappings([codeMap('edx')]);
    const remapped = await run();
    assert.equal(remapped.reason, 'stop');
    assert.equal(remapped.retired, 384n);
    assert.equal(remapped.state.registers[1], 192n);
    assert.equal(remapped.state.registers[2], 0n);
    assert.equal(remapped.resources.cachedArtifacts, 2);
  } finally {
    Core.prototype.execute = execute;
    cpu.dispose();
  }
});
