import { LatencyTrace, latencyCursors, latencyEvents } from './parallel-latency-trace.mjs';

const ENTRY = 0x1000n;
const RAM = 0x4000_0000n;
const ALIAS = 0x8000_0000n;
const PRESSURE = 64;
const REP_BYTES = 512;
const BUDGET = 10_000_000_000n;
const COLD_LENGTH = 64;
// R10 points to an isolated RAM marker and R11 holds the admitted run generation.
const ALU = '4d891a48ffc0ebfb'; // marker; inc rax; jmp inc
const REP = '4c89c64c89cfb900020000f3a44d891aebee';
const PORT = 'e780ebfc'; // out 0x80,eax; jmp out
const WATCH = '803701b801000000ebf6'; // xor byte [rdi],1; mov eax,1; jmp xor
const LOCK = '4d891af048ff07ebfa'; // marker; lock inc qword [rdi]; jmp lock
const COLD = '486bc003'; // imul rax,rax,3

function check(condition, message) {
  if (!condition) throw Error(message);
}

function literal(hex) {
  return Uint8Array.from(hex.match(/../g), (byte) => Number.parseInt(byte, 16));
}

function state(rip, values = {}) {
  const registers = Array(16).fill(0n);
  for (const [index, value] of Object.entries(values)) registers[index] = value;
  return { rip, registers, flags: 2n };
}

function span(base, backing, length, execute = false) {
  return {
    base,
    backing,
    offset: 0,
    length,
    permissions: { read: true, write: !execute, execute },
  };
}

function programFor(workload, primary, samples) {
  const code = new Uint8Array(workload === 'cold' ? 4096 + samples * 4096 : 128).fill(0x90);
  code.set(literal(primary));
  if (workload === 'cold') {
    const body = literal(COLD.repeat(COLD_LENGTH) + 'ebfe');
    for (let i = 0; i < samples; i++) code.set(body, 4096 + i * 4096);
  } else {
    const pressure = { alu: ALU, rep: ALU, port: PORT, watch: WATCH, contended: LOCK }[workload];
    code.set(literal(pressure), PRESSURE);
  }
  return code;
}

const nextTask = (() => {
  if (typeof globalThis.setImmediate === 'function')
    return () => new Promise((resolve) => setImmediate(resolve));
  const channel = new MessageChannel();
  let done;
  channel.port1.onmessage = () => done();
  return () =>
    new Promise((resolve) => {
      done = resolve;
      channel.port2.postMessage(0);
    });
})();

async function until(predicate, reason) {
  const started = performance.now();
  while (!predicate()) {
    check(performance.now() - started < 3_000, `missing witness: ${reason}`);
    await nextTask();
  }
}

function timing(values) {
  if (values.length === 0) return { samples: 0 };
  const sorted = values.toSorted((left, right) => left - right);
  check(sorted[0] >= 0, 'negative latency');
  const at = (fraction) => sorted[Math.ceil(sorted.length * fraction) - 1];
  return { samples: sorted.length, p50: at(0.5), p95: at(0.95), p99: at(0.99), max: sorted.at(-1) };
}

