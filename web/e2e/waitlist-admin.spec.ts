// UI contract for the admin waitlist, with explicit API doubles. The Rust
// handlers, PostgreSQL and authorization are covered by the relay tests.
import { test, expect, type Page } from '@playwright/test';
const owner = { id: 'owner', display_name: 'Alice de prueba', status: 'active', is_admin: true };
async function admin(page: Page) {
 await page.route('**/v1/session', route => route.fulfill({ json: { csrf_token: 'synthetic-csrf', account: owner } }));
 await page.route('**/v1/admin/accounts', route => route.fulfill({ json: { accounts: [owner], next_cursor: null } }));
 await page.route('**/v1/admin/invitations', route => route.fulfill({ json: { invitations: [], next_cursor: null } }));
}

test('admin lists waitlist requests and removes one only after confirmation', async ({ page }) => {
 await admin(page);
 let entries = [
  { id: 'wait-1', email: 'early@example.test', created_at_ms: 1790500000000 },
  { id: 'wait-2', email: 'later@example.test', created_at_ms: 1790600000000 },
 ];
 await page.route('**/v1/admin/waitlist', route => route.fulfill({ json: { entries, next_cursor: null } }));
 let deletes = 0;
 await page.route('**/v1/admin/waitlist/wait-1', route => {
  deletes++;
  expect(route.request().method()).toBe('DELETE');
  expect(route.request().headers()['x-csrf-token']).toBe('synthetic-csrf');
  entries = entries.filter(entry => entry.id !== 'wait-1');
  return route.fulfill({ status: 204, body: '' });
 });
 await page.goto('/admin');
 const waitlist = page.getByRole('region', { name: 'Lista de espera' });
 await expect(waitlist.getByRole('heading', { name: 'early@example.test' })).toBeVisible();
 await expect(waitlist.getByRole('heading', { name: 'later@example.test' })).toBeVisible();
 await waitlist.getByRole('button', { name: 'Retirar' }).first().click();
 await expect(page.getByRole('dialog')).toContainText('early@example.test');
 await page.getByRole('button', { name: 'Volver', exact: true }).click();
 expect(deletes).toBe(0);
 await waitlist.getByRole('button', { name: 'Retirar' }).first().click();
 await page.getByRole('button', { name: 'Retirar solicitud' }).click();
 await expect(waitlist.getByRole('status')).toContainText('Solicitud retirada');
 await expect(waitlist.getByRole('heading', { name: 'early@example.test' })).toHaveCount(0);
 expect(deletes).toBe(1);
});

test('admin invites someone from the waitlist and gets the link to send by hand', async ({ page }) => {
 await admin(page);
 let entries = [
  { id: 'wait-1', email: 'friend@example.test', created_at_ms: 1790500000000 },
  { id: 'wait-2', email: 'member@example.test', created_at_ms: 1790600000000 },
 ];
 const invitations: { id: string; email: string; status: string; expires_at_ms: number; account_id: null }[] = [];
 await page.route('**/v1/admin/invitations', route => route.fulfill({ json: { invitations, next_cursor: null } }));
 await page.route('**/v1/admin/waitlist', route => route.fulfill({ json: { entries, next_cursor: null } }));
 const keys: string[] = [];
 await page.route('**/v1/admin/waitlist/wait-1/invite', route => {
  const request = route.request();
  expect(request.method()).toBe('POST');
  expect(request.headers()['x-csrf-token']).toBe('synthetic-csrf');
  expect(request.postDataJSON()).toEqual({ ttl_seconds: 604800 });
  keys.push(request.headers()['idempotency-key']);
  const invitation = { id: 'invite-1', email: 'friend@example.test', status: 'pending', expires_at_ms: 1791100000000, account_id: null };
  invitations.push(invitation);
  entries = entries.filter(entry => entry.id !== 'wait-1');
  return route.fulfill({ status: 201, json: { invitation, token: 'synthetic-token' } });
 });
 await page.route('**/v1/admin/waitlist/wait-2/invite', route => route.fulfill({ status: 409, json: { error: { code: 'state_conflict', retryable: false } } }));
 await page.goto('/admin');
 const waitlist = page.getByRole('region', { name: 'Lista de espera' });
 await waitlist.getByRole('button', { name: 'Invitar' }).first().click();
 await expect(page.getByRole('dialog')).toContainText('friend@example.test');
 await page.getByRole('button', { name: 'Volver', exact: true }).click();
 expect(keys).toEqual([]);
 await waitlist.getByRole('button', { name: 'Invitar' }).first().click();
 await page.getByRole('dialog').getByRole('button', { name: 'Invitar' }).click();
 const issued = page.getByRole('dialog', { name: 'Invitación preparada' });
 await expect(issued.getByLabel('Enlace privado')).toHaveValue(/\/invite#synthetic-token$/);
 await expect(issued).toContainText('friend@example.test');
 expect(keys).toHaveLength(1);
 expect(keys[0]).toMatch(/^[0-9a-f-]{36}$/);
 await issued.getByRole('button', { name: 'Ya lo he guardado' }).click();
 await expect(waitlist.getByRole('heading', { name: 'friend@example.test' })).toHaveCount(0);
 await expect(page.getByRole('region', { name: 'Invitaciones' }).getByRole('heading', { name: 'friend@example.test' })).toBeVisible();

 await waitlist.getByRole('button', { name: 'Invitar' }).click();
 await page.getByRole('dialog').getByRole('button', { name: 'Invitar' }).click();
 await expect(waitlist.getByRole('status')).toContainText('Este correo ya tiene invitación o cuenta. Puedes retirar la solicitud.');
 await expect(waitlist.getByRole('heading', { name: 'member@example.test' })).toBeVisible();
});

test('ordinary account never requests the waitlist', async ({ page }) => {
 await page.route('**/v1/session', route => route.fulfill({ json: { csrf_token: 'synthetic-csrf', account: { ...owner, is_admin: false } } }));
 const requests: string[] = [];
 page.on('request', req => { if (req.url().includes('/v1/admin/waitlist')) requests.push(req.url()); });
 await page.goto('/admin');
 await expect(page.getByRole('heading', { name: 'Este espacio es de administración' })).toBeVisible();
 expect(requests).toEqual([]);
});
