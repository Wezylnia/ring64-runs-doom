import { access } from 'node:fs/promises';

const systemChrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

export async function systemChromeExecutable() {
  try {
    await access(systemChrome);
    return systemChrome;
  } catch {
    return undefined;
  }
}

export async function chromiumExecutable() {
  const configured = process.env.RING64_BROWSER;
  if (configured || process.platform !== 'darwin') return configured;
  return (await systemChromeExecutable()) ?? configured;
}
