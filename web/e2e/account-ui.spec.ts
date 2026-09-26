// UI contract tests use explicit API doubles. Real Rust/Keycloak/PostgreSQL
// acceptance belongs to tests/system and is a separate release gate.
import { test, expect, type Page } from '@playwright/test';
const owner = { id: 'owner', display_name: 'David de prueba', status: 'active', is_admin: true };
const token = 'A'.repeat(43);
async function session(page: Page, admin = true) {
 await page.route('**/v1/session', route => route.fulfill({ json: { csrf_token: 'synthetic-csrf', account: { ...owner, is_admin: admin } } }));
}
test('ordinary account cannot render admin or request its lists', async ({ page }) => {
 await session(page, false);
 const requests: string[] = [];
 page.on('request', req => { if (req.url().includes('/v1/admin/')) requests.push(req.url()); });
 await page.goto('/admin');
 await expect(page.getByRole('heading', { name: 'Este espacio es de administración' })).toBeVisible();
 expect(requests).toEqual([]);
});
test('expired session has no private content', async ({ page }) => {
 await page.route('**/v1/session', route => route.fulfill({ status: 401, json: { error: { code: 'unauthorized' } } }));
 await page.goto('/account');
 await expect(page.getByRole('alert')).toContainText('La sesión ha terminado');
 await expect(page.getByRole('heading', { name: 'Tus Macs' })).toHaveCount(0);
});
test('Mac directory failure does not invent an empty account', async ({ page }) => {
 await session(page);
 await page.route('**/v1/macs', route => route.fulfill({ status: 503, json: { error: { code: 'mac_directory_unavailable' } } }));
 await page.goto('/account');
 await expect(page.getByRole('alert')).toContainText('servicio no está disponible');
 await expect(page.getByText('Tu primer Mac empieza aquí.')).toHaveCount(0);
});
test('invitation token is removed from URL, sent in body and never put in browser storage', async ({ page }) => {
 let accepted = false;
 await page.route('**/v1/invitations/status', route => {
  expect(route.request().postDataJSON()).toEqual({ token });
  return route.fulfill({ json: { status: 'pending' } });
 });
 await page.route('**/v1/invitations/accept', route => {
  const body = route.request().postDataJSON();
  expect(body.token).toBe(token); expect(body.display_name).toBe('Amiga de prueba');
  expect(body.password).toBe('synthetic password only'); accepted = true;
  return route.fulfill({ status: 201, json: { status: 'active', login_required: true, credential_already_created: true } });
 });
 await page.goto(`/invite#${token}`);
 await expect(page.getByLabel('Cómo te llamas')).toBeVisible();
 expect(new URL(page.url()).hash).toBe('');
 await page.getByLabel('Cómo te llamas').fill('Amiga de prueba');
 await page.getByLabel('Elige una contraseña').fill('synthetic password only');
 await page.getByRole('button', { name: 'Crear mi cuenta' }).click();
 await expect(page.getByRole('heading', { name: 'Tu cuenta está preparada.' })).toBeVisible();
 await expect(page.getByText(/El primer intento ya había creado/)).toBeVisible();
 expect(accepted).toBe(true);
 expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
 await expect(page.locator('input[type=password]')).toHaveCount(0);
});
test('unavailable invitation does not expose the password form', async ({ page }) => {
 await page.route('**/v1/invitations/status', route => route.fulfill({ json: { status: 'unavailable' } }));
 await page.goto(`/invite#${token}`);
 await expect(page.getByRole('heading', { name: 'Necesitas una invitación válida.' })).toBeVisible();
 await expect(page.locator('input[type=password]')).toHaveCount(0);
});
test('explicit retry after an ambiguous response reuses its idempotency key', async ({ page }) => {
 await session(page);
 await page.route('**/v1/admin/accounts', route => route.fulfill({ json: { accounts: [owner], next_cursor: null } }));
 const keys: string[] = [];
 await page.route('**/v1/admin/invitations', route => {
  if (route.request().method() === 'GET') return route.fulfill({ json: { invitations: [], next_cursor: null } });
  keys.push(route.request().headers()['idempotency-key']);
  if (keys.length === 1) return route.abort('connectionreset');
  return route.fulfill({ status: 409, json: { error: { code: 'invitation_token_already_issued' } } });
 });
 await page.goto('/admin');
 await page.getByLabel('Invitar por correo').fill('friend@example.test');
 await page.getByRole('button', { name: 'Crear invitación' }).click();
 await expect(page.getByRole('alert')).toContainText('No podemos confirmar');
 expect(keys.length).toBe(1);
 await page.getByRole('button', { name: 'Crear invitación' }).click();
 await expect(page.getByRole('status')).toContainText('ya no se puede recuperar');
 expect(keys.length).toBe(2); expect(keys[1]).toBe(keys[0]);
});
test('admin emits link once, confirms deletion and sends CSRF/idempotency', async ({ page }, info) => {
 await session(page);
 await page.route('**/v1/admin/accounts', route => route.fulfill({ json: { accounts: [owner, { id: 'friend', display_name: 'Amiga de prueba', status: 'active', is_admin: false }], next_cursor: null } }));
 let creates = 0;
 await page.route('**/v1/admin/invitations', route => {
  if (route.request().method() === 'GET') return route.fulfill({ json: { invitations: [], next_cursor: null } });
  creates++;
  expect(route.request().headers()['x-csrf-token']).toBe('synthetic-csrf');
  expect(route.request().headers()['idempotency-key']).toMatch(/^[0-9a-f-]{36}$/);
  return route.fulfill({ status: 201, json: { token, invitation: { id: 'invitation', email: 'friend@example.test', status: 'pending', expires_at_ms: 1791000000000, account_id: null } } });
 });
 let deletes = 0;
 await page.route('**/v1/admin/accounts/friend', route => {
  deletes++; expect(route.request().method()).toBe('DELETE');
  expect(route.request().headers()['x-csrf-token']).toBe('synthetic-csrf');
  return route.fulfill({ status: 202, json: { reconciliation: 'pending' } });
 });
 await page.goto('/admin');
 await page.getByLabel('Invitar por correo').fill('friend@example.test');
 await page.getByRole('button', { name: 'Crear invitación' }).click();
 await expect(page.getByRole('dialog')).toBeVisible();
 await expect(page.getByLabel('Enlace privado')).toHaveValue(`http://127.0.0.1:4173/invite#${token}`);
 await page.getByRole('button', { name: 'Ya lo he guardado' }).click();
 await expect(page.getByRole('dialog')).toHaveCount(0);
 expect(creates).toBe(1);
 await page.getByRole('button', { name: 'Eliminar', exact: true }).click();
 expect(deletes).toBe(0);
 await page.getByRole('button', { name: 'Volver', exact: true }).click();
 expect(deletes).toBe(0);
 await page.getByRole('button', { name: 'Eliminar', exact: true }).click();
 await page.getByRole('button', { name: 'Eliminar cuenta', exact: true }).click();
 await expect(page.getByRole('status')).toContainText('eliminación de la cuenta está en curso');
 expect(deletes).toBe(1);
 await page.setViewportSize({ width: 375, height: 900 });
 expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
 await page.screenshot({ path: info.outputPath('admin-mobile.png'), fullPage: true });
});
