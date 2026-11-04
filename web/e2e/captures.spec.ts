import { test, type Page } from '@playwright/test';
import { join } from 'node:path';

// Opt-in: ALONDRA_WEB_CAPTURES=/some/dir npx playwright test captures
const out = process.env.ALONDRA_WEB_CAPTURES;
test.skip(!out, 'set ALONDRA_WEB_CAPTURES to a directory to write review captures');
test.setTimeout(120000);

async function settle(page: Page) {
 await page.evaluate(async () => {
  for (let y = 0; y < document.body.scrollHeight; y += innerHeight / 2) { scrollTo(0, y); await new Promise(r => setTimeout(r, 120)); }
  scrollTo(0, 0);
 });
 await page.waitForTimeout(600);
}

for (const colorScheme of ['light', 'dark'] as const) {
 for (const [name, viewport] of [['desktop', { width: 1440, height: 900 }], ['mobile', { width: 390, height: 844 }]] as const) {
  test(`capture ${name} ${colorScheme}`, async ({ browser }) => {
   const context = await browser.newContext({ viewport, colorScheme, reducedMotion: 'reduce', deviceScaleFactor: 2 });
   const page = await context.newPage();
   for (const [path, slug] of [['/', 'home'], ['/how-it-works', 'how-it-works'], ['/install', 'install'], ['/docs', 'docs'], ['/security', 'security'], ['/license', 'license']] as const) {
    await page.goto(path);
    await settle(page);
    await page.screenshot({ path: join(out!, `${slug}-${name}-${colorScheme}.png`), fullPage: true });
    if (path === '/') await page.screenshot({ path: join(out!, `hero-${name}-${colorScheme}.png`) });
   }
   await context.close();
  });
 }
}
