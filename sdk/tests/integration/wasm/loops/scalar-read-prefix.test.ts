import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { snapshot } from '../../../../../tests/execution/far-control/fixture.js';
import {
  BANK,
  PC,
  initial,
  little,
  usingPaged,
} from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('sampled quiet completion keeps branch, single-step, and execute breakpoint handling', async () => {
  // ADD RAX,1; JC first increment; JMP second increment; INC RAX; INC RAX; HLT.
  const code = '4883c0017202eb0348ffc048ffc0f4';
  const firstIncrement = PC + 8n;
  const secondIncrement = PC + 11n;
  const stop = PC + 14n;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const compile = Core.prototype.compile;
  let sampled = false;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (sampled && pc === PC && !seeds?.length)
      return compile.call(
        this,
        pc,
        limits,
        [PC, PC + 4n, PC + 6n, firstIncrement, secondIncrement, stop],
        true,
      );
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  let finishes = 0;
  let shortPrefixes = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const guard = env.paged_loop_guard;
    if (typeof guard === 'function') {
      env.paged_loop_guard = (pc: bigint, bytes: number, flags: bigint, shadow: number) => {
        const result = guard(pc, bytes, flags, shadow);
        if (pc === PC && bytes === 6 && result === 1) shortPrefixes++;
        return result;
      };
    }
    const finish = env.debug_finish;
    if (typeof finish === 'function') {
      env.debug_finish = (...args: unknown[]) => {
        finishes++;
        return finish(...args);
      };
    }
    return env;
  };
  try {
    for (sampled of [false, true]) {
      await usingPaged(compiler, { code }, async (cpu) => {
        const enter = (rax = 1n, flags = 2n, rip = PC) =>
          cpu.setState(initial(rip, { 0: rax }, flags));
        for (let wave = 0; wave < 3; wave++) {
          finishes = 0;
          shortPrefixes = 0;
          for (const [rax, expected] of [
            [1n, 3n],
            [0xffff_ffff_ffff_ffffn, 2n],
          ] as const) {
            enter(rax);
            const result = await cpu.run({ stop, budget: 10n });
            assert.equal(result.reason, 'stop');
            assert.equal(result.retired, 4n);
            assert.equal(result.state.registers[0], expected);
          }
        }
        const ordinaryFinishes = sampled ? 0 : 8;
        assert.equal(finishes, ordinaryFinishes);
        assert.equal(shortPrefixes, sampled ? 2 : 0);

        enter(1n, 0x103n, PC + 4n);
        const step = await cpu.run({ stop, budget: 10n });
        assert.equal(step.reason, 'debug-exception');
        assert.equal(step.retired, 1n);
        assert.equal(step.state.rip, firstIncrement);
        assert.equal(finishes, ordinaryFinishes + 1);

        cpu.setDebugState({ addresses: [secondIncrement, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
        enter();
        const breakpoint = await cpu.run({ stop, budget: 10n });
        assert.equal(breakpoint.reason, 'debug-exception');
        assert.equal(breakpoint.retired, 3n);
        assert.equal(breakpoint.state.rip, secondIncrement);
        assert.equal(finishes, ordinaryFinishes + 4);
      });
    }
  } finally {
    prototype.artifactImports = imports;
    Core.prototype.compile = compile;
  }
});

test('sampled direct jumps carry quiet fetch only within a native entry', async () => {
  // JMP second; NOP; NOP; JMP stop; HLT.
  const second = PC + 4n;
  const stop = PC + 6n;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const compile = Core.prototype.compile;
  const begins: bigint[] = [];
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length)
      return compile.call(this, pc, limits, [PC, second, stop], true);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, flags: bigint, shadow: number) => {
        begins.push(pc);
        return begin(pc, flags, shadow);
      };
    }
    return env;
  };
  try {
    await usingPaged(compiler, { code: 'eb029090eb00f4' }, async (cpu) => {
      for (let wave = 0; wave < 3; wave++) {
        begins.length = 0;
        cpu.setState(initial(PC));
        const result = await cpu.run({ stop, budget: 10n });
        assert.equal(result.reason, 'stop');
        assert.equal(result.retired, 2n);
      }
      assert.deepEqual(begins, [PC]);
      begins.length = 0;
      cpu.setState(initial(second));
      assert.equal((await cpu.run({ stop, budget: 10n })).retired, 1n);
      assert.deepEqual(begins, [second]);
      cpu.setDebugState({ addresses: [second, 0n, 0n, 0n], dr6: 0n, dr7: 1n });
      cpu.setState(initial(PC));
      const watched = await cpu.run({ stop, budget: 10n });
      assert.equal(watched.reason, 'debug-exception');
      assert.equal(watched.retired, 1n);
      assert.equal(watched.state.rip, second);
    });
  } finally {
    prototype.artifactImports = imports;
    Core.prototype.compile = compile;
  }
});

