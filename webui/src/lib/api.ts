/**
 * The one place console HTTP requests are made.
 *
 * Everything that used to call `fetch('/api/...')` directly goes through
 * [`apiFetch`], so the 401 → credential → replay flow exists once instead of
 * once per call site (store.ts had two, program-detail.ts a third).
 */

import { withSession } from './credentials.js'

/** An API answer that is not 2xx, carrying the server's own message. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

/**
 * `fetch` with the console credential flow applied. Returns the raw response
 * so streaming callers keep it; use {@link apiJson} for JSON endpoints.
 */
export function apiFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const send = () =>
    fetch(path, {
      ...init,
      // Same-origin by default; stated explicitly so a caller can never widen
      // this to a cross-origin request that would drop the session cookie.
      credentials: 'same-origin',
    })
  return withSession(send)
}

/** `apiFetch` plus JSON decoding and an `ApiError` on non-2xx. */
export async function apiJson<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await apiFetch(path, init)
  const body = (await res.json().catch(() => null)) as T | { error?: string } | null
  if (!res.ok) {
    const msg =
      (body as { error?: string } | null)?.error ?? `HTTP ${res.status}`
    throw new ApiError(res.status, msg)
  }
  return body as T
}

/** POST helper for the JSON endpoints (`/api/apply`, program actions). */
export function apiPostJson<T>(path: string, payload: unknown): Promise<T> {
  return apiJson<T>(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  })
}

/**
 * Remove a whole app: `POST /api/apps/<app>/remove` (guard-remove-until-stopped).
 *
 * The daemon owns the guard — a live program yields a 409 `ApiError` carrying
 * the blocking programs and the stop-first remedy, an unknown app a 404. The
 * console never sends stop+remove and never calls apply afterwards: a 2xx
 * means the removal already took effect.
 */
export function removeApp<T = RemoveAppResult>(app: string): Promise<T> {
  return apiJson<T>(`/api/apps/${encodeURIComponent(app)}/remove`, { method: 'POST' })
}

/**
 * The removal result shared with `POST /v1/apps/<app>/remove`
 * (supervisor::RemoveOutcome). Field names are the wire contract.
 */
export interface RemoveAppResult {
  app: string
  /** The registration record path that was removed (`app_dir/<app>.toml`). */
  config_path: string
  /** True when a scaffold-generated real file was deleted with it. */
  deleted_file: boolean
  /** Deployment config body kept on disk (link registrations), else null. */
  kept_config: string | null
  /**
   * True when there was no record left to delete (removed by hand): the
   * runtime state was still cleaned up, so the removal is satisfied and no
   * apply is needed.
   */
  already_unregistered: boolean
  /** Programs dropped from the runtime projection. */
  programs: string[]
  /** Always true: the removal is immediate, no apply is needed. */
  applied: boolean
  /** One-line summary rendered by the daemon. */
  result: string
}
