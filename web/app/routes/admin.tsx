import { useInfiniteQuery, useQueryClient } from '@tanstack/react-query';
import * as Dialog from '@radix-ui/react-dialog';
import { useRef, useState, type FormEvent } from 'react';
import { PrivatePage, Feedback } from '../components/private';
import { api, ApiError, type Account, type Invitation, type IssuedInvitation, type Session } from '../lib/api';
export function meta() { return [{ title: 'Administración · Alondra' }, { name: 'robots', content: 'noindex, nofollow' }]; }
export default function AdminPage() { return <PrivatePage admin>{session => <AdminContent key={session.account.id} session={session}/>}</PrivatePage>; }
type Action = { kind: 'cancel' | 'reissue'; item: Invitation } | { kind: 'disable' | 'delete'; item: Account };
const labels: Record<string, string> = { pending: 'Pendiente', provisioning: 'Creando cuenta', accepted: 'Utilizada', expired: 'Caducada', cancelled: 'Cancelada', active: 'Activa', disabled: 'Desactivada', deleting: 'Eliminando', deleted: 'Eliminada' };
function status(value: string) { return labels[value] ?? value; }
function AdminContent({ session }: { session: Session }) {
  const client = useQueryClient();
  const [action, setAction] = useState<Action | null>(null);
  const [issued, setIssued] = useState<IssuedInvitation | null>(null);
  const [notice, setNotice] = useState('');
  const [error, setError] = useState<unknown>(null);
  const [pending, setPending] = useState(false);
  const busy = useRef(false);
  const unresolved = useRef(new Map<string, string>());
  const [copied, setCopied] = useState(false);
  const invitations = useInfiniteQuery({ queryKey: ['admin-invitations', session.account.id], initialPageParam: '', queryFn: ({ pageParam, signal }) => api<{ invitations: Invitation[]; next_cursor: string | null }>(`/v1/admin/invitations${pageParam ? `?after=${encodeURIComponent(pageParam)}` : ''}`, { signal }), getNextPageParam: page => page.next_cursor || undefined });
  const accounts = useInfiniteQuery({ queryKey: ['admin-accounts', session.account.id], initialPageParam: '', queryFn: ({ pageParam, signal }) => api<{ accounts: Account[]; next_cursor: string | null }>(`/v1/admin/accounts${pageParam ? `?after=${encodeURIComponent(pageParam)}` : ''}`, { signal }), getNextPageParam: page => page.next_cursor || undefined });
  const invitationItems = invitations.data?.pages.flatMap(page => page.invitations) ?? [];
  const accountItems = accounts.data?.pages.flatMap(page => page.accounts) ?? [];
  const invitationURL = issued ? `${window.location.origin}/invite#${issued.token}` : '';
  async function refresh() {
    await Promise.all([client.invalidateQueries({ queryKey: ['admin-invitations', session.account.id] }), client.invalidateQueries({ queryKey: ['admin-accounts', session.account.id] })]);
  }
  async function mutate(path: string, body: unknown, method = 'POST'): Promise<unknown> {
    if (busy.current) return;
    busy.current = true; setPending(true); setError(null); setNotice('');
    const operation = JSON.stringify([method, path, body]);
    const key = unresolved.current.get(operation) ?? crypto.randomUUID();
    unresolved.current.set(operation, key);
    try {
      const result = await api<unknown>(path, { method, body, csrf: session.csrf_token, key });
      unresolved.current.delete(operation);
      await refresh(); return result;
    } catch (e) {
      // An interrupted/5xx response does not prove the transaction failed. Keep
      // its key for an explicit retry with the same payload during this session.
      if (e instanceof ApiError && e.status >= 400 && e.status < 500) unresolved.current.delete(operation);
      if (e instanceof ApiError && ['invitation_token_already_issued', 'action_already_applied'].includes(e.code)) {
        setNotice(e.code === 'invitation_token_already_issued' ? 'La invitación se creó, pero este enlace ya no se puede recuperar. Emite uno nuevo desde la lista.' : 'La operación ya se había aplicado. Hemos actualizado la lista.');
      } else setError(e);
      await refresh(); throw e;
    } finally { busy.current = false; setPending(false); }
  }
  async function invite(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); if (busy.current) return;
    const form = event.currentTarget;
    try {
      const result = await mutate('/v1/admin/invitations', { email: String(new FormData(form).get('email') || '').trim(), ttl_seconds: 604800 }) as IssuedInvitation;
      if (result) { setIssued(result); setCopied(false); form.reset(); }
    } catch { /* Mutation feedback is rendered once above the lists. */ }
  }
  async function confirm() {
    if (!action || busy.current) return;
    const selected = action;
    try {
      if (selected.kind === 'cancel' || selected.kind === 'reissue') {
        const result = await mutate(`/v1/admin/invitations/${encodeURIComponent(selected.item.id)}/${selected.kind}`, selected.kind === 'reissue' ? { ttl_seconds: 604800 } : {}) as IssuedInvitation;
        if (selected.kind === 'reissue' && result) { setIssued(result); setCopied(false); }
        else setNotice('Invitación cancelada. El enlace ya no permite crear una cuenta.');
      } else {
        await mutate(`/v1/admin/accounts/${encodeURIComponent(selected.item.id)}${selected.kind === 'disable' ? '/disable' : ''}`, selected.kind === 'delete' ? undefined : {}, selected.kind === 'delete' ? 'DELETE' : 'POST');
        setNotice(selected.kind === 'delete' ? 'Acceso bloqueado. La eliminación de la cuenta está en curso.' : 'Acceso desactivado. El cierre de sus sesiones está en curso.');
      }
    } catch { /* Keep errors outside the modal after it closes. */ }
    finally { setAction(null); }
  }
  const actionName = action?.kind === 'cancel' ? 'Cancelar invitación' : action?.kind === 'reissue' ? 'Emitir enlace nuevo' : action?.kind === 'disable' ? 'Desactivar cuenta' : 'Eliminar cuenta';
  return <><p className="eyebrow">Administración</p><h1>Personas y acceso.</h1><p className="lede">Invita a la alpha y gestiona el acceso. Estas herramientas no permiten leer tareas ni conversaciones.</p>
    <form className="invite-form" onSubmit={invite}><div><label htmlFor="invite-email">Invitar por correo</label><input id="invite-email" name="email" type="email" autoComplete="off" maxLength={254} required placeholder="nombre@ejemplo.com" disabled={pending}/><p className="small muted">El enlace dura 7 días. Se lo enviarás tú; Alondra aún no envía correos.</p></div><button className="button" disabled={pending}>{pending ? 'Aplicando…' : 'Crear invitación'}</button></form>
    {notice && <p className="feedback" role="status">{notice}</p>}{error != null && <Feedback error={error}/>}
    <section aria-labelledby="invitations-heading"><div className="section-heading"><h2 id="invitations-heading">Invitaciones</h2><button className="text-button" onClick={() => void refresh()} disabled={pending || invitations.isFetching || accounts.isFetching}>Actualizar listas</button></div>{invitations.isPending ? <p role="status">Cargando invitaciones…</p> : invitations.isError ? <Feedback error={invitations.error}/> : invitationItems.length === 0 ? <p className="empty-state">Aún no has invitado a nadie. Empieza con una o dos personas.</p> : <ul className="record-list">{invitationItems.map(item => <li key={item.id}><div className="record-content"><h3>{item.email}</h3><p><span className="status-label">{status(item.status)}</span> <span className="small muted">Caduca {new Date(item.expires_at_ms).toLocaleString('es-ES')}</span></p></div><div className="record-actions">{['pending', 'expired', 'cancelled'].includes(item.status) && <button onClick={() => setAction({ kind: 'reissue', item })} disabled={pending}>Nuevo enlace</button>}{item.status === 'pending' && <button onClick={() => setAction({ kind: 'cancel', item })} disabled={pending}>Cancelar</button>}</div></li>)}</ul>}{invitations.hasNextPage && <button className="text-button" disabled={invitations.isFetchingNextPage} onClick={() => void invitations.fetchNextPage()}>Cargar más invitaciones</button>}</section>
    <section aria-labelledby="accounts-heading"><h2 id="accounts-heading">Cuentas</h2>{accounts.isPending ? <p role="status">Cargando cuentas…</p> : accounts.isError ? <Feedback error={accounts.error}/> : <ul className="record-list">{accountItems.map(item => <li key={item.id}><div className="record-content"><h3>{item.display_name}{item.id === session.account.id ? ' (tú)' : ''}</h3><p><span className="status-label">{status(item.status)}</span>{item.is_admin && <span className="small muted"> · Administrador</span>}</p></div><div className="record-actions">{item.id !== session.account.id && item.status === 'active' && <button onClick={() => setAction({ kind: 'disable', item })} disabled={pending}>Desactivar</button>}{item.id !== session.account.id && ['active', 'disabled'].includes(item.status) && <button className="danger-text" onClick={() => setAction({ kind: 'delete', item })} disabled={pending}>Eliminar</button>}</div></li>)}</ul>}{accounts.hasNextPage && <button className="text-button" disabled={accounts.isFetchingNextPage} onClick={() => void accounts.fetchNextPage()}>Cargar más cuentas</button>}</section>
    <Dialog.Root open={action !== null} onOpenChange={open => { if (!open && !pending) setAction(null); }}><Dialog.Portal><Dialog.Overlay className="dialog-overlay"/><Dialog.Content className="dialog-content" onEscapeKeyDown={e => { if (pending) e.preventDefault(); }} onPointerDownOutside={e => { if (pending) e.preventDefault(); }}><Dialog.Title>{actionName}</Dialog.Title><Dialog.Description>{action && ('email' in action.item ? action.item.email : action.item.display_name)}. {action?.kind === 'reissue' ? 'El enlace anterior dejará de servir. Guarda el nuevo antes de cerrar.' : action?.kind === 'cancel' ? 'El enlace dejará de servir para crear una cuenta.' : 'Se bloqueará el acceso al servicio. Esto no borra contenido que ya exista en sus dispositivos. Conservamos registros mínimos de la baja para impedir reactivaciones y auditar la operación.'}</Dialog.Description><div className="actions"><button className="button" onClick={() => void confirm()} disabled={pending}>{pending ? 'Aplicando…' : actionName}</button><Dialog.Close asChild><button className="text-button" disabled={pending}>Volver</button></Dialog.Close></div></Dialog.Content></Dialog.Portal></Dialog.Root>
    <Dialog.Root open={issued !== null} onOpenChange={open => { if (!open) { setIssued(null); setCopied(false); } }}><Dialog.Portal><Dialog.Overlay className="dialog-overlay"/><Dialog.Content className="dialog-content"><Dialog.Title>Invitación preparada</Dialog.Title><Dialog.Description>Copia este enlace y compártelo con {issued?.invitation.email}. Quien tenga el enlace podrá usarlo. No podrás volver a verlo al cerrar.</Dialog.Description><label htmlFor="invitation-link">Enlace privado</label><textarea id="invitation-link" readOnly value={invitationURL} rows={4}/><div className="actions"><button className="button" onClick={async () => { try { await navigator.clipboard.writeText(invitationURL); setCopied(true); } catch { setCopied(false); document.getElementById('invitation-link')?.focus(); } }}>{copied ? 'Copiado' : 'Copiar enlace'}</button><Dialog.Close asChild><button className="text-button">Ya lo he guardado</button></Dialog.Close></div><p className="small muted">Si no puedes copiarlo, selecciona el texto. Un enlace nuevo invalidará este.</p></Dialog.Content></Dialog.Portal></Dialog.Root>
  </>;
}
