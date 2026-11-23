export interface BackingRecord {
  id: string;
  length: number;
  offset: number;
  incarnation: bigint;
}

export const BACKING_PAGE_BYTES = 4096;

export function backingAllocationBytes(length: number): number {
  return Math.ceil(length / BACKING_PAGE_BYTES) * BACKING_PAGE_BYTES;
}

export function findOffset(
  layout: { ramBase: number; ramCapacity: number },
  records: Iterable<BackingRecord>,
  length: number,
): number {
  let offset = layout.ramBase;
  const pageBytes = backingAllocationBytes(length);
  for (const entry of [...records].sort((a, b) => a.offset - b.offset)) {
    if (offset + pageBytes <= entry.offset) return offset;
    offset = entry.offset + backingAllocationBytes(entry.length);
  }
  if (offset + pageBytes > layout.ramBase + layout.ramCapacity)
    throw new RangeError('parallel backing arena capacity exceeded');
  return offset;
}

export function seedBacking(
  storeLane: (offset: number, value: bigint) => void,
  offset: number,
  bytes: Uint8Array,
): void {
  const input = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
  let i = 0;
  for (; i + 8 <= bytes.length; i += 8) storeLane(offset + i, input.getBigUint64(i, true));
  if (i === bytes.length) return;
  let tail = 0n;
  for (let j = i; j < bytes.length; j++) tail |= BigInt(bytes[j]!) << BigInt((j - i) * 8);
  storeLane(offset + i, tail);
}
