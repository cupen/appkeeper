/**
 * Bootstrap the console from an `ak webui` launch URL
 * (add-webui-launch-command D5/D6).
 *
 * `ak webui` opens `/?token=<token>` in auto-token mode. The SPA reads that
 * parameter before the first console request, immediately strips it from the
 * address bar/history entry (so the token does not linger in history or get
 * pasted along with the URL), exchanges it for the `HttpOnly` session cookie,
 * and only on success writes it to localStorage for later silent renewal.
 *
 * The flow is idempotent at module level: every caller awaits the same
 * promise, so `ConsoleController.hostConnected` can gate `store.start()` on
 * it without risking a second exchange.
 *
 * Fixed-token mode never puts the token in the URL, so this module does
 * nothing there — the 401 gate remains the only way in.
 */

import { exchangeToken, saveStoredToken } from './credentials.js'

let pending: Promise<void> | null = null

/** Run the bootstrap flow once (module-level idempotence). */
export function bootstrap(): Promise<void> {
  pending ??= run()
  return pending
}

/** Test seam: forget the memoized promise. */
export function resetBootstrap(): void {
  pending = null
}

/**
 * Remove `token` from the current address bar/history entry while preserving
 * every other query parameter and the hash. Runs before the exchange so the
 * token disappears even if the network call is slow or fails.
 */
function takeTokenFromAddressBar(): string | null {
  const params = new URLSearchParams(location.search)
  const token = params.get('token')?.trim()
  if (!token) return null
  params.delete('token')
  const query = params.toString()
  history.replaceState(
    history.state,
    '',
    `${location.pathname}${query ? `?${query}` : ''}${location.hash}`,
  )
  return token
}

async function run(): Promise<void> {
  const token = takeTokenFromAddressBar()
  if (!token) return
  if (await exchangeToken(token)) saveStoredToken(token)
}
