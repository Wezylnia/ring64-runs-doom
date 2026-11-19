import process from 'node:process';

const nodeTest = (...args) => [process.execPath, '--import', 'tsx', '--test', ...args];
const protocolTest = (name) =>
  nodeTest(`--test-name-pattern=${name}`, 'host/parallel-protocol.test.ts');
const leaseCheck = 'this.phase === phase && this.load(cpu, SLOT.invalidations) === invalidation';
const portValidation = 'host/machine.ts';

export const protocolMutations = [
  {
    name: 'lease-phase-recheck',
    path: 'host/parallel/control.ts',
    from: leaseCheck,
    to: leaseCheck.replace('=== phase', '>= phase'),
    check: protocolTest('a phase close between publishing and rechecking a lease prevents entry'),
    failure: /true !== false/,
    build: 'none',
  },
  {
    name: 'lease-invalidation-watermark',
    path: 'host/parallel/control.ts',
    from: leaseCheck,
    to: 'this.phase === phase',
    check: protocolTest(
      'a reopened phase cannot admit a worker before its exact invalidation arrives',
    ),
    failure: /true !== false/,
    build: 'none',
  },
  {
    name: 'worker-apply-invalidation',
    path: 'host/parallel/worker.ts',
    from: '          this.#applyInvalidation(core);\n',
    to: '',
    check: [
      'env',
      'RING64_PARALLEL_SCENARIO=peer code patches through RW alias displace a warm executable owner before CPUID successor',
      ...nodeTest('tests/runners/parallel-node.test.ts'),
    ],
    failure: /CPUID successor uses the new executable bytes: expected 2, received 1/,
    build: 'host',
  },
  {
    name: 'io-ticket-advance',
    path: portValidation,
    from: '    peer.portTicket = message.ticket;\n',
    to: '',
    check: protocolTest('a single run publishes two reached I/O effects in ticket order'),
    failure: /the reached I\/O sequence lost an effect/,
    build: 'host',
  },
  {
    name: 'io-reject-duplicate-ticket',
    path: portValidation,
    from: '      message.ticket !== nextIdentity(peer.portTicket) ||\n',
    to: '',
    check: protocolTest('a duplicate reached I/O ticket fails before the provider runs'),
    failure: /rejected request reached the provider/,
    build: 'host',
  },
  {
    name: 'io-reject-stale-run',
    path: portValidation,
    from: '      peer.command.sequence !== message.sequence ||\n',
    to: '',
    check: protocolTest('a reached I/O request from a stale run fails before the provider runs'),
    failure: /rejected request reached the provider/,
    build: 'host',
  },
  {
    name: 'io-require-active-run',
    path: portValidation,
    from: "      peer.command?.method !== 'run' ||\n      peer.command.sequence !== message.sequence ||\n",
    to: '      peer.command?.sequence !== message.sequence ||\n',
    check: protocolTest(
      'a port request tagged to idle CPU inspection fails before the provider runs',
    ),
    failure: /idle inspection reached the provider/,
    build: 'host',
  },
  {
    name: 'ins-destination-permit',
    path: 'src/embedding/exports/io.rs',
    from: 'let _destination_permit = (kind == 2).then(|| {',
    to: 'let _destination_permit = (kind == 3).then(|| {',
    check: protocolTest(
      'a reached INS retains its exclusive grant across input and a late destination fault',
    ),
    failure: /reached INS input lacks a closed phase/,
    build: 'core',
  },
];
