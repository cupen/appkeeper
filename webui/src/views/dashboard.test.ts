// @vitest-environment happy-dom
//
// webui-ui: 全局程序总览 — the dashboard is now a grouped global program
// table (problem programs first), not a stack of app cards. Banner and empty
// states stay; row clicks still land on the program detail.

import { afterEach, describe, expect, it, vi } from 'vitest'
import './dashboard.js'
import type { AkDashboard } from './dashboard.js'
import type { AkProgramTable } from '../components/program-table.js'
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
  serverClose(): void {
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

function snapshotFrame(doc: StatusDoc): Uint8Array {
  const payload = encode(doc)
  const out = new Uint8Array(payload.length + 1)
  out[0] = MSG.SNAPSHOT
  out.set(payload, 1)
  return out
}

function program(
  app: string,
  name: string,
  state: string,
  fatalReason: string | null = null,
): ProgramInfo {
  return {
    app,
    name,
    state,
    pid: state === 'running' ? 123 : null,
    unhealthy: false,
    uptime_secs: 1,
    total_exits: 0,
    restart_backoff: 1,
    last_exit: null,
    fatal_reason: fatalReason,
    wait_reason: null,
    cpu_percent: null,
    mem_bytes: null,
    log_rate: { out: { w1: 0, w10: 0, w60: 0, w300: 0 }, err: { w1: 0, w10: 0, w60: 0, w300: 0 } },
    command: 'x',
    args: [],
    work_dir: '',
  }
}

function doc(programs: ProgramInfo[]): StatusDoc {
  return {
    daemon: {
      version: '0.1.0',
      build: {
        git_commit: '02d37e02dedc',
        built_at: '2026-01-01T00:00:00Z',
        target: 'x86_64-unknown-linux-gnu',
        rustc: 'rustc 1.90.0 (0000000 2026-01-01)',
      },
      socket: '/run/appkeeper/1000/ak.sock',
      port: 9877,
      apps: 2,
      system: { cpu_percent: 3, mem_used_bytes: 1, mem_total_bytes: 2 },
      monitor_interval: 1,
      uptime_secs: 5,
      config_source: '/t',
    },
    pending: { programs: [], apps_added: [], apps_removed: [], daemon_hints: [], errors: [] },
    programs,
  }
}

function healthyDoc(): StatusDoc {
  return doc([
    program('alpha', 'web', 'running'),
    program('demo', 'healthy', 'running'),
    program('demo', 'waiting', 'backoff'),
    program('demo', 'sick', 'fatal', 'start failed 5 times'),
  ])
}

describe('dashboard (global program table)', () => {
  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
    history.replaceState({}, '', '/')
  })

  function freshStore(payload: StatusDoc = healthyDoc()): { store: ConsoleStore; ws: FakeWebSocket } {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => payload }))
    FakeWebSocket.instances = []
    history.replaceState({}, '', '/')
    const store = new ConsoleStore()
    store.start()
    const ws = FakeWebSocket.instances[0]!
    ws.serverOpen()
    return { store, ws }
  }

  async function mount(store: ConsoleStore): Promise<AkDashboard> {
    const el = document.createElement('ak-dashboard') as AkDashboard
    el.store = store
    document.body.appendChild(el)
    await el.updateComplete
    return el
  }

  async function withSnapshot(
    payload: StatusDoc = healthyDoc(),
  ): Promise<{ store: ConsoleStore; ws: FakeWebSocket; el: AkDashboard }> {
    const { store, ws } = freshStore(payload)
    const el = await mount(store)
    ws.serverFrame(snapshotFrame(payload))
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    return { store, ws, el }
  }

  function tables(el: AkDashboard): AkProgramTable[] {
    return [...el.shadowRoot!.querySelectorAll('ak-program-table')] as AkProgramTable[]
  }

  function rows(table: AkProgramTable): HTMLElement[] {
    return [
      ...table.shadowRoot!.querySelectorAll<HTMLElement>('tbody tr[data-program]'),
    ]
  }

  function statusWords(table: AkProgramTable): string[] {
    return [...table.shadowRoot!.querySelectorAll('td.status ak-status-badge')].map(
      (b) => b.shadowRoot!.querySelector('.label')!.textContent ?? '',
    )
  }

  it('groups every program by app, with problem apps and rows first', async () => {
    const { store, el } = await withSnapshot()
    const groups = tables(el)
    expect(groups).toHaveLength(2)
    // demo holds the fatal program → it leads; alpha is healthy.
    expect(groups[0]!.heading).toBe('demo')
    expect(groups[0]!.caption).toBe('3 个程序')
    expect(groups[1]!.heading).toBe('alpha')
    expect(groups[1]!.caption).toBe('1 个程序')

    // Inside the group: fatal, then backoff, then running.
    expect(rows(groups[0]!).map((r) => r.getAttribute('data-program'))).toEqual([
      'sick',
      'waiting',
      'healthy',
    ])
    expect(statusWords(groups[0]!)).toEqual(['故障', '退避中', '运行中'])
    expect(groups[0]!.shadowRoot!.textContent).toContain('start failed 5 times')
    store.stop()
    el.remove()
  })

  it('has no app-card list and keeps the rate switch in the table title bar', async () => {
    const { store, el } = await withSnapshot()
    expect(el.shadowRoot!.querySelector('section.app')).toBeNull()
    expect(el.shadowRoot!.querySelector('li[data-state]')).toBeNull()
    const first = tables(el)[0]!
    expect(first.shadowRoot!.querySelector('.table-head .rate-select')).not.toBeNull()
    store.stop()
    el.remove()
  })

  it('navigates to the program detail when a row is clicked', async () => {
    const { store, el } = await withSnapshot()
    const first = tables(el)[0]!
    rows(first)[0]!.querySelector<HTMLElement>('td.status')!.click()
    await new Promise((r) => setTimeout(r, 0))
    expect(location.pathname).toBe('/app/demo/program/sick')
    store.stop()
    el.remove()
  })

  it('shows the empty state when no app is registered', async () => {
    const { store, el } = await withSnapshot(doc([]))
    expect(tables(el)).toHaveLength(0)
    expect(el.shadowRoot!.querySelector('.empty')!.textContent).toContain('还没有应用')
    store.stop()
    el.remove()
  })

  it('keeps the poll banner and the data when the socket dies', async () => {
    const { store, ws, el } = await withSnapshot()
    ws.serverClose()
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete

    expect(el.shadowRoot!.querySelector('.banner[data-mode="poll"]')).not.toBeNull()
    expect(rows(tables(el)[0]!)).toHaveLength(3)
    store.stop()
    el.remove()
  })
})
