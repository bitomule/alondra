import { test, expect } from '@playwright/test';
import { readdirSync } from 'node:fs';

const PUBLIC = ['/', '/privacy', '/install'];

for (const colorScheme of ['light', 'dark'] as const) {
 for (const width of [320, 375, 414, 768, 1024, 1440]) {
  test(`public pages fit at ${width}px (${colorScheme})`, async ({ page }) => {
   await page.emulateMedia({ colorScheme });
   await page.setViewportSize({ width, height: 900 });
   for (const path of PUBLIC) {
    const response = await page.goto(path);
    expect(response?.status()).toBe(200);
    await expect(page.locator('h1')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    // overflow:clip can hide mistakes: also inspect actual content bounds.
    const outside = await page.locator('main h1, main h2, main h3, main p, main li, header a, footer a, pre, input, button').evaluateAll(nodes => nodes.filter(node => {
     const box = node.getBoundingClientRect();
     const hidden = box.width === 0 || getComputedStyle(node).visibility === 'hidden' || node.closest('[aria-hidden="true"]');
     return !hidden && (box.left < -1 || box.right > innerWidth + 1);
    }).map(node => `${node.tagName}.${node.className}: ${node.textContent?.slice(0, 40)}`));
    expect(outside, path).toEqual([]);
    const twoLineLinks = await page.locator('header a, footer nav a, button').evaluateAll(nodes => nodes.filter(node => {
     const lines = new Set<number>();
     const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
     for (let text = walker.nextNode(); text; text = walker.nextNode()) {
      if (!text.textContent?.trim()) continue;
      const range = document.createRange();
      range.selectNodeContents(text);
      for (const rect of range.getClientRects()) if (rect.width > 2) lines.add(Math.round(rect.bottom / 6));
     }
     return lines.size > 1;
    }).map(node => node.textContent));
    expect(twoLineLinks, path).toEqual([]);
   }
  });
 }
}

test('installation and privacy are reachable and usable with keyboard', async ({ page }) => {
 await page.goto('/');
 await page.keyboard.press('Tab');
 await expect(page.getByRole('link', { name: 'Saltar al contenido' })).toBeFocused();
 await page.getByRole('contentinfo').getByRole('link', { name: 'Instalar' }).click();
 await expect(page).toHaveURL(/\/install$/);
 await expect(page.locator('pre').first()).toHaveText('brew install bitomule/tap/alondra');
 await expect(page.locator('pre')).toHaveText(['brew install bitomule/tap/alondra', 'alondra setup', 'alondra pair', 'alondra doctor', 'alondra']);
 await page.getByRole('contentinfo').getByRole('link', { name: 'Privacidad', exact: true }).click();
 await expect(page.getByRole('heading', { name: 'Qué conoce el servicio' })).toBeVisible();
 await expect(page.getByRole('heading', { name: 'La lista de espera' })).toBeVisible();
});

test('the privacy copy says what is encrypted and what the service sees, and promises nothing more', async ({ page }) => {
 for (const path of ['/', '/privacy']) {
  await page.goto(path);
  const text = (await page.locator('main').innerText()).toLowerCase();
  expect(text).toContain('cifrado entre dispositivos autorizados');
  expect(text).toMatch(/aprobarlo desde ese mac/);
  expect(text).toContain('metadatos');
  expect(text).toContain('lista de espera');
  for (const promise of ['anónim', 'cero metadatos', 'sin metadatos', 'recuperar tus claves', 'siempre disponible', 'email cifrado', 'emails cifrados', 'te enviaremos un correo', 'recibirás un correo']) expect(text, `${path}: ${promise}`).not.toContain(promise);
 }
});

test('public and private pages never load each other’s styles, even after client navigation', async ({ page }) => {
 const sheets = () => page.locator('link[rel="stylesheet"]').evaluateAll(links => links.map(link => (link as HTMLLinkElement).href));
 await page.goto('/');
 expect((await sheets()).some(href => href.includes('public-layout'))).toBe(true);
 expect((await sheets()).some(href => href.includes('private-layout'))).toBe(false);
 await page.goto('/login-error');
 expect((await sheets()).some(href => href.includes('private-layout'))).toBe(true);
 expect((await sheets()).some(href => href.includes('public-layout'))).toBe(false);
 await page.getByRole('link', { name: 'Privacidad' }).first().click();
 await expect(page.getByRole('heading', { name: 'Qué conoce el servicio' })).toBeVisible();
 await expect.poll(async () => (await sheets()).some(href => href.includes('private-layout'))).toBe(false);
 expect((await sheets()).some(href => href.includes('public-layout'))).toBe(true);
});

test('the build only ships asset types the relay serves', () => {
 const allowed = new Set(['js', 'css', 'svg', 'png', 'woff2']);
 const unexpected = readdirSync('build/client/assets').filter(name => !allowed.has(name.split('.').pop() ?? '') || !/^[A-Za-z0-9._-]+$/.test(name));
 expect(unexpected).toEqual([]);
});

test('the real icon is the favicon and the brand mark', async ({ page }) => {
 await page.goto('/');
 await expect(page.locator('link[rel="icon"]')).toHaveAttribute('href', /\/assets\/favicon-.*\.svg$/);
 await expect(page.locator('link[rel="apple-touch-icon"]')).toHaveAttribute('href', /\/assets\/apple-touch-icon-.*\.png$/);
 await expect(page.getByRole('link', { name: 'Alondra, inicio' }).locator('svg.lark')).toBeVisible();
});

test('the flow animation plays once in view, can be paused, and ends on its final frame', async ({ page }) => {
 await page.goto('/');
 const canvas = page.locator('.score-canvas');
 await expect(canvas).toHaveAttribute('data-play', 'armed');
 await canvas.scrollIntoViewIfNeeded();
 await expect(canvas).toHaveAttribute('data-play', 'playing');
 const pause = page.getByRole('button', { name: 'Pausar' });
 await pause.click();
 await expect(page.getByRole('button', { name: 'Reanudar' })).toHaveAttribute('aria-pressed', 'true');
 await expect(canvas).toHaveAttribute('data-paused', 'true');
 await page.getByRole('button', { name: 'Reanudar' }).click();
 await expect(page.getByRole('button', { name: 'Ver otra vez' })).toBeVisible({ timeout: 15000 });
 await expect(canvas).toHaveAttribute('data-play', 'done');
 await expect(canvas.getByText('Fusionado con squash')).toBeVisible();
 await expect(canvas.getByText('Enviado a su nodo')).toBeVisible();
 await page.getByRole('button', { name: 'Ver otra vez' }).click();
 await expect(canvas).toHaveAttribute('data-play', 'playing');
});

test('with reduced motion nothing plays and every final state is visible at once', async ({ browser }) => {
 const context = await browser.newContext({ reducedMotion: 'reduce' });
 const page = await context.newPage();
 await page.goto('/');
 const canvas = page.locator('.score-canvas');
 await canvas.scrollIntoViewIfNeeded();
 await page.waitForTimeout(500);
 await expect(canvas).toHaveAttribute('data-play', 'static');
 await expect(canvas.getByText('Fusionado con squash')).toBeVisible();
 await expect(page.getByRole('button', { name: 'Pausar' })).toHaveCount(0);
 const animated = await page.locator('.hero-title > span, .wing').evaluateAll(nodes => nodes.filter(node => getComputedStyle(node).animationName !== 'none').length);
 expect(animated).toBe(0);
 await context.close();
});
