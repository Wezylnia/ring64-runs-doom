import type { MmioProvider } from 'ring64';

export function byteRegister(base: bigint): MmioProvider {
  let value = 0n;
  return {
    ranges: [{ base, length: 1 }],
    read: () => value,
    write: (_address, _width, next) => {
      value = next;
    },
  };
}
