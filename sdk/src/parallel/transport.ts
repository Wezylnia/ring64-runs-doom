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
type WorkerMessage = (message: unknown, post: Message) => void;

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

export async function launchWorker(message: Message, failure: Failure): Promise<Transport> {
  if (browser()) {
    if (typeof Worker !== 'function')
      throw new ParallelError('unsupported', 'parallel execution requires dedicated workers');
    const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
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
    node = process.getBuiltinModule('node:worker_threads');
  } catch (cause) {
    throw new ParallelError('unsupported', 'parallel execution requires Node worker threads', {
      cause,
    });
  }
  const worker = new node.Worker(new URL(import.meta.resolve('./worker.js')));
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

export function connectWorker(message: WorkerMessage): void {
  if (browser()) {
    requireWorkerCapability();
    const post: Message = (value) => globalThis.postMessage(value);
    globalThis.addEventListener('message', (event: MessageEvent<unknown>) =>
      message(event.data, post),
    );
    return;
  }
  const { parentPort } = process.getBuiltinModule('node:worker_threads');
  if (!parentPort) throw new Error('parallel worker was not started by its machine');
  const post: Message = (value) => parentPort.postMessage(value);
  parentPort.on('message', (value: unknown) => message(value, post));
}
