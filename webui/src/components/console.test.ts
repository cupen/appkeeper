// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import './console-tree.js'
import './overview-bar.js'
import '../views/program-detail.js'
import type { AkConsoleTree } from './console-tree.js'
import type { AkOverviewBar } from './overview-bar.js'
import type { AkProgramDetail } from '../views/program-detail.js'
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

function program(app: string, name: string, state: string, metrics = false): ProgramInfo {
  return {
    app,
    name,
    state,
    pid: state === 'running' ? 42 : null,
    unhealthy: false,
    uptime_secs: 120,
    total_exits: 3,
    restart_backoff: 1,
    last_exit: null,
    fatal_reason: state === 'fatal' ? 'retries exhausted' : null,
    wait_reason: null,
    cpu_percent: metrics && state === 'running' ? 12.5 : null,
    mem_bytes: metrics && state === 'running' ? 4096 : null,
    log_rate: {
      out: { w1: 1, w10: 2, w60: 3, w300: 4 },
      err: { w1: 0, w10: 0, w60: 0, w300: 0 },
    },
    command: 'sh',
    args: ['-c', 'while :; do echo tick; sleep 1; done'],
    work_dir: '/srv/demo',
  }
}

function snapshotFrame(doc: StatusDoc): Uint8Array {
  const payload = encode(doc)
  const out = new Uint8Array(payload.length + 1)
  out[0] = MSG.SNAPSHOT
  out.set(payload, 1)
  return out
}

/** STATUS deltas are JSON text ({programs, pending?, removed?}). */
function statusFrame(body: unknown): Uint8Array {
  const text = new TextEncoder().encode(JSON.stringify(body))
  const out = new Uint8Array(text.length + 1)
  out[0] = MSG.STATUS
  out.set(text, 1)
  return out
}

function doc(): StatusDoc {
  return {
    daemon: {
      version: '0.1.0',
      build: {
        git_commit: '02d37e02dedc',
        built_at: '2026-01-01T00:00:00Z',
        target: 'x86_64-unknown-linux-gnu',
        rustc: 'rustc 1.90.0 (0000000 2026-01-01)',
      },
      socket: '/tmp/ak-test/ak.sock',
      port: 9877,
      apps: 1,
      system: { cpu_percent: 21.5, mem_used_bytes: 4 * 1024 ** 3, mem_total_bytes: 16 * 1024 ** 3 },
      monitor_interval: 1,
      uptime_secs: 90,
      config_source: '/tmp/appkeeper.toml',
    },
    pending: { programs: [], apps_added: [], apps_removed: [], daemon_hints: [], errors: [] },
    programs: [
      program('demo', 'web', 'running', true),
      program('demo', 'worker', 'fatal', false),
      program('demo', 'never-started', 'stopped', false),
    ],
  }
}

