// Headless DOOM on ring64 for Node: prints the guest console, reports the
// frame rate and writes selected frames as PNG files.
//
// usage: node host/run-node.mjs [--seconds 20] [--shots out/] [--warp] [-- doom args]
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { deflateSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { DoomMachine, SCREEN_WIDTH, SCREEN_HEIGHT, defaultLimits } from './doom-machine.js';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const sdk = process.env.RING64_SDK ?? path.join(root, '../../build/npm/ring64/index.js');
const { createCpu, loadCpuCompiler } = await import(sdk);

const argv = process.argv.slice(2);
const option = (name, fallback) => {
  const i = argv.indexOf(name);
  return i < 0 ? fallback : argv[i + 1];
};
const seconds = Number(option('--seconds', '20'));
const shots = option('--shots', path.join(root, 'build/shots'));
const memory = option('--memory', undefined);
const extra = argv.includes('--') ? argv.slice(argv.indexOf('--') + 1) : [];

function crc32(bytes) {
  let c = ~0;
  for (const b of bytes) {
    c ^= b;
    for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
  }
  return ~c >>> 0;
}

function png(rgba, width, height) {
  const chunk = (type, data) => {
    const out = Buffer.alloc(12 + data.length);
    out.writeUInt32BE(data.length, 0);
    out.write(type, 4, 'ascii');
    data.copy(out, 8);
    out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
    return out;
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 6;
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++)
    Buffer.from(rgba.buffer, rgba.byteOffset + y * width * 4, width * 4).copy(
      raw,
      y * (width * 4 + 1) + 1,
    );
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
    chunk('IDAT', deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

const [elf, wad] = await Promise.all([
  readFile(path.join(root, 'build/doom.elf')),
  readFile(option('--wad', path.join(root, 'build/doom1.wad'))),
]);

const began = performance.now();
const compiler = await loadCpuCompiler(memory);
const doom = await DoomMachine.create({
  createCpu,
  compiler,
  elf: new Uint8Array(elf),
  wad: new Uint8Array(wad),
  args: extra,
  ...(process.env.DOOM_LIMITS
    ? { limits: { ...defaultLimits, ...JSON.parse(process.env.DOOM_LIMITS) } }
    : {}),
});
doom.onLog = (line) => console.log(`[doom] ${line}`);
doom.onTitle = (title) => console.log(`[title] ${title}`);

await mkdir(shots, { recursive: true });
let latest;
doom.onFrame = (rgba) => (latest = rgba);

// Scripted input (doomkeys.h): start a new game from the title screen.
const KEY_ENTER = 13;
const KEY_UP = 0xad;
const KEY_FIRE = 0xa3;
const script = [
  [3000, KEY_ENTER], // open menu
  [3600, KEY_ENTER], // New Game
  [4200, KEY_ENTER], // Knee-Deep in the Dead
  [4800, KEY_ENTER], // Hurt me plenty
];
const holds = [
  [7000, 9000, KEY_UP],
  [9500, 10500, KEY_FIRE],
];

const savedAt = new Set();
let lastFrames = 0;
let lastReport = performance.now();
const deadline = began + seconds * 1000;
while (performance.now() < deadline && doom.exitCode === undefined) {
  const t = performance.now() - began;
  while (script.length && script[0][0] <= t) {
    const [, key] = script.shift();
    doom.key(key, true);
    doom.key(key, false);
  }
  for (const hold of holds) {
    if (!hold.down && t >= hold[0]) {
      hold.down = true;
      doom.key(hold[2], true);
    }
    if (!hold.up && t >= hold[1]) {
      hold.up = true;
      doom.key(hold[2], false);
    }
  }
  const { event, sleepMs } = await doom.slice(20_000_000n);
  if (event === 'sleep' && sleepMs > 0) await new Promise((r) => setTimeout(r, sleepMs));
  const second = Math.floor((performance.now() - began) / 2000) * 2;
  if (latest && !savedAt.has(second)) {
    savedAt.add(second);
    await writeFile(
      path.join(shots, `frame-${String(second).padStart(3, '0')}s.png`),
      png(latest, SCREEN_WIDTH, SCREEN_HEIGHT),
    );
  }
  const now = performance.now();
  if (now - lastReport >= 2000) {
    const fps = ((doom.frames - lastFrames) * 1000) / (now - lastReport);
    console.log(
      `[host] t=${((now - began) / 1000).toFixed(1)}s frames=${doom.frames} fps=${fps.toFixed(1)} retired=${doom.retired}`,
    );
    lastFrames = doom.frames;
    if (process.env.DOOM_STATS) {
      console.log(JSON.stringify(doom.stats));
      doom.stats = { runs: 0, timings: {}, resources: {}, reasons: {} };
    }
    lastReport = now;
  }
}
if (latest) await writeFile(path.join(shots, 'last.png'), png(latest, SCREEN_WIDTH, SCREEN_HEIGHT));
console.log(`[host] done: frames=${doom.frames} exit=${doom.exitCode}`);
if (doom.frames === 0) process.exitCode = 1;
doom.dispose();