function stages(buffer, started, ended) {
  const rows = started.map((first, actor) =>
    new LatencyTrace(buffer, actor).read(first, ended[actor]),
  );
  const durations = (row, kind) =>
    row.filter((entry) => entry.kind === kind).map((entry) => entry.duration);
  const current = (row, kind, identity) =>
    row.find((entry) => entry.kind === kind && entry.identity === identity);
  const phases = rows[0]
    .filter((entry) => entry.kind === latencyEvents.close)
    .map((entry) => {
      const drained = current(rows[0], latencyEvents.drained, entry.identity);
      const opened = current(rows[0], latencyEvents.open, entry.identity);
      check(drained && opened, 'missing quiescence or re-admission');
      return {
        identity: entry.identity,
        start: entry.started,
        drained: drained.started,
        end: opened.started,
      };
    });
  const pauseRequests = rows[0].filter(
    (entry) => entry.kind === latencyEvents.pauseRequest && entry.auxiliary === 0,
  );
  // The worker timestamps before publishing its ack. The coordinator brackets that
  // publication by starting the phase and observing its quorum, all on the host clock.
  const clockIntervals = phases.map((phase) => {
    const ack = current(rows[1], latencyEvents.acknowledgement, phase.identity);
    check(ack, 'missing calibration acknowledgement');
    return [ack.started - phase.drained, ack.started - phase.start];
  });
  // Allow for both readings' exposed timer granularity in cross-origin-isolated Chromium.
  const lower = Math.max(...clockIntervals.map(([bound]) => bound)) - 0.01;
  const upper = Math.min(...clockIntervals.map(([, bound]) => bound)) + 0.01;
  check(lower <= upper, 'inconsistent coordinator and worker clocks');
  const rawPause = pauseRequests.map((entry) => {
    const safe = current(rows[1], latencyEvents.pauseSafe, entry.identity);
    check(safe, 'missing safe point for issued pause');
    return safe.started - entry.started;
  });
  const pauseAt = (offset) => timing(rawPause.map((duration) => Math.max(0, duration - offset)));
  const compilations = rows
    .slice(1)
    .flatMap((row) => row.filter((entry) => entry.kind === latencyEvents.compilation));
  return {
    phaseToAllAcks: timing(phases.map((phase) => phase.drained - phase.start)),
    phaseServiceAfterAcks: timing(phases.map((phase) => phase.end - phase.drained)),
    closedPhase: timing(phases.map((phase) => phase.end - phase.start)),
    pauseRequestToSafe: pauseAt((lower + upper) / 2),
    pauseSafeP99Bounds: [pauseAt(upper).p99, pauseAt(lower).p99],
    workerClockOffsetBounds: [lower, upper],
    provider: timing(durations(rows[0], latencyEvents.provider)),
    phasesOverlappingCompile: phases.filter((phase) =>
      compilations.some(
        (compile) =>
          compile.started < phase.end && compile.started + compile.duration > phase.start,
      ),
    ).length,
    workers: rows.slice(1).map((row) => ({
      native: timing(durations(row, latencyEvents.native)),
      cappedNative: timing(
        row
          .filter((entry) => entry.kind === latencyEvents.native && entry.auxiliary <= 64)
          .map((entry) => entry.duration),
      ),
      compilationWait: timing(durations(row, latencyEvents.compilation)),
      portWait: timing(durations(row, latencyEvents.port)),
      exclusiveWait: timing(durations(row, latencyEvents.exclusive)),
    })),
  };
}

function machineOptions(compiler, code, data, count, codeArena) {
  return {
    compiler,
    memoryModel: 'linear',
    limits: {
      maxInstructions: 128,
      maxSourceBytes: 2048,
      maxArtifactBytes: 262144,
      maxCachedArtifacts: 32,
      maxCachedBytes: 4 * 1024 * 1024,
      maxTranslationsPerRun: 8192,
    },
    memoryLimits: { maxBackings: 2, maxMappings: 3, maxBackingBytes: codeArena + data.length },
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'data', bytes: data },
      ],
      mappings: [span(ENTRY, 'code', code.length, true), span(RAM, 'data', data.length)],
    },
    cpus: Array.from({ length: count }, () => ({ entry: ENTRY })),
    sharedRamCapacity: codeArena + data.length,
  };
}

