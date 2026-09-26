import { useQuery, useQueryClient } from '@tanstack/react-query';
import * as Dialog from '@radix-ui/react-dialog';
import { useRef, useState } from 'react';
import { Link } from 'react-router';
import { PrivatePage, Feedback } from '../components/private';
import { api, type Session } from '../lib/api';
export function meta() { return [{ title: 'Mi cuenta · Alondra' }, { name: 'robots', content: 'noindex, nofollow' }]; }
export default function AccountPage() { return <PrivatePage>{session => <AccountContent key={session.account.id} session={session}/>}</PrivatePage>; }
type Client = { device_id: string; label: string | null; status: 'active' | 'revoked'; reconciliation: string | null };
type Mac = { mac_id: string; label: string | null; status: 'active' | 'revoked'; reconciliation: string | null; clients: Client[] };
type Action = { kind: 'mac' | 'client' | 'account'; id: string; label: string; key: string };
function short(id: string) { return id.length > 24 ? `${id.slice(0, 12)}…${id.slice(-8)}` : id; }
function AccountContent({ session }: { session: Session }) {
  const client = useQueryClient();
  const devices = useQuery({ queryKey: ['devices', session.account.id], queryFn: ({ signal }) => api<{ macs: Mac[] }>('/v1/account/devices', { signal }), staleTime: 0 });
  const [action, setAction] = useState<Action | null>(null);
  const [pending, setPending] = useState(false);
  const busy = useRef(false);
  const [error, setError] = useState<unknown>(null);
  const [notice, setNotice] = useState('');
  function prepare(kind: Action['kind'], id: string, label: string) { setError(null); setAction({ kind, id, label, key: crypto.randomUUID() }); }
  async function confirm() {
    if (!action || busy.current) return;
    const selected = action;
    busy.current = true; setPending(true); setError(null); setNotice('');
    try {
      const path = selected.kind === 'account' ? '/v1/account' : `/v1/account/devices/${selected.kind === 'mac' ? 'macs' : 'clients'}/${encodeURIComponent(selected.id)}`;
      await api(path, { method: 'DELETE', body: selected.kind === 'account' ? { confirm: true } : undefined, csrf: session.csrf_token, key: selected.key });
      if (selected.kind === 'account') {
        await client.cancelQueries(); client.clear(); window.location.replace('/account-closed');
      } else {
        await devices.refetch(); setNotice('Acceso retirado. La limpieza de su registro está en curso.');
      }
      setAction(null);
    } catch (e) { setError(e); /* Keep this exact action/key available for an explicit retry. */ }
    finally { busy.current = false; setPending(false); }
  }
  return <><p className="eyebrow">Tu cuenta</p><h1>Hola, {session.account.display_name}.</h1><p className="lede">Aquí gestionas tu acceso. Tus tareas y conversaciones siguen en tus dispositivos.</p>{notice && <p className="feedback" role="status">{notice}</p>}<div className="account-grid"><section aria-labelledby="macs-heading"><div className="section-heading"><h2 id="macs-heading">Tus Macs</h2><button className="text-button" disabled={devices.isFetching || pending} onClick={() => void devices.refetch()}>{devices.isFetching ? 'Actualizando…' : 'Actualizar'}</button></div>{devices.isPending ? <p role="status">Buscando tus Macs…</p> : devices.isError ? <Feedback error={devices.error}/> : devices.data.macs.length === 0 ? <div className="empty-state"><h3>Tu primer Mac empieza aquí.</h3><p>Instala Alondra y ejecuta <code>alondra setup</code>. Entra con esta cuenta para registrar el equipo.</p><Link className="button" to="/install">Preparar mi Mac</Link></div> : <ul className="record-list mac-list">{devices.data.macs.map(mac => <li key={mac.mac_id}><div className="record-content"><h3>{mac.label || `Mac · ${short(mac.mac_id)}`}</h3><p className="small muted">{mac.status === 'revoked' ? 'Acceso retirado' : 'Acceso autorizado'}{mac.reconciliation === 'pending' ? ' · Limpieza en curso' : ''}</p><details><summary>Identidad y dispositivos vinculados ({mac.clients.length})</summary><p className="small"><code>{mac.mac_id}</code></p>{mac.clients.length ? <ul className="client-list">{mac.clients.map(device => <li key={device.device_id}><div><strong>{device.label || 'Cliente vinculado'}</strong><p className="small"><code>{device.device_id}</code></p><p className="small muted">{device.status === 'revoked' ? 'Acceso retirado' : 'Acceso autorizado'}{device.reconciliation === 'pending' ? ' · Limpieza en curso' : ''}</p></div>{device.status === 'active' && mac.status === 'active' && <button className="text-button" disabled={pending} onClick={() => prepare('client', device.device_id, device.label || device.device_id)}>Retirar acceso al cliente</button>}</li>)}</ul> : <p className="small muted">Aún no hay clientes vinculados.</p>}</details></div>{mac.status === 'active' && <button className="text-button" disabled={pending} onClick={() => prepare('mac', mac.mac_id, mac.label || mac.mac_id)}>Retirar acceso al Mac</button>}</li>)}</ul>}</section><aside className="aside-note"><h2>Una cuenta. Tus vínculos.</h2><p>Puedes añadir más Macs y usar varios clientes. Cada cliente necesita la aprobación del Mac al que quiere acceder.</p><p>“Autorizado” indica un permiso, no que el Mac esté encendido. Comprueba su disponibilidad desde la app.</p><p>El login y la administración no dan acceso al contenido cifrado.</p><Link to="/privacy">Cómo funciona la privacidad</Link></aside></div><section className="account-closure"><h2>Dar de baja mi cuenta</h2>{session.account.is_admin ? <p className="muted">La cuenta que administra la alpha no se puede eliminar desde aquí.</p> : <><p>Se bloqueará el acceso al servicio para tus dispositivos. Conservaremos los registros mínimos necesarios para auditar la baja e impedir reactivaciones.</p><button className="text-button danger-text" disabled={pending} onClick={() => prepare('account', session.account.id, session.account.display_name)}>Dar de baja mi cuenta</button></>}</section>
  <Dialog.Root open={action !== null} onOpenChange={open => { if (!open && !pending) setAction(null); }}><Dialog.Portal><Dialog.Overlay className="dialog-overlay"/><Dialog.Content className="dialog-content" onEscapeKeyDown={event => { if (pending) event.preventDefault(); }} onPointerDownOutside={event => { if (pending) event.preventDefault(); }}><Dialog.Title>{action?.kind === 'account' ? 'Dar de baja tu cuenta' : 'Retirar acceso'}</Dialog.Title><Dialog.Description>{action?.label}. {action?.kind === 'mac' ? 'Se retirará también el acceso de sus clientes vinculados.' : action?.kind === 'client' ? 'Este vínculo dejará de tener acceso a través del servicio.' : 'Todos tus dispositivos perderán acceso al servicio.'} Esto no cancela trabajo que ya haya llegado al Mac ni borra contenido de los dispositivos.</Dialog.Description>{error != null && <Feedback error={error}/>}<div className="actions"><button className="button" disabled={pending} onClick={() => void confirm()}>{pending ? 'Aplicando…' : 'Confirmar retirada de acceso'}</button><Dialog.Close asChild><button className="text-button" disabled={pending}>Volver</button></Dialog.Close></div></Dialog.Content></Dialog.Portal></Dialog.Root></>;
}
