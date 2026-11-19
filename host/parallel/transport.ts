export class ParallelError extends Error {
  constructor(
    readonly code: 'unsupported' | 'busy' | 'capacity' | 'failed' | 'disposed',
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = 'ParallelError';
  }
}

export interface Transport {
  post(message: unknown): void;
  terminate(): Promise<void>;
}

type Message = (message: unknown) => void;
type Failure = (error: unknown) => void;

function browser(): boolean {
  return (
    typeof window !== 'undefined' ||
    (typeof globalThis.navigator !== 'undefined' && typeof process === 'undefined')
  );
}

export function requireWorkerCapability(): void {
  if (browser() && globalThis.crossOriginIsolated !== true)
    throw new ParallelError(
      'unsupported',
      'parallel execution requires browser cross-origin isolation',
    );
  if (typeof SharedArrayBuffer !== 'function' || typeof Atomics.wait !== 'function')
    throw new ParallelError('unsupported', 'parallel execution requires shared memory and atomics');
}

async function nodeThreads(): Promise<typeof import('node:worker_threads')> {
  // The browser bundle must resolve only the browser transport.
  const moduleName: string = 'node:worker_threads';
  return (await import(moduleName)) as typeof import('node:worker_threads');
}

export async function launchWorker(message: Message, failure: Failure): Promise<Transport> {
  const url = new URL('./worker.js', import.meta.url);
  if (browser()) {
    if (typeof Worker !== 'function')
      throw new ParallelError('unsupported', 'parallel execution requires dedicated workers');
    const worker = new Worker(url, { type: 'module' });
    worker.addEventListener('message', (event: MessageEvent<unknown>) => message(event.data));
    worker.addEventListener('messageerror', () =>
      failure(new Error('worker message could not be decoded')),
    );
    worker.addEventListener('error', (event) => {
      event.preventDefault();
      failure(event.error ?? new Error(event.message));
    });
    return {
      post: (value) => worker.postMessage(value),
      terminate: () => {
        worker.terminate();
        return Promise.resolve();
      },
    };
  }
  let node: typeof import('node:worker_threads');
  try {
    node = await nodeThreads();
  } catch (cause) {
    throw new ParallelError('unsupported', 'parallel execution requires Node worker threads', {
      cause,
    });
  }
  const worker = new node.Worker(url);
  let terminated = false;
  worker.on('message', message);
  worker.on('messageerror', failure);
  worker.on('error', failure);
  worker.on('exit', (code) => {
    if (!terminated) failure(new Error(`worker exited with code ${code}`));
  });
  return {
    post: (value) => worker.postMessage(value),
    async terminate() {
      terminated = true;
      await worker.terminate();
    },
  };
}

export async function connectWorker(message: Message): Promise<(value: unknown) => void> {
  if (browser()) {
    requireWorkerCapability();
    globalThis.addEventListener('message', (event: MessageEvent<unknown>) => message(event.data));
    return (value) => globalThis.postMessage(value);
  }
  const { parentPort } = await nodeThreads();
  if (!parentPort) throw new Error('parallel worker was not started by its machine');
  parentPort.on('message', message);
  return (value) => parentPort.postMessage(value);
}
