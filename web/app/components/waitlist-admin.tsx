import { useInfiniteQuery, useQueryClient } from '@tanstack/react-query';
import * as Dialog from '@radix-ui/react-dialog';
import { useRef, useState } from 'react';
import { Feedback } from './private';
import { api, ApiError, errorMessage, type IssuedInvitation, type Session } from '../lib/api';

export type WaitlistEntry = { id: string; email: string; created_at_ms: number };
type Action = { kind: 'invite' | 'remove'; entry: WaitlistEntry };

// Joining the waitlist grants nothing; inviting someone from it is a separate, explicit step.
export function WaitlistAdmin({ session, onInvited }: { session: Session; onInvited: (issued: IssuedInvitation) => void }) {
  const client = useQueryClient();
  const queryKey = ['admin-waitlist', session.account.id];
  const entries = useInfiniteQuery({ queryKey, initialPageParam: '', queryFn: ({ pageParam, signal }) => api<{ entries: WaitlistEntry[]; next_cursor: string | null }>(`/v1/admin/waitlist${pageParam ? `?after=${encodeURIComponent(pageParam)}` : ''}`, { signal }), getNextPageParam: page => page.next_cursor || undefined });
  const [selected, setSelected] = useState<Action | null>(null);
  const [pending, setPending] = useState(false);
  const [notice, setNotice] = useState('');
  const [error, setError] = useState<unknown>(null);
  const unresolved = useRef(new Map<string, string>());
  const items = entries.data?.pages.flatMap(page => page.entries) ?? [];
  async function invite(entry: WaitlistEntry) {
    // An interrupted/5xx answer does not prove nothing was issued: a retry reuses the key.
    const key = unresolved.current.get(entry.id) ?? crypto.randomUUID();
    unresolved.current.set(entry.id, key);
    try {
      const issued = await api<IssuedInvitation>(`/v1/admin/waitlist/${encodeURIComponent(entry.id)}/invite`, { method: 'POST', body: { ttl_seconds: 604800 }, csrf: session.csrf_token, key });
      unresolved.current.delete(entry.id);
      onInvited(issued);
    } catch (e) {
      if (e instanceof ApiError && e.status >= 400 && e.status < 500) unresolved.current.delete(entry.id);
      if (e instanceof ApiError && e.code === 'state_conflict') setNotice('Este correo ya tiene invitación o cuenta. Puedes retirar la solicitud.');
      else if (e instanceof ApiError && e.code === 'invitation_token_already_issued') setNotice('La invitación se creó, pero este enlace ya no se puede recuperar. Emite uno nuevo desde Invitaciones.');
      else if (e instanceof ApiError && e.status === 404) setNotice('La solicitud ya no estaba en la lista. Hemos actualizado la lista.');
      else setError(e);
    }
  }
  async function remove(entry: WaitlistEntry) {
    try {
      await api<void>(`/v1/admin/waitlist/${encodeURIComponent(entry.id)}`, { method: 'DELETE', csrf: session.csrf_token });
      setNotice('Solicitud retirada de la lista de espera.');
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) setNotice('La solicitud ya no estaba en la lista. Hemos actualizado la lista.');
      else setError(e);
    }
  }
  async function confirm() {
    if (!selected || pending) return;
    setPending(true); setError(null); setNotice('');
    try {
      if (selected.kind === 'invite') await invite(selected.entry);
      else await remove(selected.entry);
    } finally {
      setPending(false); setSelected(null);
      await client.invalidateQueries({ queryKey });
    }
  }
  const actionName = selected?.kind === 'invite' ? 'Invitar' : 'Retirar solicitud';
  return <section aria-labelledby="waitlist-heading"><div className="section-heading"><h2 id="waitlist-heading">Lista de espera</h2><button className="text-button" onClick={() => void client.invalidateQueries({ queryKey })} disabled={pending || entries.isFetching}>Actualizar</button></div>
    {notice && <p className="feedback" role="status">{notice}</p>}{error != null && <Feedback error={error}/>}
    {entries.isPending ? <p className="muted">Cargando lista de espera…</p> : entries.isError ? <p className="feedback error">{errorMessage(entries.error)}</p> : items.length === 0 ? <p className="empty-state">Nadie se ha apuntado todavía.</p> : <ul className="record-list">{items.map(item => <li key={item.id}><div className="record-content"><h3>{item.email}</h3><p className="small muted">Solicitada el {new Date(item.created_at_ms).toLocaleString('es-ES')}</p></div><div className="record-actions"><button onClick={() => setSelected({ kind: 'invite', entry: item })} disabled={pending}>Invitar</button><button className="danger-text" onClick={() => setSelected({ kind: 'remove', entry: item })} disabled={pending}>Retirar</button></div></li>)}</ul>}
    {entries.hasNextPage && <button className="text-button" disabled={entries.isFetchingNextPage} onClick={() => void entries.fetchNextPage()}>Cargar más solicitudes</button>}
    <Dialog.Root open={selected !== null} onOpenChange={open => { if (!open && !pending) setSelected(null); }}><Dialog.Portal><Dialog.Overlay className="dialog-overlay"/><Dialog.Content className="dialog-content" onEscapeKeyDown={e => { if (pending) e.preventDefault(); }} onPointerDownOutside={e => { if (pending) e.preventDefault(); }}><Dialog.Title>{selected?.kind === 'invite' ? 'Invitar desde la lista de espera' : 'Retirar solicitud'}</Dialog.Title><Dialog.Description>{selected?.entry.email}. {selected?.kind === 'invite' ? 'Se crea una invitación de 7 días y la solicitud sale de la lista. Después verás el enlace para enviárselo tú.' : 'Se borrará de la lista de espera. No afecta a cuentas ni invitaciones.'}</Dialog.Description><div className="actions"><button className="button" onClick={() => void confirm()} disabled={pending}>{pending ? 'Aplicando…' : actionName}</button><Dialog.Close asChild><button className="text-button" disabled={pending}>Volver</button></Dialog.Close></div></Dialog.Content></Dialog.Portal></Dialog.Root>
  </section>;
}
