import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { BANK, PC, initial, little, usingPaged } from '../execution/memory/paging/fixture.js';

test(
  'FS and GS widths match an OS-enabled x86-64 process on the identical instruction bytes',
  {
    skip: process.platform !== 'linux' || process.arch !== 'x64',
  },
  async (t) => {
    const directory = await mkdtemp(join(tmpdir(), 'ring64-segment-bases-'));
    try {
      const executable = join(directory, 'probe');
      execFileSync('cc', [
        '-nostdlib',
        '-static',
        '-o',
        executable,
        fileURLToPath(new URL('./segment-bases.S', import.meta.url)),
      ]);
      const native = spawnSync(executable, [], { timeout: 10000 });
      if (native.error) throw native.error;
      if (native.status === 77) return t.skip('CPUID or Linux AT_HWCAP2 does not enable FSGSBASE');
      assert.equal(native.signal, null, 'native probe received a signal');
      assert.equal(native.status, 0);
      const expected = [
        0x6001abcde123n,
        0x50024321fedcn,
        0xabcde123n,
        0x4321fedcn,
        0x4321fedcn,
        0x4321fedcn,
        0x4321fedcn,
        0xabcde123n,
      ];
      const resultBytes = native.stdout.subarray(0, 64);
      const packet = native.stdout.subarray(64);
      assert.deepEqual(resultBytes, Buffer.from(little(expected)));
      assert.ok(packet.length > 0, 'native instruction bytes');
      const compiler = await WebAssembly.compile(
        new Uint8Array(await readFile(new URL('../../build/ring64.wasm', import.meta.url))),
      );
      const data = new Uint8Array(512).fill(0xa5);
      const stop = PC + 0x2000n;
      data.set(little([stop]), 256);
      await usingPaged(
        compiler,
        {
          code: packet.toString('hex'),
          bank: { ...BANK, cr4: BANK.cr4 | (1n << 16n) },
          words: [[0x4008n, 0xa007n]],
          spans: [{ physical: 0xa000n, bytes: data }],
        },
        async (cpu) => {
          const context = cpu.readContext();
          context.segments.fs.base = 0x12340000n;
          context.segments.gs.base = 0x43210000n;
          cpu.setContext(context);
          cpu.setState(initial(PC, { 4: PC + 0x1100n, 7: PC + 0x1010n }, 0x8d7n));
          const result = await cpu.run({ budget: 64n, stop });
          assert.equal(result.reason, 'stop');
          assert.equal(result.state.registers[4], PC + 0x1108n);
          assert.equal(result.state.flags, 0x8d7n);
          assert.deepEqual(cpu.readContext(), context);
          data.set(resultBytes, 16);
          assert.deepEqual(cpu.readPhysicalMemory(0xa000n, data.length), data);
        },
      );
      t.diagnostic(
        'native FSGSBASE compared eight values across four instructions and both operand widths',
      );
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);