describe('console components', () => {
  let store: ConsoleStore
  let ws: FakeWebSocket

  beforeEach(async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => doc() }))
    FakeWebSocket.instances = []
    localStorage.clear()
    store = new ConsoleStore()
    store.start()
    ws = FakeWebSocket.instances[0]!
    ws.serverOpen()
    ws.serverFrame(snapshotFrame(doc()))
    await new Promise((r) => setTimeout(r, 0))
  })

  afterEach(() => {
    store.stop()
    vi.useRealTimers()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('tree lists every program including never-started ones, with active highlighting', async () => {
    const el = document.createElement('ak-console-tree') as AkConsoleTree
    el.apps = [{ name: 'demo', programs: doc().programs }]
    el.selectedApp = 'demo'
    el.selectedProgram = 'web'
    document.body.appendChild(el)
    await el.updateComplete

    const links = [...el.shadowRoot!.querySelectorAll('a.prog')]
    expect(links.map((a) => a.textContent)).toHaveLength(3)
    expect(links.map((a) => a.getAttribute('data-state'))).toEqual(['running', 'fatal', 'stopped'])
    const active = el.shadowRoot!.querySelector('a.prog[data-active="true"]')
    expect(active?.textContent).toContain('web')
    // never-started program renders a name, not an error
    expect(el.shadowRoot!.textContent).toContain('never-started')
    el.remove()
  })

  it('overview bar shows system metrics and the refresh selector persists', async () => {
    const el = document.createElement('ak-overview-bar') as AkOverviewBar
    el.store = store
    document.body.appendChild(el)
    await el.updateComplete
    // store already has the snapshot
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete

    const text = el.shadowRoot!.textContent!
    // The version lives only in the sidebar brand slot now (webui-ui: 品牌位
    // 版本号) — the overview bar must not repeat it.
    expect(text).not.toContain('0.1.0')
    expect(el.shadowRoot!.querySelector('.version')).toBeNull()
    expect(text).toContain('/tmp/appkeeper.toml')
    expect(text).toContain('21.5%')
    expect(text).toContain('4.0 GiB')
    const select = el.shadowRoot!.querySelector('#refresh-select') as HTMLSelectElement
    expect(select).not.toBeNull()
    // user picks 10s → persisted
    select.value = '10000'
    select.dispatchEvent(new Event('change'))
    await el.updateComplete
    expect(localStorage.getItem('ak.refresh_ms')).toBe('10000')
    expect(store.refreshInterval).toBe(10000)
    el.remove()
  })

  it('program detail shows run info and controls stop through the action menu', async () => {
    const el = document.createElement('ak-program-detail') as AkProgramDetail
    el.store = store
    el.app = 'demo'
    el.program = 'web'
    document.body.appendChild(el)
    await el.updateComplete
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete

    const text = el.shadowRoot!.textContent!
    expect(text).toContain('sh -c while :; do echo tick; sleep 1; done')
    expect(text).toContain('42')
    expect(text).toContain('12.5%')
    expect(text).toContain('4.0 KiB')

    // No resident button group: one ⋯ trigger opens the shared menu
    // (webui-ui: 单程序控制).
    const buttons = [...el.shadowRoot!.querySelectorAll('button')]
    expect(buttons).toHaveLength(1)
    const trigger = el.shadowRoot!.querySelector<HTMLButtonElement>('button.op-trigger')!
    expect(trigger.getAttribute('aria-haspopup')).toBe('menu')

    // fetch mock tracks the control call
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ result: 'program web is stopped' }),
    })
    vi.stubGlobal('fetch', fetchMock)

    trigger.click()
    await el.updateComplete
    const menu = el.shadowRoot!.querySelector('ak-action-menu')!
    const items = [...menu.shadowRoot!.querySelectorAll<HTMLButtonElement>('.menu-item[data-kind]')]
    expect(items.map((b) => b.dataset.kind)).toEqual(['stop', 'restart'])
    items[0]!.click()
    await el.updateComplete
    // inline confirmation step inside the menu: no request yet
    expect(fetchMock).not.toHaveBeenCalled()
    menu.shadowRoot!.querySelector<HTMLButtonElement>('.confirm-action')!.click()
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    expect(fetchMock).toHaveBeenCalledWith('/api/programs/web/stop', { method: 'POST', credentials: 'same-origin' })
    // The result lands in the page's notice strip.
    expect(el.shadowRoot!.querySelector('.notice')!.textContent).toContain('program web is stopped')
    el.remove()
  })

  it('program detail is one column: header → metrics → command → notices → log', async () => {
    const el = document.createElement('ak-program-detail') as AkProgramDetail
    el.store = store
    el.app = 'demo'
    el.program = 'web'
    document.body.appendChild(el)
    await el.updateComplete
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete

    const root = el.shadowRoot!
    // Structural order: the log viewer closes the column and nothing sits
    // after it (webui-ui: 进程详情 / design D8).
    const blocks = [...root.querySelectorAll('header, .metrics, .command, ak-log-viewer')]
    expect(blocks.map((n) => n.tagName.toLowerCase())).toEqual([
      'header',
      'ul',
      'div',
      'ak-log-viewer',
    ])
    // The old card grid is gone.
    expect(root.querySelector('.grid')).toBeNull()

    // Six compact metrics in one strip.
    const metrics = root.querySelector('.metrics')!
    expect(metrics.querySelectorAll('li')).toHaveLength(6)
    expect(metrics.textContent).toContain('PID')
    expect(metrics.textContent).toContain('12.5%')
    expect(metrics.textContent).toContain('4.0 KiB')
    // Running without a confirmed probe yet reads as pending, never blank.
    expect(metrics.textContent).toContain('待确认')

    // Mono command block carries the command line and the working directory.
    const command = root.querySelector('.command')!
    expect(command.textContent).toContain('sh -c while :; do echo tick; sleep 1; done')
    expect(command.textContent).toContain('/srv/demo')
    expect(command.querySelector('code')).not.toBeNull()

    // Breadcrumb links back to the app overview.
    const crumb = root.querySelector('a.crumb') as HTMLAnchorElement
    expect(crumb.getAttribute('href')).toBe('/app/demo')

    // No duplicate glyph: the badge owns the shape + word, the h1 the name.
    const h1 = root.querySelector('h1')!
    expect(h1.textContent!.trim()).toBe('web')
    expect(h1.textContent).not.toContain('▶')
    const badge = root.querySelector('ak-status-badge') as unknown as { label: string }
    expect(badge.label).toBe('运行中')
    // A running program has no backoff countdown.
    expect(root.querySelector('.backoff-note')).toBeNull()
    el.remove()
  })

  it('program detail shows the backoff seconds beside the badge', async () => {
    // Bring a backing-off program into the projection via a STATUS delta.
    ws.serverFrame(
      statusFrame({
        programs: [{ ...program('demo', 'waiting', 'backoff'), restart_backoff: 5 }],
      }),
    )
    await new Promise((r) => setTimeout(r, 0))

    const el = document.createElement('ak-program-detail') as AkProgramDetail
    el.store = store
    el.app = 'demo'
    el.program = 'waiting'
    document.body.appendChild(el)
    await el.updateComplete
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete

    const root = el.shadowRoot!
    const badge = root.querySelector('ak-status-badge') as unknown as { label: string }
    expect(badge.label).toBe('退避中')
    // The seconds are secondary text beside the pill, never inside it
    // (webui-ui: 状态展示 / backoff 状态可见).
    const note = root.querySelector('.backoff-note')
    expect(note?.textContent).toBe('5s 后重启')
    expect(root.querySelector('ak-status-badge .backoff-note')).toBeNull()
    el.remove()
  })

  it('program detail swallows the crumb click that dismisses the action menu', async () => {
    const el = document.createElement('ak-program-detail') as AkProgramDetail
    el.store = store
    el.app = 'demo'
    el.program = 'web'
    document.body.appendChild(el)
    await el.updateComplete
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete

    // Right-click anywhere opens the page's menu; a pointerdown outside then
    // closes it before the click lands on the crumb (design D2).
    const now = vi.spyOn(performance, 'now').mockReturnValue(5000)
    el.dispatchEvent(
      new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }),
    )
    await el.updateComplete
    document.body.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, composed: true }))
    await el.updateComplete

    const crumb = el.shadowRoot!.querySelector('a.crumb') as HTMLAnchorElement
    const dismissClick = new MouseEvent('click', {
      bubbles: true,
      composed: true,
      cancelable: true,
      button: 0,
    })
    crumb.dispatchEvent(dismissClick)
    expect(dismissClick.defaultPrevented).toBe(true) // shell interceptor will skip it

    // Once the window has passed, a click follows the breadcrumb normally.
    now.mockReturnValue(5200)
    const laterClick = new MouseEvent('click', {
      bubbles: true,
      composed: true,
      cancelable: true,
      button: 0,
    })
    crumb.dispatchEvent(laterClick)
    expect(laterClick.defaultPrevented).toBe(false)
    now.mockRestore()
    el.remove()
  })

  it('program detail surfaces failure feedback on 409', async () => {
    const el = document.createElement('ak-program-detail') as AkProgramDetail
    el.store = store
    el.app = 'demo'
    el.program = 'worker' // fatal state → start button offered
    document.body.appendChild(el)
    await el.updateComplete
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete

    // fatal reason sits in its own warning block below the command.
    const fatal = el.shadowRoot!.querySelector('.notice[data-tone="fatal"]')
    expect(fatal?.textContent).toContain('retries exhausted')

    const fetchMock = vi.fn().mockResolvedValue({
      ok: false,
      status: 409,
      json: async () => ({ error: 'cannot start: retries exhausted' }),
    })
    vi.stubGlobal('fetch', fetchMock)

    // fatal → the menu offers start; selected means executed (no confirm).
    const trigger = el.shadowRoot!.querySelector<HTMLButtonElement>('button.op-trigger')!
    trigger.click()
    await el.updateComplete
    const menu = el.shadowRoot!.querySelector('ak-action-menu')!
    const items = [...menu.shadowRoot!.querySelectorAll<HTMLButtonElement>('.menu-item[data-kind]')]
    expect(items.map((b) => b.dataset.kind)).toEqual(['start'])
    items[0]!.click()
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    expect(fetchMock).toHaveBeenCalledWith('/api/programs/worker/start', { method: 'POST', credentials: 'same-origin' })
    expect(el.shadowRoot!.querySelector('.notice')!.textContent).toContain('cannot start: retries exhausted')
    el.remove()
  })
})
