import { TIME_STATE_SIZE } from './time-layout.js';

export const PROTECTION_KEYS_LAYOUT = { offset: TIME_STATE_SIZE, size: 8, pkru: 0, pkrs: 4 };
export const PROTECTION_KEYS_STATE_SIZE =
  PROTECTION_KEYS_LAYOUT.offset + PROTECTION_KEYS_LAYOUT.size;
