import { useEffect, useRef, useState, type FormEvent } from 'react';
import { api, type ApiError } from '../lib/api';
import { Feedback } from '../components/private';
export function meta() { return [{ title: 'Tu invitación · Alondra' }, { name: 'robots', content: 'noindex, nofollow' }]; }
type Stage = 'checking' | 'ready' | 'provisioning' | 'unavailable' | 'accepted' | 'error';
export default function InvitePage() {
  const token = useRef('');
  const initialized = useRef(false);
  const generation = useRef(0);
  const request = useRef<AbortController | null>(null);
  const formRef = useRef<HTMLFormElement | null>(null);
  const [formVersion, setFormVersion] = useState(0);
  const [stage, setStage] = useState<Stage>('checking');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [earlierPassword, setEarlierPassword] = useState(false);
  const [resuming, setResuming] = useState(false);
  async function check(current = ++generation.current) {
    request.current?.abort();
    const controller = new AbortController(); request.current = controller;
    setStage('checking'); setError(null);
    try {
      const value = await api<{ status: string }>('/v1/invitations/status', { method: 'POST', body: { token: token.current }, signal: controller.signal });
      if (current !== generation.current) return;
      setStage(value.status === 'pending' ? 'ready' : value.status === 'provisioning' ? 'provisioning' : 'unavailable');
    } catch (e) { if (current === generation.current) { setError(e); setStage('error'); } }
  }
  useEffect(() => {
    function consumeFragment() {
      const incoming = window.location.hash.slice(1);
      // A fragment-only navigation does not remount React. Invalidate every old
      // response and clear form secrets before accepting a replacement capability.
      const current = ++generation.current;
      request.current?.abort(); formRef.current?.reset();
      token.current = '';
      setFormVersion(current); setPending(false); setError(null);
      setResuming(false); setEarlierPassword(false);
      window.history.replaceState(window.history.state, '', window.location.pathname);
      if (!/^[A-Za-z0-9_-]{43}$/.test(incoming)) { setStage('unavailable'); return; }
      token.current = incoming;
      void check(current);
    }
    window.addEventListener('hashchange', consumeFragment);
    if (!initialized.current) { initialized.current = true; consumeFragment(); }
    else if (token.current) { void check(++generation.current); }
    return () => {
      window.removeEventListener('hashchange', consumeFragment);
      ++generation.current; request.current?.abort(); formRef.current?.reset();
    };
  }, []);
  async function accept(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (pending) return;
    const form = event.currentTarget;
    const data = new FormData(form);
    const passwordInput = form.elements.namedItem('password') as HTMLInputElement;
    const password = String(data.get('password') || '');
    if (password.length < 12 || password.length > 128) { passwordInput.setCustomValidity('Usa entre 12 y 128 caracteres.'); passwordInput.reportValidity(); return; }
    const current = generation.current;
    request.current?.abort();
    const controller = new AbortController(); request.current = controller;
    setPending(true); setError(null);
    try {
      const result = await api<{ credential_already_created?: boolean }>('/v1/invitations/accept', { method: 'POST', body: { token: token.current, display_name: String(data.get('display_name') || '').trim(), password }, signal: controller.signal });
      if (current !== generation.current) return;
      token.current = ''; form.reset(); setEarlierPassword(Boolean(result.credential_already_created)); setStage('accepted');
    } catch (e) { if (current === generation.current) { passwordInput.value = ''; setError(e); if ((e as ApiError).status === 410) setStage('unavailable'); } }
    finally { if (current === generation.current) setPending(false); }
  }
  return <section className="page invite-page"><p className="eyebrow">Alpha privada</p><h1>Un sitio para ti.</h1>{stage === 'checking' && <p role="status">Comprobando tu invitación…</p>}{stage === 'ready' && <><p className="lede">Crea tu cuenta y prepara tu Mac. Después podrás conectar la app y el terminal.</p>{resuming && <p className="feedback" role="status">Vamos a reanudar el alta. Si el primer intento ya creó la cuenta, conserva su contraseña: escribir otra aquí no la sustituye.</p>}<form key={formVersion} ref={formRef} className="form-panel" onSubmit={accept}><label htmlFor="display-name">Cómo te llamas</label><input id="display-name" name="display_name" autoComplete="name" required minLength={1} maxLength={80} disabled={pending}/><label htmlFor="password">Elige una contraseña</label><input id="password" name="password" type="password" autoComplete="new-password" required minLength={12} maxLength={128} disabled={pending} aria-describedby="password-hint" onInput={e => e.currentTarget.setCustomValidity('')}/><p id="password-hint" className="small muted">Entre 12 y 128 caracteres. Puedes usar una frase larga o tu gestor de contraseñas.</p>{error != null && <Feedback error={error}/>}<button className="button" disabled={pending}>{pending ? 'Creando tu cuenta…' : 'Crear mi cuenta'}</button><p className="small muted">La invitación permite crear tu cuenta. Para leer contenido, cada cliente necesitará un vínculo aprobado por tu Mac.</p></form></>}{stage === 'accepted' && <div className="feedback" role="status"><h2>Tu cuenta está preparada.</h2>{earlierPassword && <p>El primer intento ya había creado tu contraseña. Entra con la contraseña que elegiste entonces; la del último intento no la sustituye.</p>}<p>Entra para continuar con la instalación.</p><a className="button" href="/auth/login">Entrar en Alondra</a></div>}{stage === 'provisioning' && <div className="feedback"><h2>Estamos terminando el alta.</h2><p>Un intento anterior puede seguir en curso. Comprueba su estado antes de repetirlo.</p><div className="actions"><button className="button" onClick={() => { setResuming(true); setError(null); setStage('ready'); }}>Reanudar alta</button><button className="text-button" onClick={() => void check()}>Comprobar estado</button></div><p><a href="/auth/login">Si ya creaste la cuenta, entrar</a></p></div>}{stage === 'unavailable' && <div className="feedback"><h2>Necesitas una invitación válida.</h2><p>El enlace ha caducado, ya se utilizó o dejó de estar disponible. Pide uno nuevo a quien te invitó.</p><a href="/auth/login">Ya tengo cuenta</a></div>}{stage === 'error' && <><Feedback error={error}/><button className="button" onClick={() => void check()}>Comprobar otra vez</button></>}</section>;
}
