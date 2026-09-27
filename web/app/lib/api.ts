export type Account = { id: string; status: string; is_admin: boolean; display_name: string };
export type AdminAccount = Account & { email?: string | null };
export type Session = { csrf_token: string; account: Account };
export type Invitation = { id: string; email: string; status: string; expires_at_ms: number; account_id: string | null };
export type IssuedInvitation = { invitation: Invitation; token: string };
export type Mac = { mac_id: string; label?: string };

export class ApiError extends Error {
  constructor(public status: number, public code: string) { super(code); }
}

// Cookies are HttpOnly. Invitation tokens and passwords never enter query keys,
// browser storage, logs or the shared query cache.
export async function api<T>(path: string, options: { method?: string; body?: unknown; csrf?: string; key?: string; signal?: AbortSignal } = {}): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.body !== undefined) headers['Content-Type'] = 'application/json';
  if (options.csrf) headers['X-CSRF-Token'] = options.csrf;
  if (options.key) headers['Idempotency-Key'] = options.key;
  let response: Response;
  try {
    const timeout = AbortSignal.timeout(45000);
    response = await fetch(path, { method: options.method ?? 'GET', credentials: 'same-origin', cache: 'no-store', headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body), signal: options.signal ? AbortSignal.any([options.signal, timeout]) : timeout });
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') throw error;
    throw new ApiError(0, 'network_error');
  }
  if (!response.ok) {
    const body = await response.json().catch(() => null);
    throw new ApiError(response.status, typeof body?.error?.code === 'string' ? body.error.code : 'request_failed');
  }
  return response.status === 204 ? undefined as T : response.json();
}

export function errorMessage(error: unknown): string {
  if (!(error instanceof ApiError)) return 'No se ha podido completar la operación. Vuelve a intentarlo.';
  if (error.code === 'account_closure_unconfirmed') return 'No podemos confirmar la baja y el acceso a la cuenta ya no está disponible. No vuelvas a enviar la solicitud; contacta con quien te invitó para comprobar su estado.';
  if (error.status === 401) return 'La sesión ha terminado. Vuelve a entrar para continuar.';
  if (error.status === 403) return 'Tu cuenta no tiene permiso para esta operación.';
  if (error.status === 409) return 'El estado ha cambiado. Actualiza la lista antes de continuar.';
  if (error.status === 410) return 'Esta invitación ya no está disponible. Pide un enlace nuevo a quien te invitó.';
  if (error.status === 422) return 'Revisa los datos. La contraseña debe cumplir los requisitos indicados.';
  if (error.status === 429) return 'Hay demasiados intentos. Espera un minuto antes de volver a intentarlo.';
  if (error.status === 0) return 'Se ha perdido la conexión. No podemos confirmar si la operación terminó. Comprueba el estado antes de repetirla.';
  return 'El servicio no está disponible. Comprueba el estado antes de repetir la operación.';
}
