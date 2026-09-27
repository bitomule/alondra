export type WaitlistResult =
  | { kind: 'accepted' }
  | { kind: 'invalid' }
  | { kind: 'rate_limited'; retryAfterSeconds: number | null }
  | { kind: 'failed' };

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export function looksLikeEmail(value: string): boolean {
  return value.length <= 254 && EMAIL.test(value);
}

// The API answers 202 for new and already-listed emails alike, so the page never learns which.
export async function joinWaitlist(email: string): Promise<WaitlistResult> {
  let response: Response;
  try {
    response = await fetch('/api/waitlist', {
      method: 'POST',
      credentials: 'same-origin',
      cache: 'no-store',
      headers: { Accept: 'application/json', 'Content-Type': 'application/json' },
      body: JSON.stringify({ email }),
      signal: AbortSignal.timeout(20000),
    });
  } catch {
    return { kind: 'failed' };
  }
  if (response.status === 202) return { kind: 'accepted' };
  if (response.status === 429) return { kind: 'rate_limited', retryAfterSeconds: retryAfter(response.headers.get('Retry-After')) };
  if (response.status === 400) {
    const body = await response.json().catch(() => null);
    if (body?.error === 'invalid_email') return { kind: 'invalid' };
  }
  return { kind: 'failed' };
}

export function retryAfter(header: string | null, now = Date.now()): number | null {
  if (!header) return null;
  const value = header.trim();
  if (/^\d+$/.test(value)) return Number(value);
  const at = Date.parse(value);
  return Number.isNaN(at) ? null : Math.max(0, Math.ceil((at - now) / 1000));
}

export function waitPhrase(seconds: number | null): string {
  if (seconds === null) return 'dentro de un rato';
  if (seconds < 60) return seconds <= 1 ? 'en un segundo' : `en ${seconds} segundos`;
  const minutes = Math.ceil(seconds / 60);
  if (minutes < 60) return minutes === 1 ? 'en un minuto' : `en ${minutes} minutos`;
  const hours = Math.ceil(minutes / 60);
  return hours === 1 ? 'en una hora' : `en ${hours} horas`;
}
