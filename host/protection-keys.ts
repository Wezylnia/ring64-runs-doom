export interface ProtectionKeysState {
  pkru: number;
  pkrs: number;
}

export interface ProtectionKeysLayout {
  offset: number;
  size: number;
  pkru: number;
  pkrs: number;
}

export function captureProtectionKeysState(input: ProtectionKeysState): ProtectionKeysState {
  if (typeof input !== 'object' || input === null || Array.isArray(input))
    throw new TypeError('protection keys must be a record');
  const { pkru, pkrs } = input;
  const word = (value: number) => {
    if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > 0xffffffff)
      throw new TypeError('protection key words must be unsigned32 numbers');
    return value === 0 ? 0 : value;
  };
  return { pkru: word(pkru), pkrs: word(pkrs) };
}

export function validateProtectionKeysLayout(
  input: ProtectionKeysLayout,
  stateSize: number,
  precedingEnd: number,
): void {
  if (typeof input !== 'object' || input === null || Array.isArray(input))
    throw new Error('core returned an invalid protection keys layout');
  const { offset, size, pkru, pkrs } = input;
  if (
    [offset, size, pkru, pkrs, stateSize, precedingEnd].some(
      (value) => !Number.isSafeInteger(value) || value < 0 || value > 0xffffffff,
    ) ||
    size !== 8 ||
    offset < precedingEnd ||
    offset + size > stateSize ||
    pkru + 4 > size ||
    pkrs + 4 > size ||
    Math.abs(pkru - pkrs) < 4
  )
    throw new Error('core returned an invalid protection keys layout');
}
