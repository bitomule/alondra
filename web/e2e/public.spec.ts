import { test, expect } from '@playwright/test';
for (const width of [320, 375, 414, 768, 1440]) {
 test(`public pages fit at ${width}px`, async ({ page }) => {
  await page.setViewportSize({ width, height: 1000 });
  for (const path of ['/', '/privacy', '/install']) {
   const response = await page.goto(path);
   expect(response?.status()).toBe(200);
   await expect(page.locator('h1')).toBeVisible();
   expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
   // overflow:clip can hide mistakes: also inspect actual content bounds.
   const outside = await page.locator('main, main h1, main h2, main p, nav a, pre').evaluateAll(nodes => nodes.filter(node => {
    const box = node.getBoundingClientRect(); return box.left < -1 || box.right > innerWidth + 1;
   }).map(node => node.tagName));
   expect(outside).toEqual([]);
  }
 });
}
test('installation and privacy are reachable and usable with keyboard', async ({ page }) => {
 await page.goto('/');
 await page.keyboard.press('Tab');
 await expect(page.getByRole('link', { name: 'Saltar al contenido' })).toBeFocused();
 await page.getByRole('link', { name: 'Cómo empezar' }).click();
 await expect(page.locator('pre').first()).toHaveText('brew install bitomule/tap/alondra');
 await page.getByRole('link', { name: 'Privacidad', exact: true }).click();
 await expect(page.getByRole('heading', { name: 'Qué conoce el servicio' })).toBeVisible();
});
test('public preview', async ({ page }, info) => {
 await page.setViewportSize({ width: 1440, height: 1100 });
 await page.goto('/');
 await page.screenshot({ path: info.outputPath('home-desktop.png'), fullPage: true });
 await page.setViewportSize({ width: 375, height: 900 });
 await page.screenshot({ path: info.outputPath('home-mobile.png'), fullPage: true });
});
