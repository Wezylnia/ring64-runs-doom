import type { Core } from '../wasm/core.js';

export function taskResumeExit(
  resumed: ReturnType<Core['resumeTask']>,
  deliveryEnabled: boolean,
): string | undefined {
  if (typeof resumed !== 'string') return deliveryEnabled ? undefined : resumed.taskEvent;
  if (resumed === 'dispatch' || (resumed === 'pending-interrupt' && deliveryEnabled)) return;
  return resumed;
}

export function observeInterruptExit(
  core: Core,
  reason: 'pending-interrupt' | 'halted',
  deliveryEnabled: boolean,
) {
  let eventKind = core.eventKind();
  let queuedInterrupts = core.hasQueuedInterrupts();
  if (reason === 'halted') {
    if (!queuedInterrupts || core.externalBoundary() !== 'pending-interrupt')
      return { eventKind, queuedInterrupts, servicingEvent: false };
    eventKind = core.eventKind();
    queuedInterrupts = core.hasQueuedInterrupts();
  }
  if (!deliveryEnabled) return 'pending-interrupt';
  if (eventKind !== 'pending-interrupt')
    throw new Error('core failed to accept eligible interrupt');
  return { eventKind, queuedInterrupts, servicingEvent: true };
}
