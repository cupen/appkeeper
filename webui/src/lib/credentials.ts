/**
 * Console credential flow (webui-api 控制台访问控制).
 *
 * The console has its own optional `[webui] auth_token`. When it is
 * configured, `/api/*` (except `/api/health`) and `/ws` require a session
 * obtained by exchanging the token at `/api/auth`; the server hands back an
 * `HttpOnly` cookie, so the token itself never has to travel with ordinary
 * requests.
 *
 * What this module guarantees:
 * - a token typed into the gate is never put in a URL, in storage, or in a
 *   console message — it goes out exactly once, as an `X-Auth-Token` header
 *   on the exchange request, and is dropped as soon as that returns;
 * - the only token written to storage is the one delivered by the bootstrap
 *   URL (`bootstrap.ts`); a reload or a new tab renews the session from it
 *   silently, and a rejected stored token is cleared immediately;
 * - the prompt appears ONLY after the server has actually answered 401 and
 *   no stored token could establish a session, so a console without a token
 *   behaves exactly as it did before this feature (no gate, no extra
 *   request);
 * - a request that hit 401 is replayed once after a successful exchange; a
 *   failed exchange leaves the console in the credential state rather than
 *   hammering the server.
 */

/**
 * - `silent`    — nothing has been challenged yet; behave exactly as before.
 * - `prompting` — the server answered 401; the gate is up.
 * - `session`   — a session cookie is in place.
 */
export type AuthState = 'silent' | 'prompting' | 'session'

export type AuthListener = (state: AuthState) => void

/** localStorage key for the bootstrap token (origin-isolated by the browser). */
export const STORED_TOKEN_KEY = 'ak.webui.token'

let state: AuthState = 'silent'
const listeners = new Set<AuthListener>()
let resolver: ((ok: boolean) => void) | null = null
/** True when the stored token was rejected (rotated/daemon restart). */
let storedRejected = false
/** Single-flight silent renewal: concurrent 401s share one exchange. */
let refreshing: Promise<boolean> | null = null

export function authState(): AuthState {
  return state
}

export function onAuthChange(listener: AuthListener): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

function setState(next: AuthState): void {
  if (state === next) return
  state = next
  for (const l of listeners) l(next)
}

/** Test seam: forget any session and close the gate. */
export function resetAuth(): void {
  setState('silent')
  resolver?.(false)
  resolver = null
  storedRejected = false
  refreshing = null
}

/**
 * Put the gate up and wait for the user to submit or cancel.
 * Resolves `true` when a session was established.
 */
export function requestCredential(): Promise<boolean> {
  // Already prompting: the second caller joins the first one's prompt
  // rather than stacking two gates.
  if (resolver) return new Promise<boolean>((r) => queueMicrotask(() => r(false)))
  setState('prompting')
  return new Promise<boolean>((resolve) => {
    resolver = (ok: boolean) => {
      resolver = null
      resolve(ok)
    }
  })
}

/**
 * Exchange the token for a session cookie. The token is passed as a header
 * (never a query parameter — URLs land in history and access logs). Used by
 * the bootstrap flow, which must not go through the gate.
 */
export async function exchangeToken(token: string): Promise<boolean> {
  const trimmed = token.trim()
  if (!trimmed) return false
  try {
    const res = await fetch('/api/auth', {
      method: 'POST',
      headers: { 'X-Auth-Token': trimmed },
    })
    if (res.ok) {
      setState('session')
      return true
    }
  } catch {
    /* network failure — stay in the current state */
  }
  return false
}

/**
 * Exchange a token typed into the gate. The token is not retained and,
 * unlike the bootstrap token, is never written to storage.
 */
export async function submitToken(token: string): Promise<boolean> {
  const trimmed = token.trim()
  if (!trimmed) return false
  const ok = await exchangeToken(trimmed)
  if (ok) storedRejected = false
  resolver?.(ok)
  return ok
}

/** Write the bootstrap token so a reload/new tab can renew silently. */
export function saveStoredToken(token: string): void {
  try {
    localStorage.setItem(STORED_TOKEN_KEY, token)
  } catch {
    /* storage unavailable (private mode/quota) — session still works */
  }
}

/** Drop the stored token (rejected by the server, or reset by tests). */
export function clearStoredToken(): void {
  try {
    localStorage.removeItem(STORED_TOKEN_KEY)
  } catch {
    /* storage unavailable — nothing to clear */
  }
}

function readStoredToken(): string | null {
  try {
    return localStorage.getItem(STORED_TOKEN_KEY)
  } catch {
    return null
  }
}

/**
 * True after a stored token was rejected by `/api/auth`; the gate uses it to
 * explain that `ak webui` must be run again rather than showing a generic
 * failure.
 */
export function storedTokenRejected(): boolean {
  return storedRejected
}

/**
 * Renew the session from the stored token, at most once at a time. Returns
 * `false` when there is nothing stored. A rejection clears the stored value
 * and raises the `storedTokenRejected` flag.
 */
function refreshFromStorage(): Promise<boolean> {
  if (refreshing) return refreshing
  const token = readStoredToken()
  if (!token) return Promise.resolve(false)
  refreshing = exchangeToken(token).then((ok) => {
    refreshing = null
    if (!ok) {
      clearStoredToken()
      storedRejected = true
    }
    return ok
  })
  return refreshing
}

/** The user dismissed the gate. */
export function cancelCredential(): void {
  setState('silent')
  resolver?.(false)
}

/** True when the caller should show the gate right now. */
export function shouldPrompt(): boolean {
  return state === 'prompting'
}

/**
 * Run `send`, dealing with the 401 gate: first try the stored token silently
 * (one exchange shared by concurrent 401s), then prompt once, exchange the
 * typed token and replay the request exactly once. Returns the response the
 * caller should read — on a failed exchange that is the original 401.
 */
export async function withSession(send: () => Promise<Response>): Promise<Response> {
  if (state === 'prompting') {
    const ok = await requestCredential()
    if (!ok) return send()
  }
  const res = await send()
  if (res.status !== 401) return res

  // A token from a previous `ak webui` bootstrap: renew without the gate.
  if (await refreshFromStorage()) return send()

  const ok = await requestCredential()
  if (!ok) return res
  // Replay once. A second 401 means the token was not accepted; the gate
  // stays up so the user can try again.
  return send()
}
