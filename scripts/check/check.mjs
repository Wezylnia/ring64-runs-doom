import { spawnSync } from 'node:child_process';

function run(command, ...args) {
  const result = spawnSync(command, args, { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function npm(...args) {
  const cli = process.env.npm_execpath;
  if (cli) run(process.execPath, cli, ...args);
  else run('npm', ...args);
}

npm('run', 'format:check');
npm('run', 'lint');

run('cargo', 'clippy', '--all-targets', '--', '-D', 'warnings');
run('cargo', 'clippy', '--target', 'wasm32-unknown-unknown', '--lib', '--', '-D', 'warnings');
for (const profile of ['parallel', 'memory64']) {
  run(
    'cargo',
    'clippy',
    '--target',
    'wasm32-unknown-unknown',
    '--lib',
    '--features',
    profile,
    '--',
    '-D',
    'warnings',
  );
}
run('cargo', 'test', '--locked');
run('cargo', 'test', '--locked', '--features', 'parallel');

npm('run', 'build');
npm('test');
npm('run', 'test:browser');
npm('run', 'test:package');