async function createObservedMachine(createMachine, options, Worker) {
  const prototype = Worker.prototype;
  const original = Object.getOwnPropertyDescriptor(prototype, 'postMessage');
  check(typeof original?.value === 'function', 'missing worker postMessage');
  let startup;
  Object.defineProperty(prototype, 'postMessage', {
    ...original,
    value(message, ...transfer) {
      if (message?.kind === 'start' && message.id === 0) {
        check(startup === undefined, 'duplicate worker startup');
        startup = message;
      }
      return Reflect.apply(original.value, this, [message, ...transfer]);
    },
  });
  try {
    const machine = await createMachine(options);
    if (!startup) {
      await machine.dispose();
      throw Error('worker startup did not expose the measurement inputs');
    }
    return {
      machine,
      shared: startup.shared,
      ramBase: startup.layout.ramBase,
      traceMemory: startup.latencyTraceMemory,
    };
  } finally {
    Object.defineProperty(prototype, 'postMessage', original);
  }
}

async function measureCase(
  compiler,
  createMachine,
  SharedControl,
  SLOT,
  workload,
  samples,
  Worker,
) {
  const repeat = workload === 'rep' || workload === 'contended';
  const code = programFor(workload, repeat ? REP : ALU, samples);
  const data = new Uint8Array(8192);
  for (let byte = 0; byte < REP_BYTES; byte++) data[byte] = (byte * 19 + 3) & 255;
  const codeArena = Math.ceil(code.length / 4096) * 4096;
  const count = workload === 'contended' ? 3 : 2;
  let ports = 0;
  const options = machineOptions(compiler, code, data, count, codeArena);
  if (workload === 'watch') options.memory.mappings.push(span(ALIAS, 'code', code.length));
  if (workload === 'port')
    options.io = {
      read: () => 0,
      write(port, width) {
        check(port === 0x80 && width === 32, 'unexpected port effect');
        ports++;
      },
    };
  const { machine, shared, ramBase, traceMemory } = await createObservedMachine(
    createMachine,
    options,
    Worker,
  );
  const [peer, ...others] = machine.cpus;
  const control = new SharedControl(shared);
  const words = new BigUint64Array(shared.buffer);
  const markerBase = (ramBase + codeArena + 4096) / 8;
  const marker = (id) => RAM + 4096n + BigInt(id * 128);
  const active = (id) => !(control.phase & 1n) && control.load(id, SLOT.active) === control.phase;
  const started = (id, tag) => active(id) && Atomics.load(words, markerBase + id * 16) === tag;
  const pressureProgress = () =>
    workload === 'watch' ? control.load(0, SLOT.invalidations) : BigInt(ports);
  const primaryState = (tag) =>
    state(ENTRY, { 8: RAM, 9: RAM + BigInt(REP_BYTES), 10: marker(0), 11: tag });
  const lockAddress = RAM + 2049n;
  const pressureOptions = (id, tag = 1n) =>
    state(ENTRY + BigInt(PRESSURE), {
      7: workload === 'watch' ? ALIAS + BigInt(PRESSURE + 4) : lockAddress,
      10: marker(id),
      11: tag,
    });
  const pauses = [],
    adminReads = [];
  let pressureRuns = [],
    measurementBegan,
    measurementEnded;
  let pressureWork = 0n,
    pressureCompiles = 0,
    observedInvalidations = 0n;
  try {
    await peer.setState(primaryState(0n));
    const warm = await peer.run({ budget: 6n, repeatBudget: 1024n });
    check(warm.reason === 'budget' && warm.resources.compilations === 1, 'peer did not warm');
    if (workload === 'contended') {
      for (const cpu of others) {
        await cpu.setState(pressureOptions(cpu.id, 0n));
        const seeded = await cpu.run({ budget: 3n });
        check(
          seeded.reason === 'budget' &&
            seeded.retired === 3n &&
            seeded.resources.compilations === 1,
          'split LOCK did not warm',
        );
      }
      await machine.writeMemory(lockAddress, new Uint8Array(8));
    }
    const pressureBefore = pressureProgress();
    if (traceMemory) measurementBegan = latencyCursors(traceMemory, count + 1);
    if (workload !== 'cold') {
      await Promise.all(others.map((cpu) => cpu.setState(pressureOptions(cpu.id))));
      pressureRuns = others.map((cpu) => cpu.run({ budget: BUDGET }));
      if (workload === 'port' || workload === 'watch') {
        await until(() => pressureProgress() > pressureBefore, workload);
      } else {
        await until(() => others.every((cpu) => started(cpu.id, 1n)), workload);
      }
    }
    for (let i = 0; i < samples; i++) {
      const tag = BigInt(i + 1);
      await peer.setState(primaryState(tag));
      const running = peer.run({ budget: BUDGET });
      await until(() => started(peer.id, tag), `active ${workload} peer`);
      let coldRun;
      if (workload === 'cold') {
        const rip = ENTRY + BigInt(4096 + 4096 * i);
        await others[0].setState(state(rip, { 0: 1n }));
        const before = control.phase;
        coldRun = others[0].run({ budget: 100n, stop: rip + BigInt(COLD_LENGTH * 4) });
        await until(
          () => control.phase > before && active(peer.id),
          'cold proof and peer readmission',
        );
      } else if (workload === 'watch' || workload === 'port') {
        const before = pressureProgress();
        await until(
          () => pressureProgress() > before && started(peer.id, tag),
          `fresh ${workload} effect`,
        );
      }
      const readBegan = performance.now();
      const read = await machine.readMemory(RAM, 8);
      adminReads.push(performance.now() - readBegan);
      check(
        read.every((byte, index) => byte === data[index]),
        'admin read saw corrupt guest data',
      );
      if (coldRun) {
        const result = await coldRun;
        check(
          result.reason === 'stop' &&
            result.retired === BigInt(COLD_LENGTH) &&
            result.resources.compilations === 1 &&
            result.state.registers[0] === BigInt.asUintN(64, 3n ** BigInt(COLD_LENGTH)),
          'bad cold result',
        );
        pressureWork += result.retired;
        pressureCompiles++;
      }
      await until(() => started(peer.id, tag), `${workload} pause admission`);
      const pauseBegan = performance.now();
      await peer.pause();
      pauses.push(performance.now() - pauseBegan);
      const result = await running;
      check(
        result.reason === 'paused' &&
          result.resources.compilations === 0 &&
          result.resources.nativeEntries > 0 &&
          result.retired > 0n &&
          result.retired + result.remaining === BUDGET &&
          result.repeatIterations + result.remainingRepeatBudget === BUDGET,
        'bad disjoint pause result',
      );
      if (repeat) {
        check(result.repeatIterations >= BigInt(REP_BYTES), 'REP did not make progress');
        if (result.state.rip === ENTRY + 11n) {
          const advance = result.repeatIterations % BigInt(REP_BYTES);
          check(
            result.state.registers[1] === BigInt(REP_BYTES) - advance &&
              result.state.registers[6] === RAM + advance &&
              result.state.registers[7] === RAM + BigInt(REP_BYTES) + advance,
            'bad REP continuation',
          );
        }
      } else {
        check(result.state.registers[0] === result.retired / 2n, 'bad ALU retirement');
      }
    }
    await Promise.all(others.map((cpu) => cpu.pause()));
    const pressureResults = await Promise.all(pressureRuns);
    await until(() => !(control.phase & 1n), 'last phase reopened');
    if (traceMemory) measurementEnded = latencyCursors(traceMemory, count + 1);
    for (const result of pressureResults) {
      check(
        result.reason === 'paused' &&
          result.retired > 0n &&
          result.retired + result.remaining === BUDGET,
        'bad sibling retirement',
      );
      pressureWork += result.retired;
      pressureCompiles += result.resources.compilations;
    }
    if (workload === 'port') check(BigInt(ports) === (pressureWork + 1n) / 2n, 'lost port effect');
    if (workload === 'watch') {
      observedInvalidations = pressureProgress() - pressureBefore;
      const [changed] = await machine.readMemory(ENTRY + BigInt(PRESSURE + 4), 1);
      check(
        observedInvalidations >= BigInt(samples) &&
          pressureCompiles > 1 &&
          changed === (1 ^ Number(((pressureWork + 2n) / 3n) & 1n)),
        'watched write lost',
      );
    }
    if (workload === 'contended') {
      check(pressureCompiles === 0, 'contended pressure recompiled during measurement');
      const read = await machine.readMemory(lockAddress, 8);
      const final = new DataView(read.buffer, read.byteOffset).getBigUint64(0, true);
      check(
        final === pressureResults.reduce((sum, result) => sum + result.retired / 2n, 0n),
        'locked update lost',
      );
    }
    if (repeat) {
      const rep = await machine.readMemory(RAM + BigInt(REP_BYTES), REP_BYTES);
      check(
        rep.every((byte, index) => byte === data[index]),
        'REP copied wrong bytes',
      );
    }
  } finally {
    await machine.dispose();
  }
  const trace = traceMemory && stages(traceMemory, measurementBegan, measurementEnded);
  if (trace) {
    check(trace.pauseRequestToSafe.samples === samples, 'pause trace lost events');
    check(trace.closedPhase.samples >= samples, 'phase trace lost events');
    if (workload === 'port')
      check(trace.workers[1].portWait.samples === ports, 'port trace lost events');
    if (workload === 'cold')
      check(trace.workers[1].compilationWait.samples === samples, 'cold trace lost events');
    if (workload === 'contended')
      check(
        trace.workers.slice(1).some((worker) => worker.exclusiveWait.samples !== 0),
        'no contended fallback',
      );
  }
  return {
    workload,
    workers: count,
    guestWork: {
      pressureRetired: String(pressureWork),
      pressureCompiles,
      ports,
      invalidations: String(observedInvalidations),
    },
    pausePublic: timing(pauses),
    adminReadPublic: timing(adminReads),
    trace,
  };
}

