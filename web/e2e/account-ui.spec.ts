// UI contract tests use explicit API doubles. Real Rust/Keycloak/PostgreSQL
// acceptance is a separate release gate.
import { test, expect, type Page } from '@playwright/test';
const owner = { id: 'owner', display_name: 'Alice de prueba', status: 'active', is_admin: true };
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
 await page.route('**/v1/account/devices', route => route.fulfill({ status: 503, json: { error: { code: 'mac_directory_unavailable' } } }));
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
test('admin distinguishes same-name accounts before disabling or deleting the selected identity', async ({ page }, info) => {
 await session(page);
 const accounts = [
  { id: 'same-name-one', display_name: 'Alex de prueba', email: 'alex.one@example.test', status: 'active', is_admin: false },
  { id: 'same-name-two', display_name: 'Alex de prueba', email: 'alex.two@example.test', status: 'active', is_admin: false },
 ];
 await page.route('**/v1/admin/accounts', route => route.fulfill({ json: { accounts: [owner, ...accounts], next_cursor: null } }));
 await page.route('**/v1/admin/invitations', route => route.fulfill({ json: { invitations: [], next_cursor: null } }));
 const writes: string[] = [];
 await page.route('**/v1/admin/accounts/*', route => {
  writes.push(`${route.request().method()} ${new URL(route.request().url()).pathname}`);
  return route.fulfill({ status: 202, json: { reconciliation: 'pending' } });
 });
 await page.route('**/v1/admin/accounts/*/disable', route => {
  writes.push(`${route.request().method()} ${new URL(route.request().url()).pathname}`);
  return route.fulfill({ status: 202, json: { reconciliation: 'pending' } });
 });
 await page.goto('/admin');
 for (const account of accounts) {
  const row = page.getByRole('listitem').filter({ hasText: account.email });
  await expect(row).toHaveCount(1);
  await expect(row.getByRole('heading')).toHaveText(account.display_name);
 }
 await page.getByRole('listitem').filter({ hasText: accounts[0].email }).getByRole('button', { name: 'Desactivar', exact: true }).click();
 const dialog = page.getByRole('dialog');
 await expect(dialog).toContainText(`${accounts[0].display_name} — ${accounts[0].email}`);
 await expect(dialog).not.toContainText(accounts[1].email);
 expect(writes).toEqual([]);
 await dialog.getByRole('button', { name: 'Volver', exact: true }).click();
 expect(writes).toEqual([]);
 await page.getByRole('listitem').filter({ hasText: accounts[1].email }).getByRole('button', { name: 'Eliminar', exact: true }).click();
 await expect(dialog).toContainText(`${accounts[1].display_name} — ${accounts[1].email}`);
 await expect(dialog).not.toContainText(accounts[0].email);
 expect(writes).toEqual([]);
 await page.setViewportSize({ width: 375, height: 900 });
 await page.screenshot({ path: info.outputPath('admin-same-name-confirmation.png'), fullPage: true });
 await dialog.getByRole('button', { name: 'Eliminar cuenta', exact: true }).click();
 await expect(dialog).toHaveCount(0);
 expect(writes).toEqual(['DELETE /v1/admin/accounts/same-name-two']);
 await page.getByRole('listitem').filter({ hasText: accounts[0].email }).getByRole('button', { name: 'Desactivar', exact: true }).click();
 await dialog.getByRole('button', { name: 'Desactivar cuenta', exact: true }).click();
 await expect(dialog).toHaveCount(0);
 expect(writes).toEqual(['DELETE /v1/admin/accounts/same-name-two', 'POST /v1/admin/accounts/same-name-one/disable']);
});

