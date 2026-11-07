import { test, expect } from '@playwright/test';
import { readdirSync } from 'node:fs';

const PUBLIC = ['/', '/how-it-works', '/install', '/docs', '/security', '/license'];

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
     const hidden = box.width === 0 || getComputedStyle(node).visibility === 'hidden' || node.closest('[aria-hidden="true"]') || node.closest('.demo-screen, .copy-command');
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

test('the first screen shows the headline, one sentence, the actions and the command at 1280 by 800', async ({ page }) => {
 await page.setViewportSize({ width: 1280, height: 800 });
 await page.goto('/');
 await expect(page.getByRole('heading', { level: 1 })).toHaveText('A team of agents. One inbox.');
 for (const locator of [page.locator('.hero-lede'), page.getByRole('link', { name: 'Install with Homebrew' }), page.getByRole('link', { name: 'Join the Alondra Cloud waitlist' }), page.locator('.copy-command')]) {
  await expect(locator).toBeVisible();
  const box = await locator.boundingBox();
  expect(box!.y + box!.height).toBeLessThanOrEqual(800);
 }
 await expect(page.locator('.copy-command code')).toContainText('brew install bitomule/tap/alondra');
});

test('the install command can be copied', async ({ page, context }) => {
 await context.grantPermissions(['clipboard-read', 'clipboard-write']);
 await page.goto('/');
 await page.getByRole('button', { name: /^Copy the command/ }).click();
 await expect(page.getByRole('button', { name: /^Copy the command/ })).toHaveText('Copied');
 expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('brew install bitomule/tap/alondra');
});

test('the skip link and the pages are reachable with the keyboard', async ({ page }) => {
 await page.goto('/');
 await page.keyboard.press('Tab');
 await expect(page.getByRole('link', { name: 'Skip to content' })).toBeFocused();
 await page.getByRole('contentinfo').getByRole('link', { name: 'Install', exact: true }).click();
 await expect(page).toHaveURL(/\/install$/);
 await expect(page.locator('.command-line')).toHaveText(['brew install bitomule/tap/alondra', 'alondra', 'alondra doctor', 'alondra doctor --fix']);
 await page.getByRole('contentinfo').getByRole('link', { name: 'Security and privacy', exact: true }).click();
 await expect(page.getByRole('heading', { name: 'What Alondra Cloud sees' })).toBeVisible();
});

test('the header has a GitHub icon that links to the public repository, on desktop and on a phone', async ({ page }) => {
 for (const width of [1280, 390]) {
  await page.setViewportSize({ width, height: 844 });
  await page.goto('/');
  const icon = page.getByRole('banner').getByRole('link', { name: 'Alondra on GitHub' });
  await expect(icon).toBeVisible();
  await expect(icon).toHaveAttribute('href', 'https://github.com/bitomule/alondra');
  const box = await icon.boundingBox();
  expect(box!.width).toBeGreaterThanOrEqual(44);
  expect(box!.height).toBeGreaterThanOrEqual(44);
  await icon.focus();
  expect(await icon.evaluate(node => getComputedStyle(node).outlineStyle)).not.toBe('none');
 }
 await page.getByText('Menu', { exact: true }).click();
 await expect(page.getByRole('navigation', { name: 'Main, compact' }).getByRole('link', { name: 'GitHub' })).toHaveAttribute('href', 'https://github.com/bitomule/alondra');
});

