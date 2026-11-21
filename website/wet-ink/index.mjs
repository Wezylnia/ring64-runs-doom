import { enableFullscreen } from './fullscreen.mjs';
import { createPigmentPainter } from './pigment.mjs';

const canvas = document.getElementById('ink');
const play = document.getElementById('play');
const status = document.getElementById('status');
const motion = matchMedia('(prefers-reduced-motion: reduce)');
const frameInterval = 1000 / 60;
const keyMotion = {
  ArrowLeft: [-2800, 0],
  ArrowRight: [2800, 0],
  ArrowUp: [0, -2800],
  ArrowDown: [0, 2800],
};
let painter;
let worker;
let playing = !motion.matches;
let busy = false;
let request = 0;
let firstFrame = true;
let nextStep;
let failed = false;
let pending;
let pointer = { x: 32768, y: 32768, active: false };
let previous = pointer;

function syncPlay() {
  play.textContent = playing ? 'Pause' : 'Play';
}

function schedule() {
  if (document.hidden || failed || request || (!pending && (busy || (!firstFrame && !playing))))
    return;
  request = requestAnimationFrame(advance);
}

function setPlaying(value) {
  playing = value;
  previous = pointer;
  nextStep = undefined;
  syncPlay();
  schedule();
}

function fail(message) {
  if (failed) return;
  failed = true;
  worker?.terminate();
  if (request) cancelAnimationFrame(request);
  playing = false;
  syncPlay();
  play.disabled = true;
  status.textContent = message;
}

function draw(data) {
  const start = performance.now();
  painter.paint(data.pigments);
  canvas.dataset.renderMs = data.ms.toFixed(3);
  canvas.dataset.paintMs = (performance.now() - start).toFixed(3);
  canvas.dataset.frame = String(data.tick + 1);
  status.textContent = '';
  play.disabled = false;
}

function advance(now) {
  request = 0;
  if (pending) {
    try {
      draw(pending);
    } catch {
      fail('Wet Ink could not paint the frame. Reload to try again.');
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
  worker.postMessage({ x: near.x, y: near.y, previous: packed, active: Number(near.active) });
}

const clamp = (value) => Math.round(Math.min(65535, Math.max(0, value)));
function move(event) {
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

function keyboard(event) {
  const delta = keyMotion[event.key];
  if (!delta) return;
  event.preventDefault();
  pointer = { x: clamp(pointer.x + delta[0]), y: clamp(pointer.y + delta[1]), active: true };
}

function start() {
  try {
    painter = createPigmentPainter(canvas, 192, 108);
    worker = new Worker(new URL('./worker.mjs', import.meta.url), { type: 'module' });
  } catch {
    fail('Wet Ink could not start. Reload to try again.');
    return;
  }
  canvas.dataset.painter = painter.kind;
  worker.addEventListener('error', () => fail('The Ring64 worker could not start.'));
  worker.addEventListener('messageerror', () => fail('Wet Ink could not read the frame.'));
  worker.addEventListener('message', ({ data }) => {
    if (failed) return;
    busy = false;
    if (data.error) {
      fail(data.error);
      return;
    }
    pending = data;
    schedule();
  });
  canvas.addEventListener('webglcontextlost', () =>
    fail('Graphics context lost. Reload to restart.'),
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
  enableFullscreen(
    document.getElementById('ink-stage'),
    play,
    document.getElementById('fullscreen'),
  );
  syncPlay();
  schedule();
}

start();
