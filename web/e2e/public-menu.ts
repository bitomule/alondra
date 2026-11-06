import { expect, type Page } from '@playwright/test';

export async function expectLegibleMenu(page: Page) {
 const menu = page.getByRole('navigation', { name: 'Main, compact' });
 const ratios = await menu.evaluate(nav => {
  const luminance = (css: string) => {
   const context = document.createElement('canvas').getContext('2d')!;
   context.fillStyle = css;
   context.fillRect(0, 0, 1, 1);
   const [r, g, b] = Array.from(context.getImageData(0, 0, 1, 1).data.slice(0, 3)).map(channel => {
    const value = channel / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
   });
   return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const background = getComputedStyle(nav).backgroundColor;
  return Array.from(nav.querySelectorAll('a')).map(link => {
   const [light, dark] = [luminance(getComputedStyle(link).color), luminance(background)].sort((a, b) => b - a);
   return { name: link.textContent, ratio: (light + 0.05) / (dark + 0.05) };
  });
 });
 expect(ratios.map(item => item.name)).toEqual(['How it works', 'Install', 'Documentation', 'Security and privacy', 'License', 'Sign in']);
 for (const { name, ratio } of ratios) expect(ratio, `${name} contrast`).toBeGreaterThanOrEqual(4.5);
 for (const link of await menu.getByRole('link').all()) await expect(link).toBeVisible();
}
