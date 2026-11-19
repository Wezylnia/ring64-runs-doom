// Copied into the scratch host graph. Each actor owns its own bounded row.
const ROW_RECORDS = 32_768;
const FIELDS = 5;
const COUNT_STRIDE = 128 / 4;
const BUFFER_OFFSET = 2048;
const CLOCK_ORIGIN = performance.timeOrigin - 1_700_000_000_000;

export const latencyEvents = Object.freeze({
  native: 1,
  close: 2,
  drained: 3,
  open: 4,
  pauseRequest: 5,
  pauseSafe: 6,
  compilation: 7,
  port: 8,
  provider: 9,
  exclusive: 10,
  acknowledgement: 11,
});

export function latencyBuffer(actors) {
  if (actors > BUFFER_OFFSET / 4 / COUNT_STRIDE) throw Error('too many benchmark actors');
  return new SharedArrayBuffer(BUFFER_OFFSET + actors * ROW_RECORDS * FIELDS * 8);
}

export function latencyCursors(buffer, actors) {
  const counts = new Int32Array(buffer, 0, BUFFER_OFFSET / 4);
  return Array.from({ length: actors }, (_, actor) => Atomics.load(counts, actor * COUNT_STRIDE));
}

export class LatencyTrace {
  constructor(buffer, actor) {
    this.actor = actor;
    this.countOffset = actor * COUNT_STRIDE;
    this.counts = new Int32Array(buffer, 0, BUFFER_OFFSET / 4);
    this.row = new Float64Array(
      buffer,
      BUFFER_OFFSET + actor * ROW_RECORDS * FIELDS * 8,
      ROW_RECORDS * FIELDS,
    );
  }

  cursor = 0;

  now() {
    return CLOCK_ORIGIN + performance.now();
  }

  mark(kind, identity, auxiliary = 0) {
    this.write(kind, this.now(), 0, identity, auxiliary);
  }

  span(kind, started, identity, auxiliary = 0) {
    this.write(kind, started, this.now() - started, identity, auxiliary);
  }

  write(kind, started, duration, identity, auxiliary) {
    // The cursor is single-writer. Collect only after the workers have stopped.
    const index = this.cursor++;
    if (index < ROW_RECORDS) {
      const offset = index * FIELDS;
      this.row[offset] = kind;
      this.row[offset + 1] = started;
      this.row[offset + 2] = duration;
      this.row[offset + 3] = Number(identity);
      this.row[offset + 4] = Number(auxiliary);
    }
    Atomics.store(this.counts, this.countOffset, this.cursor);
  }

  read(start, end) {
    const count = Atomics.load(this.counts, this.countOffset);
    if (end > ROW_RECORDS || end > count)
      throw Error(`latency trace overflow on actor ${this.actor}`);
    return Array.from({ length: end - start }, (_, index) => {
      const offset = (start + index) * FIELDS;
      const [kind, started, duration, identity, auxiliary] = this.row.subarray(
        offset,
        offset + FIELDS,
      );
      return { kind, started, duration, identity, auxiliary };
    });
  }
}
