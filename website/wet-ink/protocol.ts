export const pigmentWidth = 192;
export const pigmentHeight = 108;

export interface StepRequest {
  x: number;
  y: number;
  previous: number;
  active: boolean;
  compiler?: WebAssembly.Module;
}

export interface Frame {
  pigments: Uint8Array;
  tick: number;
  ms: number;
}

export type StepReply = Frame | { error: string };
