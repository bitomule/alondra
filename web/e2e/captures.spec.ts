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
   for (const [path, slug] of [['/', 'home'], ['/privacy', 'privacy'], ['/install', 'install']] as const) {
    await page.goto(path);
    await settle(page);
    await page.screenshot({ path: join(out!, `${slug}-${name}-${colorScheme}.png`), fullPage: true });
    if (path === '/') {
     await page.screenshot({ path: join(out!, `hero-${name}-${colorScheme}.png`) });
     await page.locator('.score').scrollIntoViewIfNeeded();
     await page.screenshot({ path: join(out!, `score-${name}-${colorScheme}.png`) });
    }
   }
   await context.close();
  });
 }
}

test('capture the animations as video', async ({ browser }) => {
 const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, colorScheme: 'light', recordVideo: { dir: out!, size: { width: 1280, height: 800 } } });
 const page = await context.newPage();
 await page.goto('/');
 await page.waitForTimeout(3200);
 const scrollTo = (selector: string) => page.locator(selector).first().evaluate(el => el.scrollIntoView({ behavior: 'smooth', block: 'start' }));
 await scrollTo('.score');
 await page.waitForTimeout(11500);
 for (const id of ['pides', 'reparte', 'flota', 'tarjetas', 'decides']) {
  await page.locator(`.stage[data-index]`).nth(['pides', 'reparte', 'flota', 'tarjetas', 'decides'].indexOf(id)).evaluate(el => el.scrollIntoView({ behavior: 'smooth', block: 'center' }));
  await page.waitForTimeout(1700);
 }
 await scrollTo('.autonomy');
 await page.waitForTimeout(1500);
 await context.close();
});
