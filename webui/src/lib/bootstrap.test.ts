// @vitest-environment happy-dom
// Bootstrap from an `ak webui` launch URL (add-webui-launch-command 6.1):
// - a `?token=` parameter is stripped from the address bar immediately,
//   exchanged for a session cookie, and only on success stored for later
//   silent renewal;
// - no token means no request at all (fixed-token mode and plain reloads
//   behave exactly as before);
// - a failed exchange still cleans the URL and stores nothing.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  STORED_TOKEN_KEY,
  authState,
  clearStoredToken,
  resetAuth,
} from './credentials.js'
import { bootstrap, resetBootstrap } from './bootstrap.js'

type FetchCall = { url: string; init?: RequestInit }

function jsonResponse(status: number, body: unknown = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

function stubFetch(responses: Array<Response | Error>): { calls: FetchCall[] } {
  const calls: FetchCall[] = []
  const queue = [...responses]
  const mock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init })
    const next = queue.shift()
    if (next instanceof Error) throw next
    return next ?? jsonResponse(500)
  })
  vi.stubGlobal('fetch', mock)
  return { calls }
}

describe('launch URL bootstrap (6.1)', () => {
  beforeEach(() => {
    resetAuth()
    resetBootstrap()
    clearStoredToken()
    localStorage.clear()
    sessionStorage.clear()
  })
  afterEach(() => {
    resetAuth()
    resetBootstrap()
    localStorage.clear()
    history.replaceState({}, '', '/')
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('exchanges the token, strips it from the URL and stores it only on success', async () => {
    history.replaceState({}, '', '/?token=boot-tok&view=apps#logs')
    const { calls } = stubFetch([jsonResponse(200)])

    await bootstrap()

    expect(calls).toHaveLength(1)
    expect(calls[0]!.url).toBe('/api/auth')
    expect(calls[0]!.init?.method).toBe('POST')
    expect((calls[0]!.init?.headers as Record<string, string>)['X-Auth-Token']).toBe(
      'boot-tok',
    )
    // The token is gone from the address bar; other parameters and the hash
    // survive.
    expect(location.search).toBe('?view=apps')
    expect(location.hash).toBe('#logs')
    expect(location.href).not.toContain('boot-tok')
    // Stored for a later silent renewal.
    expect(localStorage.getItem(STORED_TOKEN_KEY)).toBe('boot-tok')
    expect(authState()).toBe('session')
  })

  it('is idempotent: a second call does not exchange again', async () => {
    history.replaceState({}, '', '/?token=boot-tok')
    const { calls } = stubFetch([jsonResponse(200)])

    await Promise.all([bootstrap(), bootstrap()])
    await bootstrap()

    expect(calls).toHaveLength(1)
    expect(localStorage.getItem(STORED_TOKEN_KEY)).toBe('boot-tok')
  })

  it('does nothing without a token — no request, no storage write', async () => {
    history.replaceState({}, '', '/app/alpha?view=logs')
    const { calls } = stubFetch([])

    await bootstrap()

    expect(calls).toEqual([])
    expect(localStorage.length).toBe(0)
    expect(authState()).toBe('silent')
    expect(location.pathname).toBe('/app/alpha')
    expect(location.search).toBe('?view=logs')
  })

  it('a rejected token still cleans the URL but is not stored', async () => {
    history.replaceState({}, '', '/?token=stale-tok')
    stubFetch([jsonResponse(401)])

    await bootstrap()

    expect(location.search).toBe('')
    expect(location.href).not.toContain('stale-tok')
    expect(localStorage.getItem(STORED_TOKEN_KEY)).toBeNull()
    expect(authState()).toBe('silent')
  })
})
