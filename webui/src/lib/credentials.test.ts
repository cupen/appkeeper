// @vitest-environment happy-dom
// Unit tests for the console credential flow (unix-socket-control-plane
// tasks 6.5 / 6.6 / 6.8):
// - 6.5  no token configured → a request succeeds directly; a 401 raises the
//        gate, a successful exchange replays the request exactly once, a
//        failed exchange replays nothing;
// - 6.6  the token never lands in a URL, in localStorage / sessionStorage /
//        document.cookie, or in a console log;
// - 6.8  without a 401 challenge the flow is byte-for-byte the old behavior:
//        no gate, no state change, no extra request.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  authState,
  cancelCredential,
  onAuthChange,
  requestCredential,
  resetAuth,
  shouldPrompt,
  submitToken,
  withSession,
} from './credentials.js'

type FetchCall = { url: string; init?: RequestInit }

function jsonResponse(status: number, body: unknown = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

/**
 * Stub the global fetch with a queue of answers (a thrown Error simulates a
 * network failure). Returns every call so tests can assert where the token
 * went.
 */
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

/** Wait until `cond` holds, pumping microtasks — no fake timers needed. */
async function until(cond: () => boolean): Promise<void> {
  for (let i = 0; i < 100 && !cond(); i++) {
    await new Promise((r) => setTimeout(r, 0))
  }
  expect(cond(), 'condition never became true').toBe(true)
}

describe('credential flow (6.5)', () => {
  beforeEach(() => {
    resetAuth()
  })
  afterEach(() => {
    resetAuth()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('a response that is not a 401 passes straight through — no gate, no exchange', async () => {
    const { calls } = stubFetch([])
    const stateChanges: string[] = []
    onAuthChange((s) => stateChanges.push(s))
    const send = vi.fn(async () => jsonResponse(200, { ok: 1 }))

    const res = await withSession(send)

    expect(res.status).toBe(200)
    expect(send).toHaveBeenCalledTimes(1)
    expect(calls).toEqual([]) // no /api/auth probe behind the caller's back
    expect(authState()).toBe('silent')
    expect(stateChanges).toEqual([]) // the UI never flickered
  })

  it('401 → successful exchange → the original request is replayed exactly once', async () => {
    const { calls } = stubFetch([jsonResponse(200)]) // /api/auth accepts
    const send = vi
      .fn<(init?: RequestInit) => Promise<Response>>()
      .mockResolvedValueOnce(jsonResponse(401, { error: 'credential required' }))
      .mockResolvedValueOnce(jsonResponse(200, { ok: 1 }))

    const pending = withSession(() => send())
    await until(() => shouldPrompt())
    expect(authState()).toBe('prompting')

    const ok = await submitToken('  s3cret-tok  ')
    expect(ok).toBe(true)
    expect(authState()).toBe('session')

    const res = await pending
    expect(res.status).toBe(200)
    expect(await res.json()).toEqual({ ok: 1 })
    // replayed once: the original + the replay, nothing else
    expect(send).toHaveBeenCalledTimes(2)
    // the exchange went to /api/auth with the token in a header — not a URL
    expect(calls).toHaveLength(1)
    expect(calls[0]!.url).toBe('/api/auth')
    expect(calls[0]!.init?.method).toBe('POST')
    expect((calls[0]!.init?.headers as Record<string, string>)['X-Auth-Token']).toBe(
      's3cret-tok',
    )
  })

  it('a failed exchange does not replay — the original 401 is returned and the gate stays up', async () => {
    const { calls } = stubFetch([jsonResponse(401, { error: 'no' })]) // /api/auth rejects
    const send = vi.fn(async () => jsonResponse(401, { error: 'credential required' }))

    const pending = withSession(send)
    await until(() => shouldPrompt())

    const ok = await submitToken('wrong-token')
    expect(ok).toBe(false)

    const res = await pending
    expect(res.status).toBe(401)
    // no replay: the caller's request was made exactly once
    expect(send).toHaveBeenCalledTimes(1)
    // the console stays in the credential state so the user can retry
    expect(authState()).toBe('prompting')
    expect(calls).toHaveLength(1) // only the exchange attempt
  })

  it('a network failure during the exchange behaves like a rejected token', async () => {
    const { calls } = stubFetch([new Error('offline')])
    const send = vi.fn(async () => jsonResponse(401))

    const pending = withSession(send)
    await until(() => shouldPrompt())
    expect(await submitToken('s3cret-tok')).toBe(false)

    expect((await pending).status).toBe(401)
    expect(send).toHaveBeenCalledTimes(1)
    expect(authState()).toBe('prompting')
    expect(calls).toHaveLength(1)
  })

  it('an empty token is refused without touching the network', async () => {
    const { calls } = stubFetch([])
    expect(await submitToken('   ')).toBe(false)
    expect(calls).toEqual([])
  })

  it('cancelling the gate resolves the waiter with false and closes it', async () => {
    stubFetch([])
    const pending = requestCredential()
    expect(shouldPrompt()).toBe(true)
    cancelCredential()
    expect(await pending).toBe(false)
    expect(shouldPrompt()).toBe(false)
    expect(authState()).toBe('silent')
  })

  it('a second caller joins the open gate instead of stacking a second prompt', async () => {
    stubFetch([])
    const first = requestCredential()
    const second = requestCredential()
    await until(() => shouldPrompt())
    // The second caller must already be settled (false, via a microtask) —
    // not waiting on a gate that no longer exists.
    const resolved = await Promise.race([
      second.then(() => 'second'),
      new Promise((r) => setTimeout(() => r('pending'), 10)),
    ])
    expect(resolved).toBe('second')
    cancelCredential()
    expect(await first).toBe(false)
  })
})

describe('token hygiene (6.6)', () => {
  beforeEach(() => {
    resetAuth()
    localStorage.clear()
    sessionStorage.clear()
    document.cookie = ''
  })
  afterEach(() => {
    resetAuth()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('the token never reaches a URL, persistent storage, a cookie or the console', async () => {
    const token = 's3cret-hygiene-tok'
    const { calls } = stubFetch([jsonResponse(200)])
    const consoleSpy = vi
      .spyOn(console, 'log')
      .mockImplementation(() => {})
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    const infoSpy = vi.spyOn(console, 'info').mockImplementation(() => {})
    const debugSpy = vi.spyOn(console, 'debug').mockImplementation(() => {})
    const send = vi.fn(async () => jsonResponse(401))

    const pending = withSession(send)
    await until(() => shouldPrompt())
    expect(await submitToken(token)).toBe(true)
    await pending

    // Not in any URL (no query parameters anywhere).
    for (const c of calls) expect(c.url).not.toContain(token)
    expect(calls.every((c) => !c.url.includes('?'))).toBe(true)
    // Not in persistent (or any) web storage.
    expect(localStorage.length).toBe(0)
    expect(sessionStorage.length).toBe(0)
    // Not in a readable cookie.
    expect(document.cookie).not.toContain(token)
    // Not logged.
    for (const spy of [consoleSpy, warnSpy, errorSpy, infoSpy, debugSpy]) {
      for (const call of spy.mock.calls) {
        expect(call.map(String).join(' ')).not.toContain(token)
      }
    }
  })
})
