// @vitest-environment happy-dom
// The store-side credential flow for a refused WebSocket handshake
// (unix-socket-control-plane task 6.7):
// - the browser carries the session cookie on the /ws handshake automatically
//   (same-origin); the client never puts a credential in the URL;
// - a handshake refused before it ever opened is disambiguated by one REST
//   probe: only a 401 there routes into the credential flow, any other
//   failure stays the plain offline path;
// - once a session exists, the store reconnects /ws and goes live.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ConsoleStore } from './store.js'
import { resetAuth, shouldPrompt, submitToken } from './credentials.js'
import { MSG, type ProgramInfo, type StatusDoc } from './types.js'
import { encode } from '@msgpack/msgpack'

class FakeWebSocket {
  static OPEN = 1
  static CONNECTING = 0
  static CLOSED = 3
  static instances: FakeWebSocket[] = []
  readyState = FakeWebSocket.CONNECTING
  sent: Uint8Array[] = []
  onopen: (() => void) | null = null
  onclose: (() => void) | null = null
  onerror: (() => void) | null = null
  onmessage: ((ev: { data: ArrayBuffer }) => void) | null = null

  constructor(public url: string) {
    FakeWebSocket.instances.push(this)
  }
  send(data: unknown): void {
    this.sent.push(data as Uint8Array)
  }
  close(): void {
    this.readyState = FakeWebSocket.CLOSED
    this.onclose?.()
  }
  /** The server refused the handshake before it ever opened. */
  serverRefuses(): void {
    this.readyState = FakeWebSocket.CLOSED
    this.onclose?.()
  }
  serverOpen(): void {
    this.readyState = FakeWebSocket.OPEN
    this.onopen?.()
  }
  serverFrame(frame: Uint8Array): void {
    this.onmessage?.({ data: new Uint8Array(frame).buffer })
  }
}

function frameFor(type: number, payload: Uint8Array): Uint8Array {
  const out = new Uint8Array(payload.length + 1)
  out[0] = type
  out.set(payload, 1)
  return out
}

function doc(programs: ProgramInfo[]): StatusDoc {
  return {
    daemon: {
      version: 't',
      port: 1,
      apps: 1,
      system: { cpu_percent: 1, mem_used_bytes: 1, mem_total_bytes: 2 },
      monitor_interval: 1,
      uptime_secs: 1,
      config_source: '/t',
    },
    programs,
    pending: { programs: [], apps_added: [], apps_removed: [], daemon_hints: [], errors: [] },
  }
}

function prog(name: string): ProgramInfo {
  return {
    app: 'a',
    name,
    state: 'running',
    pid: 1,
    unhealthy: false,
    uptime_secs: 1,
    total_exits: 0,
    restart_backoff: 1,
    last_exit: null,
    fatal_reason: null,
    wait_reason: null,
    cpu_percent: null,
    mem_bytes: null,
    log_rate: { out: { w1: 0, w10: 0, w60: 0, w300: 0 }, err: { w1: 0, w10: 0, w60: 0, w300: 0 } },
    command: 'true',
    args: [],
    work_dir: '',
  }
}

function jsonResponse(status: number, body: unknown = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

type FetchCall = { url: string; init?: RequestInit }
let calls: FetchCall[] = []
let answers: Array<Response | Error> = []

function stubFetch(): void {
  const mock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init })
    const next = answers.shift()
    if (next instanceof Error) throw next
    return next ?? jsonResponse(500)
  })
  vi.stubGlobal('fetch', mock)
}

async function until(cond: () => boolean): Promise<void> {
  for (let i = 0; i < 200 && !cond(); i++) {
    await new Promise((r) => setTimeout(r, 0))
  }
  expect(cond(), 'condition never became true').toBe(true)
}

describe('refused /ws handshake routes into the credential flow (6.7)', () => {
  beforeEach(() => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket)
    FakeWebSocket.instances = []
    localStorage.clear()
    resetAuth()
    calls = []
    answers = []
    stubFetch()
  })
  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
    resetAuth()
  })

  it('refused + REST 401 → credential gate → session → /ws reconnects and goes live', async () => {
    answers.push(
      jsonResponse(401), // the store's polling probe hits the same gate
      jsonResponse(401), // the handshakeRefused probe (joins the open gate)
      jsonResponse(200), // /api/auth accepts the token
      jsonResponse(401), // both probes replay and still see 401
      jsonResponse(401), // (polling replay)
    )
    const store = new ConsoleStore()
    store.start()
    const firstWs = FakeWebSocket.instances[0]!
    expect(firstWs.url).not.toContain('token') // no credential in the URL, ever
    firstWs.serverRefuses() // refused before it ever opened

    // The REST probes answered 401 → the gate is up.
    await until(() => shouldPrompt())

    // The user exchanges the token once; the session cookie is HttpOnly and
    // every later request (and the /ws handshake) carries it.
    answers.push(jsonResponse(200)) // /api/auth
    expect(await submitToken('s3cret-ws-tok')).toBe(true)

    // With a session, the store reconnects /ws.
    await until(() => FakeWebSocket.instances.length >= 2)
    const secondWs = FakeWebSocket.instances[1]!
    expect(secondWs).not.toBe(firstWs)
    expect(secondWs.url).toBe(firstWs.url) // same-origin URL, cookie rides along

    secondWs.serverOpen()
    secondWs.serverFrame(frameFor(MSG.SNAPSHOT, encode(doc([prog('p')]) as never)))
    await until(() => store.getSnapshot().transport === 'live')
    expect(store.getSnapshot().doc?.programs.map((p) => p.name)).toEqual(['p'])
  })

  it('refused + REST not-401 (backend down) stays the offline path — no gate', async () => {
    answers.push(jsonResponse(503), jsonResponse(503)) // poll + handshake probe
    const store = new ConsoleStore()
    store.start()
    FakeWebSocket.instances[0]!.serverRefuses()

    await until(() => store.getSnapshot().transport === 'offline')
    expect(shouldPrompt()).toBe(false)
    // No reconnect storm: one client so far (the backoff timer keeps running,
    // which is pre-existing behavior and not part of this flow).
    expect(FakeWebSocket.instances).toHaveLength(1)
    store.stop()
  })

  it('refused + REST 200 (no auth configured) does not raise the gate', async () => {
    answers.push(jsonResponse(200, doc([])), jsonResponse(200, doc([])))
    const store = new ConsoleStore()
    store.start()
    FakeWebSocket.instances[0]!.serverRefuses()

    await until(() => store.getSnapshot().transport === 'poll')
    expect(shouldPrompt()).toBe(false)
    store.stop()
  })
})
