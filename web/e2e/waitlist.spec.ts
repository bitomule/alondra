import { test, expect, type Page, type Route } from '@playwright/test';

const hero = (page: Page) => page.locator('#alpha');
const access = (page: Page) => page.locator('#alpha');

async function mockWaitlist(page: Page, handler: (route: Route, body: { email: string }) => Promise<void> | void) {
  const bodies: { email: string }[] = [];
  await page.route('**/api/waitlist', async route => {
    expect(route.request().method()).toBe('POST');
    expect(route.request().headers()['content-type']).toContain('application/json');
    const body = route.request().postDataJSON();
    bodies.push(body);
    await handler(route, body);
  });
  return bodies;
}

async function submit(page: Page, scope: ReturnType<typeof hero>, email: string) {
  await scope.getByLabel('Tu email').fill(email);
  await scope.getByRole('button', { name: 'Apuntarme a la alpha' }).click();
}

test('a new email is accepted, confirmed without promising an email, and focuses the confirmation', async ({ page }) => {
  const bodies = await mockWaitlist(page, route => route.fulfill({ status: 202, json: { status: 'accepted' } }));
  await page.goto('/');
  await submit(page, hero(page), '  ada@example.com ');
  const done = hero(page).getByRole('status');
  await expect(done).toContainText('Estás en la lista.');
  await expect(done).toContainText('ada@example.com');
  await expect(done).toContainText('No te enviaremos un correo de confirmación');
  await expect(done).toBeFocused();
  expect(bodies).toEqual([{ email: 'ada@example.com' }]);
  await expect(access(page)).toContainText('Estás en la lista.');
});

test('an email already on the list gets exactly the same answer as a new one', async ({ page }) => {
  const seen = new Set<string>();
  await mockWaitlist(page, (route, body) => { seen.add(body.email); return route.fulfill({ status: 202, json: { status: 'accepted' } }); });
  await page.goto('/');
  await submit(page, hero(page), 'ada@example.com');
  await expect(hero(page).getByRole('status')).toContainText('Estás en la lista.');
  const first = await hero(page).locator('.waitlist').innerText();
  await hero(page).getByRole('button', { name: 'Apuntar otro email' }).click();
  await expect(hero(page).getByLabel('Tu email')).toHaveValue('');
  await submit(page, hero(page), 'ada@example.com');
  await expect(hero(page).getByRole('status')).toContainText('Estás en la lista.');
  expect(await hero(page).locator('.waitlist').innerText()).toBe(first);
  expect(seen.size).toBe(1);
});

test('the server rejecting the address keeps it, marks the field and explains', async ({ page }) => {
  await mockWaitlist(page, route => route.fulfill({ status: 400, json: { error: 'invalid_email' } }));
  await page.goto('/');
  await submit(page, access(page), 'ada@example.invalid');
  const input = access(page).getByLabel('Tu email');
  await expect(access(page).getByRole('alert')).toHaveText('Revisa el email: no parece una dirección válida.');
  await expect(input).toHaveValue('ada@example.invalid');
  await expect(input).toHaveAttribute('aria-invalid', 'true');
  await expect(input).toBeFocused();
});

test('an obviously wrong address never reaches the API', async ({ page }) => {
  const bodies = await mockWaitlist(page, route => route.fulfill({ status: 202, json: { status: 'accepted' } }));
  await page.goto('/');
  await submit(page, hero(page), 'ada@');
  await expect(hero(page).getByRole('alert')).toHaveText('Revisa el email: no parece una dirección válida.');
  await expect(hero(page).getByLabel('Tu email')).toHaveValue('ada@');
  expect(bodies).toHaveLength(0);
});

test('rate limiting says when to try again and keeps the email', async ({ page }) => {
  let retryAfter: string | undefined = '120';
  await mockWaitlist(page, route => route.fulfill({ status: 429, headers: retryAfter ? { 'Retry-After': retryAfter } : {}, json: { error: 'rate_limited' } }));
  await page.goto('/');
  await submit(page, hero(page), 'ada@example.com');
  await expect(hero(page).getByRole('alert')).toHaveText('Demasiados intentos seguidos. Vuelve a probar en 2 minutos.');
  await expect(hero(page).getByLabel('Tu email')).toHaveValue('ada@example.com');
  retryAfter = '1';
  await hero(page).getByRole('button', { name: 'Apuntarme a la alpha' }).click();
  await expect(hero(page).getByRole('alert')).toHaveText('Demasiados intentos seguidos. Vuelve a probar en un segundo.');
  retryAfter = undefined;
  await hero(page).getByRole('button', { name: 'Apuntarme a la alpha' }).click();
  await expect(hero(page).getByRole('alert')).toHaveText('Demasiados intentos seguidos. Vuelve a probar dentro de un rato.');
});

test('a lost connection or a server error keeps the email and a retry can succeed', async ({ page }) => {
  let attempt = 0;
  await mockWaitlist(page, route => {
    attempt++;
    if (attempt === 1) return route.abort('connectionreset');
    if (attempt === 2) return route.fulfill({ status: 503, body: 'unavailable' });
    return route.fulfill({ status: 202, json: { status: 'accepted' } });
  });
  await page.goto('/');
  await submit(page, hero(page), 'ada@example.com');
  const failed = 'No hemos podido apuntarte. Tu email sigue aquí: vuelve a intentarlo en un momento.';
  await expect(hero(page).getByRole('alert')).toHaveText(failed);
  await expect(hero(page).getByLabel('Tu email')).toHaveValue('ada@example.com');
  await hero(page).getByRole('button', { name: 'Apuntarme a la alpha' }).click();
  await expect(hero(page).getByRole('alert')).toHaveText(failed);
  await hero(page).getByLabel('Tu email').press('Enter');
  await expect(hero(page).getByRole('status')).toContainText('Estás en la lista.');
  expect(attempt).toBe(3);
});

test('while sending, the form says so and cannot be sent twice', async ({ page }) => {
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  const bodies = await mockWaitlist(page, async route => { await held; await route.fulfill({ status: 202, json: { status: 'accepted' } }); });
  await page.goto('/');
  await submit(page, hero(page), 'ada@example.com');
  const button = hero(page).getByRole('button', { name: 'Enviando…' });
  await expect(button).toBeDisabled();
  await expect(hero(page).locator('form')).toHaveAttribute('aria-busy', 'true');
  await expect(hero(page).getByRole('status')).toHaveText('Enviando tu email…');
  await expect(hero(page).getByLabel('Tu email')).toHaveAttribute('readonly', '');
  await hero(page).getByLabel('Tu email').press('Enter');
  release();
  await expect(hero(page).getByRole('status')).toContainText('Estás en la lista.');
  expect(bodies).toHaveLength(1);
});

test('the form works from the keyboard alone', async ({ page }) => {
  await mockWaitlist(page, route => route.fulfill({ status: 202, json: { status: 'accepted' } }));
  await page.goto('/');
  const input = hero(page).getByLabel('Tu email');
  await input.focus();
  await page.keyboard.type('grace@example.com');
  await page.keyboard.press('Tab');
  await expect(hero(page).getByRole('button', { name: 'Apuntarme a la alpha' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(hero(page).getByRole('status')).toContainText('grace@example.com');
});
