import { test, expect } from '@playwright/test';

for (const preview of [false, true]) for (const locale of ['en', 'es']) {
  test(`proposal ${preview ? 'preview' : 'published'} ${locale}: responsive, localized and working waitlist`, async ({ page }) => {
    await page.goto(preview ? `/proposal/${locale}/` : `/?lang=${locale}`);
    if (!preview) await expect(page.locator('meta[name=robots]')).toHaveCount(0);
    await expect(page.locator('html')).toHaveAttribute('lang', locale);
    await expect(page.locator('h1')).toHaveText(locale === 'en' ? 'The orchestrator for your agents.' : 'El orquestador para tus agentes.');
    for (const width of [320, 375, 414, 768, 1440]) {
      await page.setViewportSize({ width, height: 900 });
      await page.evaluate(() => document.fonts.ready);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      const clipped = await page.locator('.proposal a, .proposal button').evaluateAll(nodes => nodes.filter(node => node.getBoundingClientRect().width > 0 && node.scrollWidth > node.clientWidth + 1).map(node => node.textContent));
      expect(clipped).toEqual([]);
      await page.screenshot({ path: `/private/tmp/alondra-proposal-${locale}-${width}.png`, fullPage: true });
    }
    const forms = page.locator('form');
    await forms.first().getByRole('button').click();
    await expect(page.getByRole('alert')).toContainText(locale === 'en' ? 'valid email' : 'dirección válida');
    let body: unknown;
    await page.route('**/api/waitlist', async route => { body = route.request().postDataJSON(); await route.fulfill({ status: 503 }); });
    await forms.first().getByRole('textbox').fill('proposal@example.com');
    await forms.first().getByRole('button').click();
    await expect(page.getByRole('alert')).toContainText(locale === 'en' ? 'could not add you' : 'No hemos podido');
    await page.unroute('**/api/waitlist');
    await page.route('**/api/waitlist', route => route.fulfill({ status: 429, headers: { 'Retry-After': '30' } }));
    await forms.first().getByRole('button').click();
    await expect(page.getByRole('alert')).toContainText(locale === 'en' ? '30 seconds' : '30 segundos');
    await page.unroute('**/api/waitlist');
    await page.route('**/api/waitlist', route => route.fulfill({ status: 202 }));
    await forms.first().getByRole('button').click();
    await expect(page.locator('.waitlist.is-done')).toHaveCount(1);
    await expect(page.getByRole('status')).toContainText(locale === 'en' ? 'on the list' : 'Estás en la lista');
    expect(body).toEqual({ email: 'proposal@example.com' });
    await page.getByRole('link', { name: locale === 'en' ? 'Español' : 'English', exact: true }).click();
    await expect(page.locator('html')).toHaveAttribute('lang', locale === 'en' ? 'es' : 'en');
  });
}