async function calibration(compiler, createMachine) {
  const machine = await createMachine(
    machineOptions(compiler, literal(ALU), new Uint8Array(4096), 2, 4096),
  );
  const values = [];
  try {
    for (let trial = 0; trial < 5; trial++) {
      await Promise.all(
        machine.cpus.map((cpu) =>
          cpu.setState(state(ENTRY, { 10: RAM + BigInt(cpu.id * 128), 11: 1n })),
        ),
      );
      const began = performance.now();
      const outcomes = await Promise.all(
        machine.cpus.map((cpu) => cpu.run({ budget: 2_000_000n })),
      );
      const elapsed = performance.now() - began;
      for (const run of outcomes) {
        check(
          run.reason === 'budget' &&
            run.retired === 2_000_000n &&
            run.state.registers[0] === 1_000_000n &&
            (trial === 0 || run.resources.compilations === 0),
          'calibration changed the work',
        );
      }
      if (trial > 0) values.push(elapsed);
    }
    return { elapsedMs: values, instructionsPerTrial: 4_000_000 };
  } finally {
    await machine.dispose();
  }
}

export async function measureLatency(compiler, clean, traced, samples, reverse, Worker) {
  const report = {};
  const roots = reverse ? { traced, clean } : { clean, traced };
  for (const [name, root] of Object.entries(roots)) {
    const [{ createMachine }, { SharedControl, SLOT }] = await Promise.all([
      import(new URL('host/machine.js', root).href),
      import(new URL('host/parallel/control.js', root).href),
    ]);
    const calibrationResult = await calibration(compiler, createMachine);
    const workloads = [];
    for (const workload of ['alu', 'rep', 'cold', 'port', 'watch', 'contended']) {
      workloads.push(
        await measureCase(compiler, createMachine, SharedControl, SLOT, workload, samples, Worker),
      );
    }
    report[name] = { calibration: calibrationResult, workloads };
  }
  return report;
}
