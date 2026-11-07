import { useMutation } from '@tanstack/react-query';
import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import { joinWaitlist, looksLikeEmail, type WaitlistResult } from './waitlist';

type Outcome = WaitlistResult | { kind: 'invalid_local' };

export type Waitlist = ReturnType<typeof useWaitlist>;

// One state for every form on the page: joining from the hero also settles the closing form.
export function useWaitlist() {
  const [email, setEmail] = useState('');
  const [origin, setOrigin] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [joined, setJoined] = useState<string | null>(null);
  const mutation = useMutation({ mutationFn: joinWaitlist });
  async function submit(from: string) {
    if (mutation.isPending) return;
    const value = email.trim();
    setOrigin(from);
    if (!looksLikeEmail(value)) { setOutcome({ kind: 'invalid_local' }); return; }
    setOutcome(null);
    const result = await mutation.mutateAsync(value);
    setOutcome(result);
    if (result.kind === 'accepted') setJoined(value);
  }
  function again() { setJoined(null); setOutcome(null); setEmail(''); }
  return { email, setEmail, origin, outcome, joined, pending: mutation.isPending, submit, again };
}

function errorText(outcome: Outcome | null): string | null {
  switch (outcome?.kind) {
    case 'invalid_local':
    case 'invalid': return 'Please enter a valid email address.';
    case 'rate_limited': return outcome.retryAfterSeconds === null ? 'Too many attempts. Please try again later.' : `Too many attempts. Try again in ${outcome.retryAfterSeconds} seconds.`;
    case 'failed': return 'We could not add you. Your email is still here; please try again shortly.';
    default: return null;
  }
}

export function WaitlistForm({ waitlist, name, tone = 'paper', submitLabel = 'Join the waitlist' }: { waitlist: Waitlist; name: string; tone?: 'paper' | 'stage'; submitLabel?: string }) {
  const id = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const doneRef = useRef<HTMLDivElement>(null);
  const mine = waitlist.origin === name;
  const error = mine ? errorText(waitlist.outcome) : null;
  const invalid = mine && (waitlist.outcome?.kind === 'invalid' || waitlist.outcome?.kind === 'invalid_local');

  useEffect(() => {
    if (!mine) return;
    if (waitlist.joined) doneRef.current?.focus();
    else if (error) inputRef.current?.focus();
  }, [mine, waitlist.joined, error]);

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    void waitlist.submit(name);
  }

  if (waitlist.joined) {
    return (
      <div className={`waitlist is-done tone-${tone}`} ref={doneRef} tabIndex={-1} role={mine ? 'status' : undefined}>
        <p className="waitlist-done-title"><span className="waitlist-check" aria-hidden="true"/>You’re on the list.</p>
        <p className="waitlist-done-body">We saved <strong>{waitlist.joined}</strong>. There is no confirmation email. When your turn comes, you will get an email with an invitation to create your Alondra Cloud account. Invitations go out in small batches.</p>
        <button type="button" className="waitlist-again" onClick={waitlist.again}>Add another email</button>
      </div>
    );
  }

  return (
    <form className={`waitlist tone-${tone}`} onSubmit={onSubmit} noValidate aria-busy={waitlist.pending && mine}>
      <label className="waitlist-label" htmlFor={`${id}-email`}>Your email</label>
      <div className="waitlist-row">
        <input
          ref={inputRef}
          id={`${id}-email`}
          className="waitlist-input"
          type="email"
          name="email"
          autoComplete="email"
          inputMode="email"
          autoCapitalize="none"
          spellCheck={false}
          placeholder="you@example.com"
          required
          value={waitlist.email}
          readOnly={waitlist.pending}
          aria-invalid={invalid || undefined}
          aria-describedby={`${id}-hint${error ? ` ${id}-error` : ''}`}
          onChange={event => waitlist.setEmail(event.target.value)}
        />
        <button className="waitlist-submit" type="submit" disabled={waitlist.pending}>
          {waitlist.pending && mine ? <><span className="waitlist-spinner" aria-hidden="true"/>Sending…</> : submitLabel}
        </button>
      </div>
      <p className="waitlist-hint" id={`${id}-hint`}>Just your email, stored so we can send you an invitation to create your account.</p>
      <p className="waitlist-live" role="status">{waitlist.pending && mine ? 'Sending your email…' : ''}</p>
      {error && <p className="waitlist-error" id={`${id}-error`} role="alert">{error}</p>}
    </form>
  );
}
