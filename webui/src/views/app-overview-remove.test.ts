// @vitest-environment happy-dom
//
// guard-remove-until-stopped 4.2 / 4.3: the App overview remove action —
// confirmation boundary, no request on cancel, 409 keeps the page with the
// daemon's blockers, success reports "no apply needed" and navigates away,
// and the sidebar/projection converge after the removal.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import '../views/app-overview.js'
import type { AkAppOverview } from './app-overview.js'
import { ConsoleStore } from '../lib/store.js'
import { MSG, type ProgramInfo, type StatusDoc } from '../lib/types.js'
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
  serverOpen(): void {
    this.readyState = FakeWebSocket.OPEN
    this.onopen?.()
  }
  serverFrame(frame: Uint8Array): void {
    this.onmessage?.({ data: new Uint8Array(frame).buffer })
  }
}

function program(app: string, name: string, state = 'stopped'): ProgramInfo {
  return {
    app,
    name,
    state,
    pid: state === 'running' ? 42 : null,
    unhealthy: false,
    uptime_secs: 0,
    total_exits: 0,
    restart_backoff: 1,
    last_exit: null,
    fatal_reason: null,
    wait_reason: null,
    cpu_percent: null,
    mem_bytes: null,
    log_rate: {
      out: { w1: 0, w10: 0, w60: 0, w300: 0 },
      err: { w1: 0, w10: 0, w60: 0, w300: 0 },
    },
    command: 'sleep 300',
    args: [],
    work_dir: '',
  }
}

function baseDoc(programs: ProgramInfo[]): StatusDoc {
  return {
    daemon: {
      version: '0.1.0',
      port: 9877,
      apps: 2,
      system: { cpu_percent: 5, mem_used_bytes: 1, mem_total_bytes: 2 },
      monitor_interval: 1,
      uptime_secs: 10,
      config_source: '/t',
    },
    pending: { programs: [], apps_added: [], apps_removed: [], daemon_hints: [], errors: [] },
    programs,
  }
}

function snapshotFrame(doc: StatusDoc): Uint8Array {
  const payload = encode(doc)
  const out = new Uint8Array(payload.length + 1)
  out[0] = MSG.SNAPSHOT
  out.set(payload, 1)
  return out
}

const REMOVED_RESULT = {
  app: 'alpha',
  config_path: '/apps/alpha.toml',
  deleted_file: true,
  kept_config: null,
  already_unregistered: false,
  programs: ['web', 'worker'],
  applied: true,
  result: 'app[alpha] removed (generated config file removed: /apps/alpha.toml); no apply needed',
}

