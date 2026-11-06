import { test, expect, type Response } from '@playwright/test';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { resolve, sep } from 'node:path';
import { expectLegibleMenu } from './public-menu';

const pages = ['/', '/how-it-works', '/install', '/docs', '/security', '/privacy', '/license'];
const links = [
 ['How it works', '/how-it-works'], ['Install', '/install'], ['Documentation', '/docs'],
 ['Security and privacy', '/security'], ['License', '/license'],
];

test.beforeEach(async ({ page }) => {
 await page.route('**/*', route => ['GET', 'HEAD'].includes(route.request().method()) ? route.continue() : route.abort());
});

test('the deployed scripts and styles match the expected public build', async ({ page }) => {
 const assets: Response[] = [];
 page.on('response', response => {
  if (['script', 'stylesheet'].includes(response.request().resourceType())) assets.push(response);
 });
 const response = await page.goto('/');
 expect(response?.status()).toBe(200);
 await expect(page.locator('h1')).toBeVisible();
 expect(assets.some(asset => asset.request().resourceType() === 'script')).toBe(true);
 expect(assets.some(asset => asset.request().resourceType() === 'stylesheet')).toBe(true);
 const build = resolve(process.env.WEB_BUILD_DIR ?? 'build/client');
 const origin = new URL(page.url()).origin;
 const hashes: Record<string, string> = {};
 for (const asset of assets) {
  const url = new URL(asset.url());
  expect(url.origin, 'assets must come from the checked origin').toBe(origin);
  expect(asset.status(), url.pathname).toBe(200);
  const file = resolve(build, `.${decodeURIComponent(url.pathname)}`);
  expect(file.startsWith(build + sep), 'asset must stay inside the expected build').toBe(true);
  const digest = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
  hashes[url.pathname] = digest(await asset.body());
  expect(hashes[url.pathname], url.pathname).toBe(digest(readFileSync(file)));
 }
 await test.info().attach('asset-hashes', { body: JSON.stringify(hashes, null, 2), contentType: 'application/json' });
});

for (const colorScheme of ['light', 'dark'] as const) {
 test(`public routes and mobile navigation work (${colorScheme})`, async ({ page }) => {
  await page.emulateMedia({ colorScheme, reducedMotion: 'reduce' });
  await page.setViewportSize({ width: 390, height: 844 });
  for (const path of pages) {
   const response = await page.goto(path);
   expect(response?.status(), path).toBe(200);
   await expect(page.locator('h1')).toBeVisible();
   await page.getByText('Menu', { exact: true }).click();
   await expectLegibleMenu(page);
   if (path === '/') await test.info().attach(`mobile-menu-${colorScheme}`, {
    body: await page.screenshot(), contentType: 'image/png',
   });
  }
  for (const [name, path] of links) {
   await page.goto('/');
   await page.getByText('Menu', { exact: true }).click();
   await page.getByRole('navigation', { name: 'Main, compact' }).getByRole('link', { name, exact: true }).click();
   await expect(page).toHaveURL(new RegExp(`${path}$`));
   await expect(page.locator('h1')).toBeVisible();
  }
 });
}
