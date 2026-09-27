import * as Dialog from '@radix-ui/react-dialog';
import { useId, useRef, useState, type FormEvent } from 'react';
import { api, ApiError } from '../lib/api';
import { Feedback } from './private';

type Props = {
  kind: 'macs' | 'clients'; id: string; label: string | null; revision: number;
  csrf: string; disabled: boolean; onSaved: (message: string) => void;
};
type Attempt = { kind: Props['kind']; id: string; label: string | null; expected_revision: number; key: string };

export function DeviceLabel({ kind, id, label, revision, csrf, disabled, onSaved }: Props) {
  const inputId = useId();
  const hintId = useId();
  const busy = useRef(false);
  const [open, setOpen] = useState(false);
  const [text, setText] = useState('');
  const [pending, setPending] = useState(false);
  const [selection, setSelection] = useState({ kind, id, revision });
  const [attempt, setAttempt] = useState<Attempt | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [conflict, setConflict] = useState(false);
  const normalized = text.normalize('NFC').trim();
  const invalid = [...normalized].length > 128 || /[\p{Cc}\p{Cf}]/u.test(normalized);

  function begin() {
    setSelection({ kind, id, revision });
    setText(label ?? ''); setAttempt(null); setError(null); setConflict(false); setOpen(true);
  }
  async function save(event: FormEvent) {
    event.preventDefault();
    if (busy.current || invalid || conflict || disabled) return;
    // Freeze the version, payload and key after the first send. A lost response
    // must never turn a changed draft into an unrelated second mutation.
    const selected = attempt ?? { kind: selection.kind, id: selection.id, label: normalized || null, expected_revision: selection.revision, key: crypto.randomUUID() };
    setAttempt(selected); busy.current = true; setPending(true); setError(null);
    try {
      await api(`/v1/account/devices/${selected.kind}/${encodeURIComponent(selected.id)}/label`, {
        method: 'POST', csrf, key: selected.key,
        body: { label: selected.label, expected_revision: selected.expected_revision },
      });
      setOpen(false); onSaved('Nombre guardado.');
    } catch (error) {
      if (error instanceof ApiError && error.code === 'action_already_applied') {
        setOpen(false); onSaved('El cambio ya se había registrado. Actualizando la lista…');
      } else if (error instanceof ApiError && error.status === 409) {
        setConflict(true);
      } else if (error instanceof ApiError && error.status === 422) {
        setAttempt(null); setError(new Error('Usa un nombre de hasta 128 caracteres, sin caracteres de control.'));
      } else setError(error);
    } finally { busy.current = false; setPending(false); }
  }
  const device = kind === 'macs' ? 'Mac' : 'cliente';
  return <Dialog.Root open={open} onOpenChange={value => { if (!pending) setOpen(value); }}>
    <Dialog.Trigger asChild><button className="text-button" disabled={disabled} onClick={begin}>Cambiar nombre del {device}</button></Dialog.Trigger>
    <Dialog.Portal><Dialog.Overlay className="dialog-overlay"/><Dialog.Content className="dialog-content"
      onEscapeKeyDown={event => { if (pending) event.preventDefault(); }}
      onPointerDownOutside={event => { if (pending) event.preventDefault(); }}>
      <Dialog.Title>Un nombre para reconocerlo</Dialog.Title>
      <Dialog.Description>Este nombre se guarda en tu cuenta de Alondra y el servicio puede verlo. No cambia la identidad ni los permisos del dispositivo.</Dialog.Description>
      <p className="small device-identity"><code>{selection.id}</code></p>
      <form onSubmit={event => void save(event)}>
        <label htmlFor={inputId}>Nombre del {device}</label>
        <input id={inputId} value={text} onChange={event => setText(event.target.value)} disabled={pending || attempt !== null || conflict}
          aria-describedby={hintId} aria-invalid={invalid || undefined} autoComplete="off"/>
        <p id={hintId} className="small muted">Hasta 128 caracteres. Déjalo vacío para usar su identificador.</p>
        {invalid && <p role="alert">Usa un nombre de hasta 128 caracteres, sin caracteres de control.</p>}
        {error instanceof Error && !(error instanceof ApiError) ? <p className="feedback error" role="alert">{error.message}</p> : error != null && <Feedback error={error}/>}
        {conflict && <p className="feedback error" role="alert">El nombre ha cambiado en otra sesión. Actualiza la lista y vuelve a abrir este dispositivo antes de editarlo.</p>}
        <div className="actions">
          {conflict ? <button type="button" className="button" onClick={() => { setOpen(false); onSaved('Actualizando los nombres…'); }}>Actualizar lista</button>
            : <button type="submit" className="button" disabled={pending || disabled || invalid}>{pending ? 'Guardando…' : attempt ? 'Reintentar el mismo cambio' : 'Guardar nombre'}</button>}
          <Dialog.Close asChild><button type="button" className="text-button" disabled={pending}>Volver</button></Dialog.Close>
        </div>
      </form>
    </Dialog.Content></Dialog.Portal>
  </Dialog.Root>;
}
