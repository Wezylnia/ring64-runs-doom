export async function awaitBrowserResult(page, name, timeoutMessage) {
  await page.waitForFunction((key) => globalThis[key] !== undefined, name, {
    timeout: 30_000,
  });
  let deadline;
  return Promise.race([
    page.evaluate(async (key) => {
      try {
        return await globalThis[key];
      } catch (error) {
        const causes = new Set();
        const messages = [];
        for (let current = error; current !== undefined && !causes.has(current);) {
          causes.add(current);
          messages.push(String(current));
          current = current instanceof Error ? current.cause : undefined;
        }
        throw new Error(messages.join('\n'));
      }
    }, name),
    new Promise((_, reject) => {
      deadline = setTimeout(() => reject(new Error(timeoutMessage)), 120_000);
    }),
  ]).finally(() => clearTimeout(deadline));
}
