import { defineConfig } from '@playwright/test';

const origin = new URL(process.env.PUBLIC_WEB_ORIGIN ?? '');
if (!['http:', 'https:'].includes(origin.protocol) || origin.username || origin.password || origin.href !== `${origin.origin}/`) {
 throw new Error('PUBLIC_WEB_ORIGIN must be an HTTP(S) origin without credentials, a path or a query');
}
const sourceCommit = process.env.WEB_SOURCE_COMMIT ?? '';
if (!/^[a-f0-9]{40}$/.test(sourceCommit)) throw new Error('Set WEB_SOURCE_COMMIT to the public commit used to build WEB_BUILD_DIR');

export default defineConfig({
 testDir: './e2e',
 testMatch: 'public-deployment.spec.ts',
 workers: 1,
 retries: 0,
 timeout: 60_000,
 reporter: [['list'], ['json', { outputFile: 'test-results/public-deployment.json' }]],
 metadata: { origin: origin.origin, sourceCommit },
 use: { baseURL: origin.origin, browserName: 'chromium', serviceWorkers: 'block', trace: 'off' },
});
