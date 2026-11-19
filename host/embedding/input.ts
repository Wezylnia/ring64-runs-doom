export const U64_MAX = (1n << 64n) - 1n;

export function unsigned64(value: unknown, name: string): asserts value is bigint {
  if (typeof value !== 'bigint' || value < 0n || value > U64_MAX) {
    throw new TypeError(`${name} must be an unsigned 64-bit bigint`);
  }
}

export function byteLength(value: unknown, name: string): asserts value is number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > 0xffffffff) {
    throw new TypeError(`${name} must be an unsigned wasm32 byte length`);
  }
}

export function copyMemoryInput(bytes: Uint8Array): Uint8Array {
  if (!(bytes instanceof Uint8Array)) throw new TypeError('memory input must be Uint8Array');
  const copy = new Uint8Array(bytes);
  byteLength(copy.length, 'length');
  return copy;
}