test('a taken same-page prefix branch carries only the immediately preceding fetch', async () => {
  // Register prefix, CALL, and the INC/CMP/JB prefix which returns to the first prefix.
  const code = '8b0783c00131d285c0e807000000ffc53b2e72ecf4c3';
  const stop = PC + 20n;
  const tail = PC + 14n;
  const source = PC + 0x1000n;
  const data = new Uint8Array(4096);
  data.set([13, 0, 0, 0, 64]);
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let headGuards = 0;
  let tailGuards = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const guard = env.paged_loop_guard;
    if (typeof guard === 'function') {
      env.paged_loop_guard = (pc: bigint, bytes: number, flags: bigint, shadow: number) => {
        if (pc === PC) headGuards++;
        if (pc === tail) tailGuards++;
        return guard(pc, bytes, flags, shadow);
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [
          [0x4008n, 0xa007n],
          [0x4010n, 0xb007n],
        ],
        spans: [
          { physical: 0xa000n, bytes: data },
          { physical: 0xb000n, bytes: new Uint8Array(4096) },
        ],
      },
      async (cpu) => {
        const enter = (rip = PC, read = source) =>
          cpu.setState(initial(rip, { 4: PC + 0x3000n, 5: 0n, 6: source + 4n, 7: read }));
        for (let wave = 0; wave < 3; wave++) {
          headGuards = tailGuards = 0;
          enter();
          const result = await cpu.run({ stop, budget: 1000n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 576n);
          assert.equal(result.state.registers[0], 14n);
        }
        assert.ok(tailGuards > 50);
        assert.ok(headGuards < 4, 'the taken branches should carry the tail fetch');

        enter();
        const short = await cpu.run({ stop, budget: 10n });
        assert.equal(short.reason, 'budget');
        assert.equal(short.retired, 10n);
        assert.equal(short.state.rip, PC + 2n);

        enter(tail);
        const interior = await cpu.run({ stop: PC + 2n, budget: 10n });
        assert.equal(interior.reason, 'stop');
        assert.equal(interior.retired, 4n);
        assert.equal(interior.state.registers[0], 13n);

        enter(tail, PC + 0x4000n);
        const fault = await cpu.run({ stop, budget: 10n });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.retired, 3n);
        assert.equal(fault.state.rip, PC);

        enter(tail);
        cpu.setDebugState({ addresses: [PC, 0n, 0n, 0n], dr6: 0n, dr7: 1n });
        const debug = await cpu.run({ stop, budget: 10n });
        assert.equal(debug.reason, 'debug-exception');
        assert.equal(debug.retired, 3n);
        assert.equal(debug.state.rip, PC);

        cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr6: 0n, dr7: 0n });
        cpu.writePhysicalMemory(0x9004n, Uint8Array.of(2));
        enter(tail);
        const modified = await cpu.run({ stop, budget: 1000n });
        assert.equal(modified.reason, 'stop');
        assert.equal(modified.state.registers[0], 15n);
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});

