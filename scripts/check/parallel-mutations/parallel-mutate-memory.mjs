function scenario(name) {
  return [
    'env',
    `RING64_PARALLEL_SCENARIO=${name}`,
    'node',
    '--import',
    'tsx',
    '--test',
    'tests/runners/parallel-node.test.ts',
  ];
}

const splitWrite =
  'split locked write fault publishes no partial bytes and exact retry retires once';
const alias = 'exchange through overlapping aliases snapshots once and later logical bytes win';

export const memoryMutations = [
  {
    name: 'split-operand-skips-tail-permission',
    path: 'src/memory/access.rs',
    from: 'if !permitted {\n                return Err(failure(Cause::Permission, cursor));\n            }',
    to: 'if !permitted && (index == first || !cfg!(feature = "parallel")) {\n                return Err(failure(Cause::Permission, cursor));\n            }',
    check: scenario(splitWrite),
    failure: /late permission check fault: expected data-fault, received stop/,
    build: 'core',
  },
  {
    name: 'page-store-dirties-prefix-before-tail-admission',
    path: 'src/x86/paging/access.rs',
    from: 'let length = u64::from(width - offset).min(remaining) as u32;\n            let write = if access == Access::Write {',
    to: `let length = u64::from(width - offset).min(remaining) as u32;
            #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
            if access == Access::Write && dirty && offset == 0 && length < width && storage.image.parallel_attached() {
                let update = storage.image.prepare_physical_update(leaf, entry_width).expect("mutation expects writable PTE");
                storage.image.publish_physical_update(&update, walk::DIRTY);
            }
            let write = if access == Access::Write {`,
    check: scenario(
      'paged split write denial preserves the first payload and dirty bit until repair',
    ),
    failure: /first leaf not dirty: expected 0, received 64/,
    build: 'core',
  },
  {
    name: 'duplicate-alias-first-byte-wins',
    path: 'src/memory/parallel.rs',
    from: 'for (&offset, &byte) in offsets.iter().zip(values) {',
    to: 'for (&offset, &byte) in offsets.iter().zip(values).rev() {',
    check: scenario(alias),
    failure: /later guest offsets replace duplicate canonical bytes/,
    build: 'core',
  },
  {
    name: 'generated-fast-lane-forgets-backing-offset',
    path: 'src/memory/parallel/image.rs',
    from: 'let absolute = backing.offset + mapping.offset + relative;\n        if (absolute & 7) + width > 8',
    to: 'let absolute = backing.offset + relative;\n        if (absolute & 7) + width > 8',
    check: scenario(alias),
    failure: /peer sees one canonical lane/,
    build: 'core',
  },
  {
    name: 'parallel-slice-flushes-architectural-tlb',
    path: 'src/embedding/exports/memory/parallel.rs',
    from: '.paging_work\n            .clear();',
    to: '.memory_admin_changed();',
    check: scenario(
      'guest PTE replacement preserves both local TLBs until the selected CPU executes INVLPG',
    ),
    failure: /(?:writer|peer) keeps its old TLB translation/,
    build: 'core',
  },
  {
    name: 'locked-masked-lane-publishes-without-compare',
    path: 'src/dbt/wasm/memory/parallel.rs',
    from: 'self.op(Op::I64AtomicRmwCmpxchg(LANE));',
    to: `self.op(Op::LocalSet(scalar.payload))
            .op(Op::Drop)
            .op(Op::LocalGet(scalar.payload))
            .op(Op::I64AtomicStore(LANE))
            .op(Op::LocalGet(scalar.old));`,
    check: scenario(
      'narrow locked updates through distinct aliases preserve neighboring bytes and exact retirements',
    ),
    failure: /target bytes and never-addressed neighbors/,
    build: 'core',
  },
];