test('the header has no sign-in link; existing accounts sign in from the footer', async ({ page }) => {
 for (const width of [1280, 390]) {
  await page.setViewportSize({ width, height: 844 });
  await page.goto('/');
  await expect(page.getByRole('banner').getByRole('link', { name: 'Sign in' })).toHaveCount(0);
 }
 await page.getByText('Menu', { exact: true }).click();
 await expect(page.getByRole('navigation', { name: 'Main, compact' }).getByRole('link', { name: 'Sign in' })).toHaveCount(0);
 await expect(page.getByRole('contentinfo').getByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/auth/login');
});

test('the landing says in one place that Alondra Cloud is by invitation and how to ask for one', async ({ page }) => {
 await page.goto('/');
 const cloud = page.locator('#cloud');
 await expect(cloud.getByRole('heading', { name: 'Alondra Cloud is by invitation.' })).toBeVisible();
 await expect(cloud).toContainText('An invitation lets you create your account.');
 await expect(cloud).toContainText('To ask for one, leave your email.');
});

test('the compact menu opens and lists every page', async ({ page }) => {
 await page.setViewportSize({ width: 390, height: 844 });
 await page.goto('/');
 await expect(page.getByRole('navigation', { name: 'Main', exact: true })).toBeHidden();
 await page.getByText('Menu', { exact: true }).click();
 for (const name of ['How it works', 'Install', 'Documentation', 'Security and privacy', 'License']) await expect(page.getByRole('navigation', { name: 'Main, compact' }).getByRole('link', { name })).toBeVisible();
});

for (const colorScheme of ['light', 'dark'] as const) {
 for (const path of ['/', '/install']) {
  test(`on an iPhone the compact menu links are legible on ${path} (${colorScheme})`, async ({ page }) => {
   await page.emulateMedia({ colorScheme });
   await page.setViewportSize({ width: 390, height: 844 });
   await page.goto(path);
   await page.getByText('Menu', { exact: true }).click();
   const menu = page.getByRole('navigation', { name: 'Main, compact' });
   const ratios = await menu.evaluate(nav => {
    const rgb = (css: string) => {
     const context = document.createElement('canvas').getContext('2d')!;
     context.fillStyle = css;
     context.fillRect(0, 0, 1, 1);
     return Array.from(context.getImageData(0, 0, 1, 1).data.slice(0, 3));
    };
    const luminance = (css: string) => {
     const [r, g, b] = rgb(css).map(channel => { const value = channel / 255; return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4; });
     return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const background = getComputedStyle(nav).backgroundColor;
    return Array.from(nav.querySelectorAll('a')).map(link => {
     const [light, dark] = [luminance(getComputedStyle(link).color), luminance(background)].sort((a, b) => b - a);
     return { name: link.textContent, ratio: (light + 0.05) / (dark + 0.05) };
    });
   });
   expect(ratios.map(item => item.name)).toEqual(['How it works', 'Install', 'Documentation', 'Security and privacy', 'License', 'GitHub']);
   for (const { name, ratio } of ratios) expect(ratio, `${name} contrast`).toBeGreaterThanOrEqual(4.5);
   for (const link of await menu.getByRole('link').all()) await expect(link).toBeVisible();
  });
 }
}

test('the security copy says what the service sees and what it never receives, and promises nothing more', async ({ page }) => {
 await page.goto('/security');
 const text = (await page.locator('main').innerText()).toLowerCase();
 expect(text).toContain('what alondra cloud never sees');
 expect(text).toContain('what alondra cloud sees');
 expect(text).toContain('the network addresses of both ends, when they talk and how much they send');
 expect(text).toContain('six-character code');
 expect(text).toContain('revoke it in two places');
 expect(text).not.toContain('measure what it costs');
 for (const promise of ['anonymous', 'zero metadata', 'no metadata', 'forward secrecy', 'unhackable', 'military', 'we never see']) expect(text, promise).not.toContain(promise);
});

test('what is not built yet is marked as coming soon, and nothing sells plans, prices or self-hosting', async ({ page }) => {
 await page.goto('/');
 expect(await page.locator('.soon').allInnerTexts()).toEqual(['Coming soon', 'Coming soon', 'Coming soon']);
 await page.goto('/how-it-works');
 expect(await page.locator('.soon').allInnerTexts()).toEqual(['Coming soon', 'Coming soon', 'Coming soon']);
 for (const path of PUBLIC) {
  await page.goto(path);
  const text = (await page.locator('body').innerText()).toLowerCase();
  expect(text, `${path}: price`).not.toMatch(/\$\d/);
  for (const banned of ['pricing', '/month', '€', 'self-host', 'seamless', 'supercharge', 'unleash', 'revolutioniz', 'next-generation', '—']) expect(text, `${path}: ${banned}`).not.toContain(banned);
 }
});

test('no public page names the transport library or the Live Activity, in text, alt text, labels or metadata', async ({ page }) => {
 for (const path of PUBLIC) {
  await page.goto(path);
  const said = await page.evaluate(() => [
   document.body.innerText,
   document.title,
   ...Array.from(document.querySelectorAll('svg text'), node => node.textContent ?? ''),
   ...Array.from(document.querySelectorAll('[alt], [aria-label], [title], meta[content]'), node => ['alt', 'aria-label', 'title', 'content'].map(name => node.getAttribute(name) ?? '').join(' ')),
  ].join('\n').toLowerCase());
  for (const banned of ['iroh', 'live activit']) expect(said, `${path}: ${banned}`).not.toContain(banned);
 }
});

test('body copy is set in the sans text face at reading size', async ({ page }) => {
 for (const path of ['/', '/how-it-works', '/security']) {
  await page.goto(path);
  const body = await page.locator('main p').first().evaluate(node => { const style = getComputedStyle(node); return { family: style.fontFamily, size: parseFloat(style.fontSize), weight: Number(style.fontWeight) }; });
  expect(body.family, path).toMatch(/^"?Archivo Alondra/);
  expect(body.size, path).toBeGreaterThanOrEqual(18);
  expect(body.weight, path).toBeGreaterThanOrEqual(400);
 }
});

test('under reduced motion the replay shows its final state and nothing moves', async ({ page }) => {
 await page.emulateMedia({ reducedMotion: 'reduce' });
 await page.goto('/');
 const demo = page.locator('.demo-screen');
 await demo.scrollIntoViewIfNeeded();
 await expect(demo.locator('.demo-after').first()).toBeVisible();
 await expect(demo.locator('.demo-before').first()).toHaveCSS('opacity', '0');
 await expect(page.locator('.flow .flow-lit').first()).toHaveCSS('animation-name', 'none');
 await page.goto('/how-it-works');
 await expect(page.locator('.diagram .packets').first()).toBeHidden();
});

test('with motion allowed the replay plays once it is visible', async ({ page }) => {
 await page.setViewportSize({ width: 1280, height: 800 });
 await page.goto('/');
 await expect(page.locator('.demo')).toHaveClass(/demo-armed/);
 await page.locator('.demo').scrollIntoViewIfNeeded();
 await expect(page.locator('.demo')).toHaveClass(/demo-playing/);
 await expect(page.locator('.demo-ok .demo-after >> nth=0')).toHaveCSS('opacity', '1', { timeout: 10000 });
});

test('every moving figure can be paused, and pausing stops its motion', async ({ page }) => {
 await page.emulateMedia({ reducedMotion: 'no-preference' });
 await page.goto('/');
 for (const [figure, what] of [['.flow', 'flow'], ['.demo', 'replay'], ['.journey', 'highlights'], ['.diagram', 'envelopes']] as const) {
  await page.locator(figure).scrollIntoViewIfNeeded();
  const pause = page.getByRole('button', { name: `Pause the ${what}` });
  await pause.click();
  await expect(page.getByRole('button', { name: `Play the ${what}` })).toBeVisible();
 }
 await expect(page.locator('.flow-playing .flow-lit').first()).toHaveCSS('animation-play-state', 'paused');
 await expect(page.locator('.journey-playing .mark-1')).toHaveCSS('animation-play-state', 'paused');
 await expect(page.locator('.demo-playing .demo-plan')).toHaveCSS('animation-play-state', 'paused');
 expect(await page.locator('.diagram-wide').evaluate(svg => (svg as SVGSVGElement).animationsPaused())).toBe(true);
});

test('under reduced motion there is nothing to pause', async ({ page }) => {
 await page.emulateMedia({ reducedMotion: 'reduce' });
 await page.goto('/');
 await expect(page.getByRole('button', { name: /^(Pause|Play) the/ })).toHaveCount(0);
});

for (const width of [320, 390]) {
 test(`at ${width}px no command, code block or terminal replay is cut or scrolls sideways`, async ({ page }) => {
  await page.setViewportSize({ width, height: 844 });
  for (const path of PUBLIC) {
   await page.goto(path);
   const cut = await page.locator('.copy-command code, pre, .demo-screen, .demo-line > span').evaluateAll(nodes => nodes.filter(node => node.scrollWidth > node.clientWidth + 1).map(node => `${node.className || node.tagName}: ${node.textContent?.slice(0, 40)}`));
   expect(cut, path).toEqual([]);
  }
  await page.goto('/');
  const box = await page.locator('.hero .copy-command').boundingBox();
  const button = await page.locator('.hero .copy-command button').boundingBox();
  expect(button!.x + button!.width).toBeLessThanOrEqual(box!.x + box!.width + 1);
 });
}

test('on a phone the waitlist field is one line tall', async ({ page }) => {
 await page.setViewportSize({ width: 390, height: 844 });
 await page.goto('/');
 const box = await page.locator('#cloud input[type=email]').boundingBox();
 expect(box!.height).toBeLessThan(64);
});

test('public and private pages never load each other’s styles, even after client navigation', async ({ page }) => {
 const sheets = () => page.locator('link[rel="stylesheet"]').evaluateAll(links => links.map(link => (link as HTMLLinkElement).href));
 await page.goto('/');
 expect((await sheets()).some(href => /\/(?:public|public-layout)-[^/]+\.css$/.test(href))).toBe(true);
 expect((await sheets()).some(href => href.includes('private-layout'))).toBe(false);
 await page.goto('/login-error');
 expect((await sheets()).some(href => href.includes('private-layout'))).toBe(true);
 expect((await sheets()).some(href => /\/(?:public|public-layout)-[^/]+\.css$/.test(href))).toBe(false);
 await page.getByRole('link', { name: 'Privacidad' }).first().click();
 await expect(page.getByRole('heading', { name: 'What Alondra Cloud sees' })).toBeVisible();
 await expect.poll(async () => (await sheets()).some(href => href.includes('private-layout'))).toBe(false);
 expect((await sheets()).some(href => /\/(?:public|public-layout)-[^/]+\.css$/.test(href))).toBe(true);
});

test('the build only ships asset types the relay serves', () => {
 const allowed = new Set(['js', 'css', 'svg', 'png', 'woff2']);
 const unexpected = readdirSync('build/client/assets').filter(name => !allowed.has(name.split('.').pop() ?? '') || !/^[A-Za-z0-9._-]+$/.test(name));
 expect(unexpected).toEqual([]);
});

test('the real icon is the favicon and the brand mark, and every page has one canonical address', async ({ page }) => {
 await page.goto('/');
 await expect(page.locator('link[rel="icon"]')).toHaveAttribute('href', /\/assets\/favicon-.*\.svg$/);
 await expect(page.locator('link[rel="apple-touch-icon"]')).toHaveAttribute('href', /\/assets\/apple-touch-icon-.*\.png$/);
 await expect(page.getByRole('link', { name: 'Alondra, home' }).locator('svg.lark')).toBeVisible();
 await expect(page.locator('html')).toHaveAttribute('lang', 'en');
 for (const path of PUBLIC) {
  await page.goto(path);
  await expect(page.locator('link[rel=canonical]')).toHaveAttribute('href', `https://getalondra.com${path === '/' ? '/' : path}`);
 }
 await page.goto('/privacy');
 await expect(page.locator('link[rel=canonical]')).toHaveAttribute('href', 'https://getalondra.com/security');
});
