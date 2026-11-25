// Plays a few seconds of DOOM in headless Chromium and saves screenshots.
// usage: node host/browser-check.mjs [url] [outdir]
import { chromium } from 'playwright';
import { mkdir } from 'node:fs/promises';
import path from 'node:path';

const url = process.argv[2] ?? 'http://localhost:8064/';
const out = process.argv[3] ?? 'build/browser-shots';
await mkdir(out, { recursive: true });

const browser = await chromium.launch(
  process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {},
);
const page = await browser.newPage({
  viewport: { width: 1280, height: 900 },
  deviceScaleFactor: 1,
});
const errors = [];
page.on('pageerror', (error) => errors.push(String(error)));
page.on('console', (message) => {
  if (message.type() === 'error') errors.push(message.text());
});
await page.goto(url);
await page.waitForSelector('#play:not([disabled])', { timeout: 60_000 });
await page.waitForTimeout(3000);
await page.screenshot({ path: path.join(out, '1-attract.png') });

await page.click('#play');
const press = async (key, ms = 120) => {
  await page.keyboard.down(key);
  await page.waitForTimeout(ms);
  await page.keyboard.up(key);
};
await press('Enter');
await page.waitForTimeout(500);
await press('Enter');
await page.waitForTimeout(500);
await press('Enter');
await page.waitForTimeout(500);
await press('Enter');
await page.waitForTimeout(2500);
await page.keyboard.down('ArrowUp');
await page.waitForTimeout(1500);
await page.keyboard.up('ArrowUp');
await press('Control', 400);
await page.waitForTimeout(800);
await page.screenshot({ path: path.join(out, '2-e1m1.png') });
await press('Tab');
await page.waitForTimeout(600);
await page.screenshot({ path: path.join(out, '3-automap.png') });
await press('Tab');
await page.waitForTimeout(2000);

const meters = await page.evaluate(() => ({
  fps: document.getElementById('fps').textContent,
  mips: document.getElementById('mips').textContent,
  retired: document.getElementById('retired').textContent,
  translations: document.getElementById('translations').textContent,
  mode: document.getElementById('mode').textContent,
  status: document.getElementById('status').textContent,
}));
console.log(JSON.stringify(meters));
await page.locator('#canvas').screenshot({ path: path.join(out, '4-canvas.png') });
console.log(`errors: ${errors.length ? errors.join('\n') : 'none'}`);
await browser.close();