test('guarded scalar read prefixes preserve stop, single-step, and split-page fault ordering', async () => {
  const code = '0fb70783c00331c983c001'; // MOVZX EAX,word [RDI]; ADD EAX,3; XOR ECX,ECX; ADD EAX,1
  const stop = PC + BigInt(code.length / 2);
  const source = PC + 0x1001n;
  const data = new Uint8Array(4096);
  data.set([0x65, 0x87], 1);
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let accepted = 0;
  let refused = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const read = env.paged_loop_read_span;
    if (typeof read === 'function') {
      env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
        const result = read(address, width, segment);
        if (result) accepted++;
        else refused++;
        return result;
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      { code, words: [[0x4008n, 0xa003n]], spans: [{ physical: 0xa000n, bytes: data }] },
      async (cpu) => {
        const enter = (address = source, flags = 2n) =>
          cpu.setState(initial(PC, { 0: 0x1234n, 7: address }, flags));
        for (let wave = 0; wave < 3; wave++) {
          enter();
          const completed = await cpu.run({ budget: 10n, stop });
          assert.equal(completed.reason, 'stop');
          assert.equal(completed.retired, 4n);
          assert.equal(completed.state.registers[0], 0x8769n);
        }
        assert.ok(accepted > 0, 'the ordinary unaligned supervisor read uses a retained span');
        const beforeStop = accepted;
        enter();
        const interior = await cpu.run({ budget: 10n, stop: PC + 3n });
        assert.equal(interior.retired, 1n);
        assert.equal(interior.state.registers[0], 0x8765n);
        assert.equal(accepted, beforeStop);

        enter(source, 0x102n);
        const step = await cpu.run({ budget: 10n, stop });
        assert.equal(step.reason, 'debug-exception');
        assert.equal(step.retired, 1n);
        assert.equal(step.state.registers[0], 0x8765n);
        assert.equal(accepted, beforeStop);

        const beforeSplit = refused;
        enter(PC + 0x1fffn);
        const split = await cpu.run({ budget: 10n, stop });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 0n);
        assert.equal(split.state.rip, PC);
        assert.equal(split.state.registers[0], 0x1234n);
        assert.equal(refused, beforeSplit + 1);
      },
    );
    await usingPaged(
      compiler,
      {
        code,
        context: snapshot('long64', 3),
        bank: { ...BANK, cr0: BANK.cr0 | 0x40000n },
        words: [[0x4008n, 0xa007n]],
        spans: [{ physical: 0xa000n, bytes: data }],
      },
      async (cpu) => {
        cpu.setState(initial(PC, { 7: source + 1n }, 0x40002n));
        assert.equal((await cpu.run({ budget: 10n, stop })).reason, 'stop');
        const beforeAlignment = accepted + refused;
        cpu.setState(initial(PC, { 0: 0x1234n, 7: source }, 0x40002n));
        const alignment = await cpu.run({ budget: 10n, stop });
        assert.equal(alignment.reason, 'alignment-check');
        assert.equal(alignment.retired, 0n);
        assert.equal(alignment.state.rip, PC);
        assert.equal(alignment.state.registers[0], 0x1234n);
        assert.equal(accepted + refused, beforeAlignment);
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});

test('guarded MOVZX prefixes reuse live read translations across entries and revoke on host remap', async () => {
  const source = PC + 0x1001n;
  const stop = PC + 11n;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const compile = Core.prototype.compile;
  let readCalls = 0;
  let acceptedGuards = 0;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length)
      return compile.call(this, pc, limits, [PC, PC + 3n, PC + 6n, PC + 8n, stop], true);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const read = env.paged_loop_read_span;
    if (typeof read === 'function') {
      env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
        readCalls++;
        return read(address, width, segment);
      };
    }
    const guard = env.paged_loop_guard;
    if (typeof guard === 'function') {
      env.paged_loop_guard = (pc: bigint, bytes: number, flags: bigint, shadow: number) => {
        const proof = guard(pc, bytes, flags, shadow);
        if (pc === PC && proof === 1) acceptedGuards++;
        return proof;
      };
    }
    return env;
  };

  const original = new Uint8Array(4096);
  original.set([0x65, 0x87], 1);
  const replacement = new Uint8Array(4096);
  replacement.set([0x34, 0x12], 1);
  try {
    for (const [opcode, initialValue, remappedValue] of [
      ['b6', 0x65n, 0x34n],
      ['b7', 0x8765n, 0x1234n],
    ] as const) {
      await usingPaged(
        compiler,
        {
          // MOVZX EAX,[RDI]; ADD EAX,3; XOR ECX,ECX; ADD EAX,1; JMP back to MOVZX.
          code: `0f${opcode}0783c00331c983c001ebf3`,
          words: [[0x4008n, 0xa003n]],
          spans: [
            { physical: 0xa000n, bytes: original },
            { physical: 0xb000n, bytes: replacement },
          ],
        },
        async (cpu) => {
          const enter = (address = source, flags = 2n) =>
            cpu.setState(initial(PC, { 0: 0x4321n, 7: address }, flags));
          const prefix = async (value: bigint) => {
            const result = await cpu.run({ stop, budget: 10n });
            assert.equal(result.reason, 'stop');
            assert.equal(result.retired, 4n);
            assert.equal(result.state.rip, stop);
            assert.equal(result.state.registers[0], value + 4n);
          };
          const rewind = async () => {
            const back = await cpu.run({ stop: PC, budget: 10n });
            assert.equal(back.reason, 'stop');
            assert.equal(back.retired, 1n);
          };

          enter();
          await prefix(initialValue);
          await rewind();
          await prefix(initialValue);
          await rewind();
          const beforeWarm = readCalls;
          const beforeGuards = acceptedGuards;
          for (let wave = 0; wave < 3; wave++) {
            await prefix(initialValue);
            await rewind();
          }
          assert.equal(acceptedGuards, beforeGuards + 3);
          assert.equal(readCalls, beforeWarm, 'warm guarded reads bypass the translation helper');

          cpu.writePhysicalMemory(0x4008n, little([0xb003n]));
          await prefix(remappedValue);
          await rewind();
          await prefix(remappedValue);
          await rewind();
          const afterRemap = readCalls;
          const beforeRemappedGuard = acceptedGuards;
          await prefix(remappedValue);
          assert.equal(acceptedGuards, beforeRemappedGuard + 1);
          assert.equal(readCalls, afterRemap);
          await rewind();

          const interior = await cpu.run({ stop: PC + 3n, budget: 10n });
          assert.equal(interior.reason, 'stop');
          assert.equal(interior.retired, 1n);
          assert.equal(interior.state.rip, PC + 3n);
          assert.equal(interior.state.registers[0], remappedValue);
          assert.equal(readCalls, afterRemap);

          enter(source, 0x102n);
          const step = await cpu.run({ stop, budget: 10n });
          assert.equal(step.reason, 'debug-exception');
          assert.equal(step.retired, 1n);
          assert.equal(step.state.rip, PC + 3n);
          assert.equal(step.state.registers[0], remappedValue);
          assert.equal(readCalls, afterRemap);

          if (opcode === 'b7') {
            enter(PC + 0x1fffn);
            const split = await cpu.run({ stop, budget: 10n });
            assert.equal(split.reason, 'page-fault');
            assert.equal(split.retired, 0n);
            assert.equal(split.state.rip, PC);
            assert.equal(split.state.registers[0], 0x4321n);
            assert.equal(split.pageFault?.address, PC + 0x2000n);
            assert.equal(readCalls, afterRemap + 1);
          }

          cpu.setDebugState({ addresses: [source, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
          enter();
          const watch = await cpu.run({ stop, budget: 10n });
          assert.equal(watch.reason, 'debug-exception');
          assert.equal(watch.debugException?.cause, 'data-breakpoint');
          assert.equal(watch.retired, 1n);
          assert.equal(watch.state.rip, PC + 3n);
          assert.equal(watch.state.registers[0], remappedValue);
        },
      );
    }

    const largePage = PC + 0x200000n;
    await usingPaged(
      compiler,
      {
        code: '0fb70783c00331c983c001',
        words: [[0x3018n, 0x200083n]],
        spans: [
          { physical: 0x200000n, bytes: original },
          { physical: 0x201000n, bytes: replacement, read: false },
        ],
      },
      async (cpu) => {
        for (let wave = 0; wave < 3; wave++) {
          cpu.setState(initial(PC, { 7: largePage + 1n }));
          const ordinary = await cpu.run({ stop, budget: 10n });
          assert.equal(ordinary.reason, 'stop');
          assert.equal(ordinary.retired, 4n);
          assert.equal(ordinary.state.registers[0], 0x8769n);
        }
        const beforeGuard = acceptedGuards;
        cpu.setState(initial(PC, { 0: 0x4321n, 7: largePage + 0xfffn }));
        const split = await cpu.run({ stop, budget: 10n });
        assert.equal(acceptedGuards, beforeGuard + 1);
        assert.equal(split.reason, 'physical-memory-fault');
        assert.equal(split.physicalMemoryFault?.phase, 'final-access');
        assert.equal(split.retired, 0n);
        assert.equal(split.state.rip, PC);
        assert.equal(split.state.registers[0], 0x4321n);
      },
    );
  } finally {
    prototype.artifactImports = imports;
    Core.prototype.compile = compile;
  }
});

test('sampled stack comparisons retire with their branch and replay an unavailable operand precisely', async () => {
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const compile = Core.prototype.compile;
  const source = PC + 0x1000n;
  const data = new Uint8Array(4096);
  data[0] = 16;
  let accepted = 0;
  let refused = 0;
  let corridor: bigint[];
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, corridor, true);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const read = env.paged_loop_read_span;
    if (typeof read === 'function') {
      env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
        const result = read(address, width, segment);
        if (width === 8) {
          if (result) accepted++;
          else refused++;
        }
        return result;
      };
    }
    return env;
  };
  try {
    for (const add of ['', '4883c201']) {
      // Optional ADD RDX,1; CMP RAX,[RSP]; JB stop; INC RCX; HLT.
      const code = add + '483b0424720348ffc1f4';
      const compare = PC + BigInt(add.length / 2);
      const branch = compare + 4n;
      const preceding = add ? 1n : 0n;
      const stop = compare + 9n;
      corridor = [...new Set([PC, compare, branch, branch + 2n, stop])];
      await usingPaged(
        compiler,
        { code, words: [[0x4008n, 0xa003n]], spans: [{ physical: 0xa000n, bytes: data }] },
        async (cpu) => {
          const enter = (rax: bigint, rsp = source) =>
            cpu.setState(initial(PC, { 0: rax, 1: 5n, 2: 8n, 4: rsp }));
          const before = accepted;
          for (let wave = 0; wave < 3; wave++) {
            for (const rax of [1n, 32n]) {
              enter(rax);
              const outcome = await cpu.run({ budget: 12n, stop });
              const taken = rax === 1n;
              assert.equal(outcome.reason, 'stop');
              assert.equal(outcome.retired, preceding + (taken ? 2n : 3n));
              assert.equal(outcome.state.registers[1], taken ? 5n : 6n);
              assert.equal(outcome.state.flags & 1n, taken ? 1n : 0n);
            }
          }
          assert.ok(accepted > before, 'the checked comparison should use a retained stack span');

          enter(1n);
          const partial = await cpu.run({ budget: preceding + 1n, stop });
          assert.equal(partial.reason, 'budget');
          assert.equal(partial.retired, preceding + 1n);
          assert.equal(partial.state.rip, branch);

          enter(1n);
          const interior = await cpu.run({ budget: 12n, stop: branch });
          assert.equal(interior.reason, 'stop');
          assert.equal(interior.retired, preceding + 1n);
          assert.equal(interior.state.rip, branch);

          const beforeSplit = refused;
          enter(1n, source + 4092n);
          const split = await cpu.run({ budget: 12n, stop });
          assert.equal(split.reason, 'page-fault');
          assert.equal(split.retired, preceding);
          assert.equal(split.state.rip, compare);
          assert.equal(split.state.registers[2], 8n + preceding);
          assert.equal(refused, beforeSplit + 1);
        },
      );
    }
  } finally {
    prototype.artifactImports = imports;
    Core.prototype.compile = compile;
  }
});
