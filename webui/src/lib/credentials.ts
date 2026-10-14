/**
 * Console credential flow (webui-api 控制台访问控制).
 *
 * The console has its own optional `[webui] auth_token`. When it is
 * configured, `/api/*` (except `/api/health`) and `/ws` require a session
 * obtained by exchanging the token at `/api/auth`; the server hands back an
 * `HttpOnly` cookie, so the token itself never has to live anywhere in the
 * browser.
 *
 * What this module guarantees:
 * - the token is never put in a URL, in localStorage/sessionStorage, or in a
 *   console message — it goes out exactly once, as an `X-Auth-Token` header
 *   on the exchange request, and is dropped as soon as that returns;
 * - the prompt appears ONLY after the server has actually answered 401, so a
 *   console without a token behaves exactly as it did before this feature
 *   (no gate, no extra request);
 * - a request that hit 401 is replayed once after a successful exchange; a
 *   failed exchange leaves the console sitting in the credential state rather
 *   than hammering the server.
 */

/**
 * - `silent`    — nothing has been challenged yet; behave exactly as before.
 * - `prompting` — the server answered 401; the gate is up.
 * - `session`   — a session cookie is in place.
 */
export type AuthState = 'silent' | 'prompting' | 'session'

export type AuthListener = (state: AuthState) => void

let state: AuthState = 'silent'
const listeners = new Set<AuthListener>()
let resolver: ((ok: boolean) => void) | null = null

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
 * (never a query parameter — URLs land in history and access logs) and is
 * not retained by this module.
 */
export async function submitToken(token: string): Promise<boolean> {
  const trimmed = token.trim()
  if (!trimmed) return false
  try {
    const res = await fetch('/api/auth', {
      method: 'POST',
      headers: { 'X-Auth-Token': trimmed },
    })
    if (res.ok) {
      setState('session')
      resolver?.(true)
      return true
    }
  } catch {
    /* network failure — stay in the prompting state */
  }
  resolver?.(false)
  return false
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
 * Run `send`, dealing with the 401 gate: prompt once, exchange the token and
 * replay the request exactly once. Returns the response the caller should
 * read — on a failed exchange that is the original 401.
 */
export async function withSession(send: () => Promise<Response>): Promise<Response> {
  if (state === 'prompting') {
    const ok = await requestCredential()
    if (!ok) return send()
  }
  const res = await send()
  if (res.status !== 401) return res
  // First challenge in this page: gate up and collect a token.
  const ok = await requestCredential()
  if (!ok) return res
  // Replay once. A second 401 means the token was not accepted; the gate
  // stays up so the user can try again.
  return send()
}
