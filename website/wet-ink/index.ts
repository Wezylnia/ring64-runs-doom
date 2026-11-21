import { enableFullscreen } from './fullscreen.js';
import { createPigmentPainter, type PigmentPainter } from './pigment.js';
import {
  pigmentHeight,
  pigmentWidth,
  type Frame,
  type StepReply,
  type StepRequest,
} from './protocol.js';

function attachInk(
  canvas: HTMLCanvasElement,
  stage: HTMLElement,
  play: HTMLButtonElement,
  status: HTMLElement,
  fullscreen: HTMLButtonElement,
) {
  const motion = matchMedia('(prefers-reduced-motion: reduce)');
  const frameInterval = 1000 / 60;
  const keyMotion = new Map<string, [number, number]>([
    ['ArrowLeft', [-2800, 0]],
    ['ArrowRight', [2800, 0]],
    ['ArrowUp', [0, -2800]],
    ['ArrowDown', [0, 2800]],
  ]);
  let painter: PigmentPainter;
  let worker: Worker;
  let playing = !motion.matches;
  let busy = false;
  let request = 0;
  let firstFrame = true;
  let nextStep: number | undefined;
  let failed = false;
  let pending: Frame | undefined;
  let pointer = { x: 32768, y: 32768, active: false };
  let previous = pointer;

  function syncPlay() {
    const label = playing ? 'pause animation' : 'play animation';
    stage.dataset.playing = String(playing);
    play.dataset.playing = String(playing);
    play.textContent = playing ? 'Pause' : 'Play';
    play.title = label;
    play.setAttribute('aria-label', label);
  }

  function schedule() {
    if (document.hidden || failed || request || (!pending && (busy || (!firstFrame && !playing))))
      return;
    request = requestAnimationFrame(advance);
  }

  function setPlaying(value: boolean) {
    playing = value;
    previous = pointer;
    nextStep = undefined;
    syncPlay();
    schedule();
  }

  function fail(message: string) {
    if (failed) return;
    failed = true;
    worker?.terminate();
    if (request) cancelAnimationFrame(request);
    playing = false;
    syncPlay();
    play.disabled = true;
    status.textContent = message;
  }

  function draw(data: Frame) {
    const start = performance.now();
    painter.paint(data.pigments);
    canvas.dataset.renderMs = data.ms.toFixed(3);
    canvas.dataset.paintMs = (performance.now() - start).toFixed(3);
    canvas.dataset.frame = String(data.tick + 1);
    status.textContent = '';
    play.disabled = false;
  }

  function advance(now: number) {
    request = 0;
    if (pending) {
      try {
        draw(pending);
      } catch {
        fail('could not paint wet ink. reload to try again.');
        return;
      }
      pending = undefined;
    }
    if (busy || (!firstFrame && !playing)) return;
    if (nextStep !== undefined && now + 1 < nextStep) {
      schedule();
      return;
    }
    nextStep = Math.max(nextStep ?? now, now - frameInterval * 0.5) + frameInterval;

    const near = pointer;
    const prior = previous.active && near.active ? previous : near;
    previous = near;
    const packed = ((prior.y << 16) | prior.x) >>> 0;
    firstFrame = false;
    busy = true;
    const step: StepRequest = { x: near.x, y: near.y, previous: packed, active: near.active };
    worker.postMessage(step);
  }

  const clamp = (value: number) => Math.round(Math.min(65535, Math.max(0, value)));
  function move(event: PointerEvent) {
    if (!event.isPrimary) return;
    const box = canvas.getBoundingClientRect();
    pointer = {
      x: clamp(((event.clientX - box.left) * 65535) / box.width),
      y: clamp(((event.clientY - box.top) * 65535) / box.height),
      active: true,
    };
  }

  function leave() {
    pointer = { ...pointer, active: false };
    previous = pointer;
  }

  function keyboard(event: KeyboardEvent) {
    if (event.key === ' ' && !play.disabled) {
      event.preventDefault();
      if (!event.repeat) setPlaying(!playing);
      return;
    }
    const delta = keyMotion.get(event.key);
    if (!delta) return;
    event.preventDefault();
    pointer = { x: clamp(pointer.x + delta[0]), y: clamp(pointer.y + delta[1]), active: true };
  }

  try {
    painter = createPigmentPainter(canvas, pigmentWidth, pigmentHeight);
    worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
  } catch {
    fail('could not start wet ink. reload to try again.');
    return;
  }
  canvas.dataset.painter = painter.kind;
  worker.addEventListener('error', () => fail('could not start the ring64 worker.'));
  worker.addEventListener('messageerror', () => fail('could not read the wet ink frame.'));
  worker.addEventListener('message', ({ data }: MessageEvent<StepReply>) => {
    if (failed) return;
    busy = false;
    if ('error' in data) {
      fail(data.error);
      return;
    }
    pending = data;
    schedule();
  });
  canvas.addEventListener('webglcontextlost', () =>
    fail('graphics context lost. reload to restart.'),
  );
  play.addEventListener('click', () => setPlaying(!playing));
  motion.addEventListener('change', () => {
    if (motion.matches) setPlaying(false);
  });
  document.addEventListener('visibilitychange', () => {
    nextStep = undefined;
    if (document.hidden) leave();
    schedule();
  });
  canvas.addEventListener('pointerenter', move);
  canvas.addEventListener('pointermove', move);
  canvas.addEventListener('pointerleave', leave);
  canvas.addEventListener('pointercancel', leave);
  canvas.addEventListener('pointerup', (event) => {
    if (event.pointerType === 'touch') leave();
  });
  canvas.addEventListener('keydown', keyboard);
  canvas.addEventListener('blur', leave);
  enableFullscreen(stage, play, fullscreen);
  syncPlay();
  schedule();
}

const canvas = document.getElementById('ink');
const stage = document.getElementById('ink-stage');
const status = document.getElementById('status');
const play = document.getElementById('play');
const fullscreen = document.getElementById('fullscreen');
if (
  canvas instanceof HTMLCanvasElement &&
  stage instanceof HTMLElement &&
  status instanceof HTMLElement &&
  play instanceof HTMLButtonElement &&
  fullscreen instanceof HTMLButtonElement
) {
  attachInk(canvas, stage, play, status, fullscreen);
}
