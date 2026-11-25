import { readFile } from 'node:fs/promises';
import test from 'node:test';
import assert from 'node:assert/strict';
import { Worker } from 'node:worker_threads';
import type { loadParallelCompiler } from '../../sdk/src/machine.js';
import { supportsParallelMemory64 } from '../../sdk/src/parallel/memory/ram.js';
import type { ToWorker } from '../../sdk/src/parallel/protocol.js';
import { runParallelScenarios } from '../execution/parallel/index.js';
import {
  RAM,
  isParallelMemory64,
  linearMachine,
  withMachine,
  type MachineFactory,
} from '../execution/parallel/fixture.js';
import { checkParallelMemory64 } from '../execution/parallel/memory64.js';

const entry = new URL('../../build/host/machine.js', import.meta.url);
const selected = process.env.RING64_PARALLEL_SCENARIO;

function sharedMemoryImport(memory64: boolean): WebAssembly.Module {
  const header = [0, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
  const name = [3, 0x65, 0x6e, 0x76, 3, 0x72, 0x61, 0x6d]; // "env" "ram"
  const limits = [memory64 ? 7 : 3, 1, 0x80, 0x80, memory64 ? 16 : 4]; // 1 page to 16 or 4 GiB
  return new WebAssembly.Module(Uint8Array.from([...header, 2, 15, 1, ...name, 2, ...limits]));
}

test('parallel public API runs real dedicated Node workers', { timeout: 120_000 }, async () => {
  const { createMachine }: { createMachine: MachineFactory } = await import(entry.href);
  const bytes = await readFile(new URL('../../build/ring64-parallel.wasm', import.meta.url));
  const result = await runParallelScenarios(createMachine, Uint8Array.from(bytes), selected).catch(
    (error: unknown) => {
      if (selected && error instanceof Error && error.cause !== undefined) throw error.cause;
      throw error;
    },
  );
  if (selected) assert.deepEqual(result.passed, [selected]);
  assert.ok(result.passed.length > 0);
  assert.equal(new Set(result.passed).size, result.passed.length);
});

test(
  'parallel selects an available profile and uses the narrowest shared RAM that fits',
  { timeout: 120_000, skip: selected !== undefined },
  async (t) => {
    const api: {
      createMachine: MachineFactory;
      loadParallelCompiler: typeof loadParallelCompiler;
    } = await import(entry.href);
    const supported = supportsParallelMemory64();
    const compiler = await api.loadParallelCompiler();
    assert.equal(isParallelMemory64(compiler), supported, 'default parallel profile');
    assert.equal(isParallelMemory64(await api.loadParallelCompiler('memory32')), false);
    if (!supported) {
      assert.notEqual(process.env.RING64_REQUIRE_PARALLEL_MEMORY64, '1');
      await assert.rejects(api.loadParallelCompiler('memory64'), { code: 'unsupported' });
      return;
    }
    const explicit = await api.loadParallelCompiler('memory64');
    assert.equal(isParallelMemory64(explicit), true, 'explicit shared memory64 profile');

    const post = Worker.prototype.postMessage;
    let shared: WebAssembly.Memory | undefined;
    t.mock.method(
      Worker.prototype,
      'postMessage',
      function (this: Worker, ...args: Parameters<Worker['postMessage']>) {
        const message = args[0] as ToWorker;
        if (message.kind === 'start') shared ??= message.shared;
        return post.apply(this, args);
      },
    );
    const small = linearMachine(compiler, '90');
    small.memory.backings[1] = { id: 'data', bytes: Uint8Array.of(42), byteLength: 4096 };
    await withMachine(api.createMachine, small, async (machine) => {
      assert.deepEqual(await machine.readMemory(RAM, 2), Uint8Array.of(42, 0));
    });
    assert.ok(shared, 'small machine started a worker');
    assert.doesNotThrow(
      () => new WebAssembly.Instance(sharedMemoryImport(false), { env: { ram: shared! } }),
      'small machine shared RAM fits a memory32 import',
    );
    const result = await runParallelScenarios(api.createMachine, explicit);
    assert.ok(result.passed.length > 0);

    shared = undefined;
    await checkParallelMemory64(api.createMachine, compiler);
    assert.ok(shared, 'large machine started a worker');
    assert.doesNotThrow(
      () => new WebAssembly.Instance(sharedMemoryImport(true), { env: { ram: shared! } }),
      'large machine shared RAM fits a memory64 import',
    );
  },
);
