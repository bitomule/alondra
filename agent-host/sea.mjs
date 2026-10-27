import { copyFileSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { inject } from 'postject';

const node = process.env.JS_BINARY__NODE_BINARY;
if (!node) throw new Error('Hermetic Node path required');
if (!process.env.JS_BINARY__EXECROOT) throw new Error('Bazel execroot required');
const [bundle, output, manifest, entitlements] = process.argv.slice(2).map((path) => resolve(process.env.JS_BINARY__EXECROOT, path));
if (process.platform !== 'darwin' || process.arch !== 'arm64') throw new Error('The SEA host requires macOS arm64');
const run = (command, args) => {
  const result = spawnSync(command, args, { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed: ${result.status}`);
};
writeFileSync(`${output}.config`, JSON.stringify({ main: bundle, output: `${output}.blob`, disableExperimentalSEAWarning: true, execArgvExtension: entitlements ? 'env' : 'none', useSnapshot: false, useCodeCache: false }));
run(node, ['--experimental-sea-config', `${output}.config`]);
copyFileSync(node, output);
run('/usr/bin/codesign', ['--remove-signature', output]);
await inject(output, 'NODE_SEA_BLOB', readFileSync(`${output}.blob`), { sentinelFuse: 'NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2', machoSegmentName: 'NODE_SEA' });
// A release host runs under the hardened runtime, as the release signs it with its Developer ID.
run('/usr/bin/codesign', ['--sign', '-', ...(entitlements ? ['--options', 'runtime', '--entitlements', entitlements] : []), output]);
writeFileSync(manifest, JSON.stringify({ laboratory: !entitlements, sdk: process.argv[6] ?? '0.3.300', node: process.version, platform: process.platform, architecture: process.arch, sha256: createHash('sha256').update(readFileSync(output)).digest('hex'), bundle_sha256: createHash('sha256').update(readFileSync(bundle)).digest('hex'), signature: 'ad-hoc', developer_id: false }, null, 2));
