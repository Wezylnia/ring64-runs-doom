import { chromium } from 'playwright';
import { systemChromeExecutable } from '../support/browser.mjs';

export async function evaluateInChromium(server, evaluate, argument) {
  await new Promise((ready, failed) => {
    server.once('error', failed);
    server.listen(0, '127.0.0.1', ready);
  });
  let browser;
  try {
    const executablePath = process.env.RING64_BROWSER || (await systemChromeExecutable());
    browser = await chromium.launch({ headless: true, executablePath });
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    const result = await page.evaluate(evaluate, argument);
    result.browserVersion = browser.version();
    return result;
  } finally {
    try {
      await browser?.close();
    } finally {
      await new Promise((closed) => server.close(closed));
    }
  }
}