describe('app overview remove action (guard-remove-until-stopped)', () => {
  let store: ConsoleStore
  let ws: FakeWebSocket
  let fetchMock: ReturnType<typeof vi.fn>

  beforeEach(async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket)
    fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    FakeWebSocket.instances = []
    history.replaceState({}, '', '/app/alpha')
    store = new ConsoleStore()
    store.start()
    ws = FakeWebSocket.instances[0]!
    ws.serverOpen()
    ws.serverFrame(
      snapshotFrame(baseDoc([program('alpha', 'web'), program('alpha', 'worker'), program('beta', 'job')])),
    )
    await new Promise((r) => setTimeout(r, 0))
  })

  afterEach(() => {
    store.stop()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  async function mountOverview(app: string): Promise<AkAppOverview> {
    const el = document.createElement('ak-app-overview') as AkAppOverview
    el.store = store
    el.app = app
    document.body.appendChild(el)
    await el.updateComplete
    return el
  }

  function removeButton(el: AkAppOverview): HTMLButtonElement {
    const b = el.shadowRoot!.querySelector<HTMLButtonElement>('button[data-kind="remove"]')
    expect(b).not.toBeNull()
    return b!
  }

  it('confirming cancel sends no request and changes nothing', async () => {
    const view = await mountOverview('alpha')
    const confirm = vi.fn().mockReturnValue(false)
    vi.stubGlobal('confirm', confirm)
    removeButton(view).click()
    await new Promise((r) => setTimeout(r, 0))
    await view.updateComplete

    expect(confirm).toHaveBeenCalledTimes(1)
    // The confirm text names the configuration boundary and the no-auto-stop rule.
    const text = String(confirm.mock.calls[0]![0])
    expect(text).toContain('app_dir')
    expect(text).toContain('外部部署配置')
    expect(text).toContain('不会自动停止')
    expect(fetchMock).not.toHaveBeenCalled()
    expect(view.shadowRoot!.querySelector('.remove-strip')).toBeNull()
    expect(store.getSnapshot().doc!.programs.map((p) => p.name)).toEqual(['web', 'worker', 'job'])
    view.remove()
  })

  it('a 409 shows the blocking reason and keeps the page and the app', async () => {
    const view = await mountOverview('alpha')
    fetchMock.mockResolvedValueOnce({
      ok: false,
      status: 409,
      json: async () => ({
        error:
          'app "alpha" still has live program(s): web (running); run `ak stop alpha` first, then remove',
      }),
    })
    vi.stubGlobal('confirm', vi.fn().mockReturnValue(true))
    removeButton(view).click()
    await new Promise((r) => setTimeout(r, 0))
    await view.updateComplete

    const [url, init] = fetchMock.mock.calls.at(-1) as unknown as [string, RequestInit]
    expect(url).toBe('/api/apps/alpha/remove')
    expect(init.method).toBe('POST')
    // Exactly one request — never a stop+remove pair.
    expect(fetchMock).toHaveBeenCalledTimes(1)

    const strip = view.shadowRoot!.querySelector('.remove-strip')
    expect(strip).not.toBeNull()
    expect(strip!.getAttribute('data-error')).toBe('true')
    expect(strip!.textContent).toContain('web (running)')
    expect(strip!.textContent).toContain('ak stop alpha')
    // The app is still projected and still navigable.
    expect(view.shadowRoot!.querySelector('h1')!.textContent).toContain('alpha')
    expect(store.getSnapshot().doc!.programs.some((p) => p.app === 'alpha')).toBe(true)
    view.remove()
  })

  it('success reports no-apply-needed, drops the app and navigates to a live overview', async () => {
    const view = await mountOverview('alpha')
    fetchMock.mockResolvedValueOnce({ ok: true, status: 200, json: async () => REMOVED_RESULT })
    vi.stubGlobal('confirm', vi.fn().mockReturnValue(true))
    removeButton(view).click()
    await new Promise((r) => setTimeout(r, 0))
    await view.updateComplete

    // The projection lost alpha's programs immediately (2xx is authoritative).
    expect(store.getSnapshot().doc!.programs.map((p) => p.name)).toEqual(['job'])
    // The success text says no apply is needed and survives the navigation.
    const flash = store.flash
    expect(flash).not.toBeNull()
    expect(flash!.text).toContain('no apply needed')
    // Navigated to a still-available overview (beta), not the dead app.
    expect(location.pathname).toBe('/app/beta')
    view.remove()
  })

  it('removing the last app falls back to the dashboard', async () => {
    history.replaceState({}, '', '/app/alpha')
    const solo = await mountOverview('alpha')
    // Only alpha remains in the projection.
    ws.serverFrame(snapshotFrame(baseDoc([program('alpha', 'web')])))
    await new Promise((r) => setTimeout(r, 0))
    await solo.updateComplete
    fetchMock.mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => ({ ...REMOVED_RESULT, programs: ['web'] }),
    })
    vi.stubGlobal('confirm', vi.fn().mockReturnValue(true))
    removeButton(solo).click()
    await new Promise((r) => setTimeout(r, 0))
    expect(location.pathname).toBe('/')
    solo.remove()
  })

  it('a WS STATUS delta with `removed` drops stale programs from the projection', async () => {
    const text = new TextEncoder().encode(
      JSON.stringify({
        programs: [program('beta', 'job', 'running')],
        removed: ['web', 'worker'],
      }),
    )
    const frame = new Uint8Array(text.length + 1)
    frame[0] = MSG.STATUS
    frame.set(text, 1)
    ws.serverFrame(frame)
    await new Promise((r) => setTimeout(r, 0))
    await new Promise((r) => setTimeout(r, 0))

    expect(store.getSnapshot().doc!.programs.map((p) => p.name)).toEqual(['job'])
    // Other apps are untouched.
    expect(store.getSnapshot().doc!.programs[0]!.app).toBe('beta')
  })
})
