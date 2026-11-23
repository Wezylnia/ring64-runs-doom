import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import { encode, hash, manifest, root } from '../artifacts.mjs';

const families = {
  'src/embedding/exports/memory/scalar.rs': [
    'load',
    'load_for_write',
    'store',
    'segmented_load',
    'segmented_load_for_write',
    'segmented_store',
  ],
  'src/embedding/exports/numeric/vector.rs': ['vector_load', 'vector_store'],
  'src/embedding/exports/numeric/mod.rs': ['load_mxcsr', 'store_mxcsr'],
  'src/embedding/exports/numeric/x87.rs': ['x87'],
  'src/embedding/exports/numeric/mmx.rs': ['mmx'],
  'src/embedding/exports/numeric/numeric_image/mod.rs': ['numeric_image'],
  'src/embedding/exports/memory/streaming.rs': ['masked_store', 'clflush'],
  'src/embedding/exports/system/protection.rs': ['protection'],
  'src/embedding/exports/system/far_control.rs': ['far_control'],
  'src/embedding/exports/events/software_events.rs': ['software_event'],
};
const files = [],
  wrappers = [],
  references = [];
for (const [path, names] of Object.entries(families)) {
  const bytes = await readFile(resolve(root, path)),
    source = bytes.toString();
  files.push({ path, sha256: hash(bytes) });
  for (const name of names)
    for (const observed of name === 'clflush' ? [false] : [false, true]) {
      const symbol = `core_${name}${observed ? '_observed' : ''}`;
      const anchor = `pub extern "C" fn ${symbol}(`,
        start = source.indexOf(anchor);
      const parameters = name === 'clflush' ? ['PAGED'] : ['OBSERVED', 'PAGED'];
      if (name === 'store') parameters.push('STACK_QWORD');
      const constArguments = parameters.map((parameter) => parameter === 'OBSERVED' && observed);
      const dispatch = `core_${name}_impl::<${constArguments.join(', ')}>`;
      const end = source.indexOf('\n}', start);
      if (start < 0 || end < start || !source.slice(start, end).includes(dispatch))
        throw Error(symbol + ' no longer explicitly specializes PAGED=false');
      const signature = `fn core_${name}_impl<${parameters.map((parameter) => `const ${parameter}: bool`).join(', ')}>`;
      if (!source.includes(signature)) throw Error(name + ' lost const paging specialization');
      wrappers.push({
        export: symbol,
        path,
        line: source.slice(0, start).split('\n').length,
        dispatch,
        pagingIsCompileTimeFalse: true,
      });
    }
}
for (const [path, anchors] of Object.entries({
  'src/embedding/exports/mod.rs': ['macro_rules! guest_access', 'macro_rules! guest_semantics'],
  'src/dbt/mod.rs': [
    'compile_with_paging(image, context, None, entry, limits, observed)',
    'let emit = if image.has_mmio() {\n        wasm::emit_mmio_profile\n    } else {\n        wasm::emit_profile',
    'emit(&unit, observed, paging.is_some(), max_artifact_bytes)',
  ],
  'src/dbt/wasm/artifact/placement.rs': ['let observed = observed || paged;'],
  'src/dbt/wasm/artifact/mod.rs': ['let access_reason = (segmented || paged).then'],
  'src/dbt/wasm/execution/boundary.rs': ['if self.paged {'],
  'src/embedding/exports/events/system_flags.rs': ['fn boundary_observed(&self)'],
  'sdk/src/wasm/core.ts': [
    'bindArtifactImports(this.#exports, { observed, paged: this.isPaged',
    'run(this.#statePointer, budget',
  ],
  'sdk/src/wasm/artifact-imports.ts': [
    'if (paged)',
    'observed ? observedPagedBindings : pagedBindings',
  ],
  'src/memory/access/mod.rs': [
    'fn preflight<const PHYSICAL: bool>',
    'self.preflight::<false>(address, width, Access::Read',
    'self.preflight::<true>(address, width, Access::Read',
  ],
  'src/x86/memory/access.rs': ['impl GuestMemory for Image'],
  'src/embedding/exports/execution/paged_decode.rs': ['fn paged_decode(', 'fn encoding_current('],
  'src/embedding/exports/memory/paging/mod.rs': ['pub extern "C" fn core_paged_debug_begin('],
  'src/x86/memory/paging/walk/mod.rs': [
    'walk_with::<false, false>',
    'walk_with::<true, FETCH>',
    'actual_walk::<true>',
  ],
})) {
  const bytes = await readFile(resolve(root, path)),
    source = bytes.toString();
  files.push({ path, sha256: hash(bytes) });
  for (const anchor of anchors) {
    const at = source.indexOf(anchor);
    if (at < 0) throw Error('missing source proof anchor: ' + anchor);
    references.push({ path, anchor, line: source.slice(0, at).split('\n').length });
  }
}
const evidence = {
  at: new Date().toISOString(),
  manifest: await manifest(root),
  files,
  wrappers,
  references,
  findings: [
    'The recorded linear memory and system exports instantiate their helpers with PAGED=false, including the observed variants where available.',
    'Linear and physical backing preflight select the address domain through const PHYSICAL.',
    'The compiler selects the paged artifact profile from the optional paging state.',
  ],
};
await mkdir(resolve(root, 'build/verification'), { recursive: true });
const output = resolve(root, 'build/verification/paging-source-proof.json');
await writeFile(output, encode(evidence));
console.log({
  output,
  helperExports: wrappers.length,
  families: Object.values(families).flat().length,
});
