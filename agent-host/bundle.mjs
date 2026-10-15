import { build } from 'esbuild';
import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const [entry, outputArgument, metadataArgument] = process.argv.slice(2);
if (!process.env.JS_BINARY__EXECROOT) throw new Error('Bazel execroot required');
const output = resolve(process.env.JS_BINARY__EXECROOT, outputArgument);
const metadata = resolve(process.env.JS_BINARY__EXECROOT, metadataArgument);
const result = await build({
  entryPoints: [entry], outfile: output, bundle: true, platform: 'node', target: 'node24', format: 'cjs',
  sourcemap: false, minify: true, metafile: true,
  define: { 'import.meta.url': '__alondraImportMetaUrl' },
  banner: { js: "const __alondraImportMetaUrl = require('node:url').pathToFileURL(__filename).href;" },
});
const external = Object.values(result.metafile.outputs).flatMap((output) => output.imports.filter((item) => item.external).map((item) => item.path));
const { builtinModules } = await import('node:module');
const builtins = new Set(builtinModules.flatMap((name) => [name, `node:${name}`]));
for (const name of external) if (!builtins.has(name)) throw new Error(`Unclosed bundle dependency: ${name}`);
writeFileSync(metadata, JSON.stringify({ node: process.version, external: [...new Set(external)].sort(), metafile: result.metafile }, null, 2));
