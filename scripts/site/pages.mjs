import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, posix } from 'node:path';
import hljs from 'highlight.js/lib/core';
import c from 'highlight.js/lib/languages/c';
import typescript from 'highlight.js/lib/languages/typescript';
import { renderBenchmarks } from './benchmarks.mjs';

hljs.registerLanguage('c', c);
hljs.registerLanguage('typescript', typescript);

export async function writeSitePages(source, output, wetInkCompiler) {
  const [masthead, canvas, controls, reference] = await Promise.all([
    readFile(join(source, 'masthead.html'), 'utf8'),
    readFile(join(source, 'wet-ink/canvas.html'), 'utf8'),
    readFile(join(source, 'wet-ink/controls.html'), 'utf8'),
    readFile(join(source, 'docs/reference.html'), 'utf8'),
  ]);
  for (const page of [
    'index.html',
    'under-the-ink/index.html',
    'docs/index.html',
    'benchmarks/index.html',
  ]) {
    let html = await readFile(join(source, page), 'utf8');
    if (page === 'benchmarks/index.html') {
      const results = JSON.parse(await readFile(join(source, 'benchmarks/results.json'), 'utf8'));
      html = renderBenchmarks(html, results);
    }
    if (page === 'docs/index.html') {
      html = html.replace('<!-- docs-reference -->', () => reference.trimEnd());
      for (const [marker, name] of html.matchAll(/<!-- example:([^ ]+) -->/g)) {
        if (!/^(?:[\w-]+\/)*[\w-]+\.(?:ts|c)$/.test(name))
          throw Error(`invalid docs example: ${name}`);
        const code = await readFile(join(source, 'docs/examples', name), 'utf8');
        const language = name.endsWith('.c') ? 'c' : 'typescript';
        html = html.replace(marker, () => hljs.highlight(code.trimEnd(), { language }).value);
      }
    }
    const destination = join(output, page);
    const siteRoot = posix.relative(posix.dirname(page), '.') || '.';
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(
      destination,
      html
        .replace('<!-- site-masthead -->', () =>
          masthead
            .replaceAll('{{root}}', siteRoot)
            .replaceAll('{{docsCurrent}}', String(page.startsWith('docs/')))
            .replaceAll('{{benchmarksCurrent}}', String(page.startsWith('benchmarks/')))
            .trim(),
        )
        .replace('<!-- wet-ink-compiler -->', () => {
          const base = JSON.stringify(`${siteRoot}/wet-ink/`);
          return `<script>{const wetInkCompilerBase = new URL(${base}, document.baseURI);${wetInkCompiler}}</script>`;
        })
        .replace('<!-- wet-ink-canvas -->', () => canvas.trim())
        .replace('<!-- wet-ink-controls -->', () => controls.trim()),
    );
  }
}
