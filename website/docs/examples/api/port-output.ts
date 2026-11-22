import type { IoProvider } from 'ring64';

export function portOutput(onByte: (value: number) => void): IoProvider {
  return {
    read: () => 0,
    write(port, width, value) {
      if (port === 0xe9 && width === 8) onByte(value);
    },
  };
}