for (const email of [null, undefined]) {
 test(`admin identifies an account with ${email === null ? 'null' : 'missing'} email by immutable ID`, async ({ page }, info) => {
  await session(page);
  const account = { id: 'legacy-6b36474f-05ba-47ab-9d48-e5a4e1829a3d', display_name: 'Alex de prueba', email, status: 'active', is_admin: false };
  await page.route('**/v1/admin/accounts', route => route.fulfill({ json: { accounts: [owner, account], next_cursor: null } }));
  await page.route('**/v1/admin/invitations', route => route.fulfill({ json: { invitations: [], next_cursor: null } }));
  let writes = 0;
  await page.route('**/v1/admin/accounts/*', route => { writes++; return route.fulfill({ status: 202, json: { reconciliation: 'pending' } }); });
  await page.goto('/admin');
  const row = page.getByRole('listitem').filter({ hasText: `ID: ${account.id}` });
  await expect(row).toHaveCount(1);
  await expect(row.getByRole('heading')).toHaveText(account.display_name);
  await row.getByRole('button', { name: 'Eliminar', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText(`${account.display_name} — ID: ${account.id}`);
  expect(writes).toBe(0);
  await page.setViewportSize({ width: 375, height: 900 });
  const description = dialog.locator('p');
  expect(await description.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath('admin-legacy-confirmation.png'), fullPage: true });
  await dialog.getByRole('button', { name: 'Volver', exact: true }).click();
  expect(writes).toBe(0);
 });
}

test('revoking a Mac requires confirmation and preserves the key after ambiguous failure', async ({ page }) => {
 await session(page, false);
 let revoked = false;
 await page.route('**/v1/account/devices', route => route.fulfill({ json: { macs: [{ mac_id: 'mac-synthetic', label: 'Mac de prueba', status: revoked ? 'revoked' : 'active', reconciliation: revoked ? 'pending' : null, clients: [] }] } }));
 const keys: string[] = [];
 await page.route('**/v1/account/devices/macs/mac-synthetic', route => {
  expect(route.request().method()).toBe('DELETE');
  expect(route.request().headers()['x-csrf-token']).toBe('synthetic-csrf');
  keys.push(route.request().headers()['idempotency-key']);
  revoked = true; // The server committed before the response was lost.
  if (keys.length === 1) return route.abort('connectionreset');
  return route.fulfill({ status: 409, json: { error: { code: 'action_already_applied' } } });
 });
 await page.goto('/account');
 await page.getByRole('button', { name: 'Retirar acceso al Mac' }).click();
 expect(keys).toHaveLength(0);
 await page.getByRole('button', { name: 'Confirmar retirada de acceso' }).click();
 await expect(page.getByRole('alert')).toContainText('No podemos confirmar');
 await page.getByRole('button', { name: 'Confirmar retirada de acceso' }).click();
 await expect(page.getByRole('dialog')).toHaveCount(0);
 expect(keys).toHaveLength(2); expect(keys[1]).toBe(keys[0]);
 await expect(page.getByRole('button', { name: 'Retirar acceso al Mac' })).toHaveCount(0);
 await expect(page.getByRole('status')).toContainText('ya se había aplicado');
});
test('self deletion leaves private UI and does not query a disabled account again', async ({ page }) => {
 await session(page, false);
 await page.route('**/v1/account/devices', route => route.fulfill({ json: { macs: [] } }));
 let deleted = false;
 await page.route('**/v1/account', route => {
  expect(route.request().method()).toBe('DELETE');
  expect(route.request().postDataJSON()).toEqual({ confirm: true });
  expect(route.request().headers()['x-csrf-token']).toBe('synthetic-csrf');
  deleted = true;
  return route.fulfill({ status: 202, json: { reconciliation: 'pending' } });
 });
 await page.goto('/account');
 await page.getByRole('button', { name: 'Dar de baja mi cuenta' }).click();
 expect(deleted).toBe(false);
 await page.getByRole('button', { name: 'Confirmar retirada de acceso' }).click();
 await expect(page).toHaveURL(/\/account-closed\/?$/);
 await expect(page.getByRole('heading', { name: 'Has solicitado la baja.' })).toBeVisible();
 await expect(page.getByRole('button', { name: 'Cerrar sesión' })).toHaveCount(0);
 await expect(page.getByText('Alice de prueba')).toHaveCount(0);
 expect(deleted).toBe(true);
});
test('ambiguous self deletion never presents a later auth failure as proof of deletion', async ({ page }) => {
 await session(page, false);
 await page.route('**/v1/account/devices', route => route.fulfill({ json: { macs: [] } }));
 let attempts = 0;
 await page.route('**/v1/account', route => {
  attempts++;
  return attempts === 1 ? route.abort('connectionreset') : route.fulfill({ status: 401, json: { error: { code: 'unauthorized' } } });
 });
 await page.goto('/account');
 await page.getByRole('button', { name: 'Dar de baja mi cuenta' }).click();
 await page.getByRole('button', { name: 'Confirmar retirada de acceso' }).click();
 await expect(page.getByRole('alert')).toContainText('No podemos confirmar');
 await page.getByRole('button', { name: 'Confirmar retirada de acceso' }).click();
 await expect(page.getByRole('alert')).toContainText('No podemos confirmar la baja');
 await expect(page.getByRole('heading', { name: 'Has solicitado la baja.' })).toHaveCount(0);
 expect(new URL(page.url()).pathname).not.toContain('account-closed');
});
test('ambiguous invitation can resume after reopening its private link', async ({ page }) => {
 let attempts = 0;
 await page.route('**/v1/invitations/status', route => route.fulfill({ json: { status: attempts ? 'provisioning' : 'pending' } }));
 await page.route('**/v1/invitations/accept', route => {
  expect(route.request().postDataJSON().token).toBe(token);
  attempts++;
  if (attempts === 1) return route.abort('connectionreset');
  return route.fulfill({ status: 201, json: { status: 'active', credential_already_created: true } });
 });
 await page.goto(`/invite#${token}`);
 await page.getByLabel('Cómo te llamas').fill('Amiga de prueba');
 await page.getByLabel('Elige una contraseña').fill('synthetic first password');
 await page.getByRole('button', { name: 'Crear mi cuenta' }).click();
 await expect(page.getByRole('alert')).toContainText('No podemos confirmar');
 await page.goto('/');
 await page.goto(`/invite#${token}`);
 await expect(page.getByRole('heading', { name: 'Estamos terminando el alta.' })).toBeVisible();
 expect(attempts).toBe(1);
 await page.getByRole('button', { name: 'Reanudar alta' }).click();
 await expect(page.getByRole('status')).toContainText('escribir otra aquí no la sustituye');
 await page.getByLabel('Cómo te llamas').fill('Amiga de prueba');
 await page.getByLabel('Elige una contraseña').fill('synthetic retry password');
 await page.getByRole('button', { name: 'Crear mi cuenta' }).click();
 await expect(page.getByRole('heading', { name: 'Tu cuenta está preparada.' })).toBeVisible();
 await expect(page.getByText(/la del último intento no la sustituye/)).toBeVisible();
 expect(attempts).toBe(2);
 expect(new URL(page.url()).hash).toBe('');
});
test('admin can cancel a provisioning invitation after explicit confirmation', async ({ page }) => {
 await session(page);
 await page.route('**/v1/admin/accounts', route => route.fulfill({ json: { accounts: [owner], next_cursor: null } }));
 let canceled = false;
 await page.route('**/v1/admin/invitations', route => route.fulfill({ json: { invitations: [{ id: 'pending-create', email: 'friend@example.test', status: canceled ? 'canceled' : 'provisioning', expires_at_ms: 1791000000000 }], next_cursor: null } }));
 await page.route('**/v1/admin/invitations/pending-create/cancel', route => {
  expect(route.request().method()).toBe('POST');
  expect(route.request().headers()['x-csrf-token']).toBe('synthetic-csrf');
  expect(route.request().headers()['idempotency-key']).toMatch(/^[0-9a-f-]{36}$/);
  canceled = true; return route.fulfill({ json: { status: 'canceled' } });
 });
 await page.goto('/admin');
 await expect(page.getByRole('button', { name: 'Nuevo enlace' })).toBeVisible();
 await page.getByRole('button', { name: 'Cancelar', exact: true }).click();
 expect(canceled).toBe(false);
 await page.getByRole('button', { name: 'Cancelar invitación', exact: true }).click();
 await expect(page.getByRole('status')).toContainText('Invitación cancelada');
 expect(canceled).toBe(true);
});
test('fragment replacement invalidates delayed status and never delivers the old invitation', async ({ page }) => {
 const replacement = 'B'.repeat(43);
 let releaseFirst!: () => void;
 const delayed = new Promise<void>(resolve => { releaseFirst = resolve; });
 let firstSeen!: () => void;
 const firstRequest = new Promise<void>(resolve => { firstSeen = resolve; });
 await page.route('**/v1/invitations/status', async route => {
  const value = route.request().postDataJSON().token;
  if (value === token) {
   firstSeen(); await delayed;
   await route.fulfill({ json: { status: 'unavailable' } }).catch(() => {});
  } else { expect(value).toBe(replacement); await route.fulfill({ json: { status: 'pending' } }); }
 });
 const delivered: string[] = [];
 await page.route('**/v1/invitations/accept', route => {
  delivered.push(route.request().postDataJSON().token);
  return route.fulfill({ json: { status: 'active' } });
 });
 await page.goto(`/invite#${token}`);
 await firstRequest;
 await page.evaluate(value => { window.location.hash = value; }, replacement);
 await expect(page.getByLabel('Cómo te llamas')).toBeVisible();
 releaseFirst();
 await expect(page.getByRole('heading', { name: 'Necesitas una invitación válida.' })).toHaveCount(0);
 expect(new URL(page.url()).hash).toBe('');
 await page.getByLabel('Cómo te llamas').fill('Second recipient');
 await page.getByLabel('Elige una contraseña').fill('synthetic replacement password');
 await page.getByRole('button', { name: 'Crear mi cuenta' }).click();
 await expect(page.getByRole('heading', { name: 'Tu cuenta está preparada.' })).toBeVisible();
 expect(delivered).toEqual([replacement]);
 expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
});
test('replacing a ready invitation clears its entered name and password', async ({ page }) => {
 await page.route('**/v1/invitations/status', route => route.fulfill({ json: { status: 'pending' } }));
 await page.goto(`/invite#${token}`);
 await page.getByLabel('Cómo te llamas').fill('Old recipient');
 await page.getByLabel('Elige una contraseña').fill('synthetic old password');
 await page.evaluate(value => { window.location.hash = value; }, 'B'.repeat(43));
 await expect(page.getByLabel('Cómo te llamas')).toHaveValue('');
 await expect(page.getByLabel('Elige una contraseña')).toHaveValue('');
 expect(new URL(page.url()).hash).toBe('');
});

test('device name retry preserves target, revision, normalized payload and key after a lost response', async ({ page }, info) => {
 await session(page, false);
 let label: string | null = null; let revision = 0;
 await page.route('**/v1/account/devices', route => route.fulfill({ json: { macs: [{ mac_id: 'mac-alias-fixture', label, label_revision: revision, status: 'active', reconciliation: null, clients: [] }] } }));
 const writes: { key: string; body: unknown }[] = [];
 await page.route('**/v1/account/devices/macs/mac-alias-fixture/label', route => {
  expect(route.request().method()).toBe('POST');
  expect(route.request().headers()['x-csrf-token']).toBe('synthetic-csrf');
  writes.push({ key: route.request().headers()['idempotency-key'], body: route.request().postDataJSON() });
  label = 'Portátil de José'; revision = 1;
  if (writes.length === 1) return route.abort('connectionreset');
  return route.fulfill({ status: 409, json: { error: { code: 'action_already_applied' } } });
 });
 await page.goto('/account');
 await page.getByRole('button', { name: 'Cambiar nombre del Mac' }).click();
 await page.getByLabel('Nombre del Mac', { exact: true }).fill('  Portátil de Jose\u0301  ');
 await page.getByRole('button', { name: 'Guardar nombre', exact: true }).click();
 await expect(page.getByRole('alert')).toContainText('No podemos confirmar');
 await expect(page.getByLabel('Nombre del Mac', { exact: true })).toBeDisabled();
 expect(writes).toHaveLength(1);
 expect(writes[0].body).toEqual({ label: 'Portátil de José', expected_revision: 0 });
 await page.getByRole('button', { name: 'Reintentar el mismo cambio' }).click();
 await expect(page.getByRole('dialog')).toHaveCount(0);
 await expect(page.getByRole('heading', { name: 'Portátil de José' })).toBeVisible();
 expect(writes).toHaveLength(2); expect(writes[1]).toEqual(writes[0]);
 await page.setViewportSize({ width: 375, height: 900 });
 await page.getByRole('button', { name: 'Cambiar nombre del Mac' }).click();
 await expect(page.getByText('Este nombre se guarda en tu cuenta', { exact: false })).toBeVisible();
 expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
 await page.screenshot({ path: info.outputPath('device-name-mobile.png'), fullPage: true });
});

test('device name conflict never overwrites a newer revision and revoked devices cannot be renamed', async ({ page }) => {
 await session(page, false);
 let label = 'Anterior'; let revision = 1; let reads = 0;
 await page.route('**/v1/account/devices', route => { reads++; return route.fulfill({ json: { macs: [
  { mac_id: 'mac-current', label, label_revision: revision, status: 'active', reconciliation: null, clients: [] },
  { mac_id: 'mac-revoked', label: 'Retirado', label_revision: 2, status: 'revoked', reconciliation: 'completed', clients: [] },
 ] } }); });
 let writes = 0;
 await page.route('**/v1/account/devices/macs/mac-current/label', route => {
  writes++; expect(route.request().postDataJSON()).toEqual({ label: 'Mi propuesta', expected_revision: 1 });
  label = 'Cambio desde otra sesión'; revision = 2;
  return route.fulfill({ status: 409, json: { error: { code: 'state_conflict' } } });
 });
 await page.goto('/account');
 await expect(page.getByRole('button', { name: 'Cambiar nombre del Mac' })).toHaveCount(1);
 await page.getByRole('button', { name: 'Cambiar nombre del Mac' }).click();
 await page.getByLabel('Nombre del Mac', { exact: true }).fill('Mi propuesta');
 // A background refetch must not change the revision of an already-open draft.
 const beforeRefresh = reads;
 label = 'Cambio desde otra sesión'; revision = 2;
 await page.evaluate(() => window.dispatchEvent(new Event('visibilitychange')));
 await expect.poll(() => reads).toBeGreaterThan(beforeRefresh);
 await expect(page.locator('h3').filter({ hasText: 'Cambio desde otra sesión' })).toHaveText('Cambio desde otra sesión');
 await expect(page.getByLabel('Nombre del Mac', { exact: true })).toHaveValue('Mi propuesta');
 await page.getByRole('button', { name: 'Guardar nombre', exact: true }).click();
 await expect(page.getByRole('alert')).toContainText('otra sesión');
 await expect(page.getByRole('button', { name: 'Reintentar el mismo cambio' })).toHaveCount(0);
 await page.getByRole('button', { name: 'Actualizar lista', exact: true }).click();
 await expect(page.getByRole('heading', { name: 'Cambio desde otra sesión' })).toBeVisible();
 expect(writes).toBe(1);
});

test('client name uses its own link, rejects controls and can clear an alias', async ({ page }) => {
 await session(page, false);
 let label: string | null = 'Teléfono';
 await page.route('**/v1/account/devices', route => route.fulfill({ json: { macs: [{ mac_id: 'mac-parent', label: 'Mac', label_revision: 1, status: 'active', reconciliation: null,
  clients: [{ device_id: 'client-link-one', label, label_revision: 3, status: 'active', reconciliation: null }] }] } }));
 let writes = 0;
 await page.route('**/v1/account/devices/clients/client-link-one/label', route => {
  writes++; expect(route.request().postDataJSON()).toEqual({ label: null, expected_revision: 3 }); label = null;
  return route.fulfill({ json: { label: null, label_revision: 4 } });
 });
 await page.goto('/account');
 await page.getByText('Identidad y dispositivos vinculados (1)').click();
 await page.getByRole('button', { name: 'Cambiar nombre del cliente' }).click();
 await page.getByLabel('Nombre del cliente', { exact: true }).fill('Nombre\u202E');
 await expect(page.getByRole('button', { name: 'Guardar nombre', exact: true })).toBeDisabled();
 expect(writes).toBe(0);
 await page.getByLabel('Nombre del cliente', { exact: true }).fill('');
 await page.getByRole('button', { name: 'Guardar nombre', exact: true }).click();
 await expect(page.getByRole('dialog')).toHaveCount(0);
 await expect(page.getByText('Cliente vinculado', { exact: true })).toBeVisible();
 expect(writes).toBe(1);
});
const testflight = 'https://testflight.apple.com/join/synthetic';
async function acceptInvitation(page: Page, response: Record<string, unknown>) {
 await page.route('**/v1/invitations/status', route => route.fulfill({ json: { status: 'pending' } }));
 await page.route('**/v1/invitations/accept', route => route.fulfill({ status: 201, json: response }));
 await page.goto(`/invite#${token}`);
 await page.getByLabel('Cómo te llamas').fill('Amiga de prueba');
 await page.getByLabel('Elige una contraseña').fill('synthetic password only');
 await page.getByRole('button', { name: 'Crear mi cuenta' }).click();
 await expect(page.getByRole('heading', { name: 'Tu cuenta está preparada.' })).toBeVisible();
}
test('finishing the invitation offers the TestFlight link the relay sent', async ({ page }) => {
 await acceptInvitation(page, { status: 'active', login_required: true, credential_already_created: false, testflight_url: testflight });
 await expect(page.getByRole('link', { name: 'Instala la app desde TestFlight' })).toHaveAttribute('href', testflight);
 await expect(page.getByRole('link', { name: 'Entrar en Alondra' })).toBeVisible();
});
test('finishing the invitation shows no TestFlight link when none is configured', async ({ page }) => {
 await acceptInvitation(page, { status: 'active', login_required: true, credential_already_created: false });
 await expect(page.getByRole('link', { name: 'Entrar en Alondra' })).toBeVisible();
 await expect(page.getByRole('link', { name: 'Instala la app desde TestFlight' })).toHaveCount(0);
});
test('install shows the TestFlight link only to a session that carries one', async ({ page }) => {
 const link = page.getByRole('link', { name: 'Get the app on TestFlight' });
 await page.route('**/v1/session', route => route.fulfill({ status: 401, json: { error: { code: 'unauthorized' } } }));
 await page.goto('/install');
 await expect(page.locator('main').getByRole('link', { name: 'join the waitlist' })).toBeVisible();
 await expect(link).toHaveCount(0);
 await page.unroute('**/v1/session');
 await session(page, false);
 await page.reload();
 await expect(link).toHaveCount(0);
 await page.unroute('**/v1/session');
 await page.route('**/v1/session', route => route.fulfill({ json: { csrf_token: 'synthetic-csrf', account: { ...owner, is_admin: false }, testflight_url: testflight } }));
 await page.reload();
 await expect(link).toHaveAttribute('href', testflight);
});
test('account shows the TestFlight link from the session', async ({ page }) => {
 await page.route('**/v1/session', route => route.fulfill({ json: { csrf_token: 'synthetic-csrf', account: { ...owner, is_admin: false }, testflight_url: testflight } }));
 await page.route('**/v1/account/devices', route => route.fulfill({ json: { macs: [] } }));
 await page.goto('/account');
 await expect(page.getByRole('link', { name: 'Instala la app desde TestFlight' })).toHaveAttribute('href', testflight);
});
