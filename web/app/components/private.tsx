import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from 'react-router';
import { useState, type ReactNode } from 'react';
import { api, ApiError, errorMessage, type Session } from '../lib/api';

export function Feedback({ error }: { error: unknown }) {
  return <div className="feedback error" role="alert">{errorMessage(error)}{error instanceof ApiError && error.status === 401 && <p><a href="/auth/login">Volver a entrar</a></p>}</div>;
}

export function PrivatePage({ admin = false, children }: { admin?: boolean; children: (session: Session) => ReactNode }) {
  const session = useQuery({ queryKey: ['session'], queryFn: ({ signal }) => api<Session>('/v1/session', { signal }), staleTime: 0, refetchOnWindowFocus: 'always' });
  // Never keep private content visible on failed revalidation (including logout
  // in another tab or a revoked account). Backend checks every request too.
  if (session.isPending) return <section className="page"><p role="status">Comprobando tu sesión…</p></section>;
  if (session.isError) return <section className="page prose"><h1>Tu espacio en Alondra</h1><Feedback error={session.error}/></section>;
  if (session.data.account.status !== 'active') return <section className="page prose"><h1>Acceso desactivado</h1><p>Contacta con quien te invitó para revisar el acceso a la alpha.</p><Logout session={session.data}/></section>;
  if (admin && !session.data.account.is_admin) return <section className="page prose"><h1>Este espacio es de administración</h1><p>Tu cuenta no tiene acceso.</p><Link to="/account">Ir a mi cuenta</Link></section>;
  return <section className="page private-page"><div className="workspace-nav"><nav aria-label="Tu espacio"><Link to="/account">Mi cuenta</Link>{session.data.account.is_admin && <Link to="/admin">Administración</Link>}</nav><Logout session={session.data}/></div>{children(session.data)}</section>;
}

function Logout({ session }: { session: Session }) {
  const client = useQueryClient();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  async function logout() {
    setPending(true); setError(null);
    try {
      await api('/auth/logout', { method: 'POST', csrf: session.csrf_token, body: {} });
      await client.cancelQueries(); client.clear(); window.location.assign('/');
    } catch (e) { setError(e); setPending(false); }
  }
  return <div><button className="text-button" onClick={logout} disabled={pending}>{pending ? 'Cerrando sesión…' : 'Cerrar sesión'}</button>{error != null && <Feedback error={error}/>}</div>;
}
