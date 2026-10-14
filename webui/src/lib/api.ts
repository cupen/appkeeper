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
