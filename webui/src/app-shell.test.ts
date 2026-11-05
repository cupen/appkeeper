// @vitest-environment happy-dom
/**
 * Shell audit: the console frame — brand sidebar + console tree + routed
 * outlet. Routes: `/` (dashboard), `/app/:app` (app overview), and
 * `/app/:app/program/:program` (detail). Unknown paths fall back to the
 * dashboard.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { matchRoute } from './router.js'
import { AkApp, ROUTES } from './app-shell.js'
import { consoleStore } from './lib/store.js'
import { MSG, type ProgramInfo, type StatusDoc } from './lib/types.js'
import { encode } from '@msgpack/msgpack'

describe('shell route surface', () => {
  it('ships the console routes (dashboard, app, program)', () => {
    expect(ROUTES).toEqual([
      { id: 'dashboard', pattern: '/' },
      { id: 'app', pattern: '/app/:app' },
      { id: 'program', pattern: '/app/:app/program/:program' },
    ])
  })

  it('resolves `/` and falls back for anything else', () => {
    expect(matchRoute(ROUTES, '/')?.id).toBe('dashboard')
    expect(matchRoute(ROUTES, '/sessions')).toBeNull()
  })

  it('resolves app and program deep links with encoded params', () => {
    const app = matchRoute(ROUTES, '/app/demo-app')
    expect(app?.id).toBe('app')
    expect(app?.params['app']).toBe('demo-app')
    const prog = matchRoute(ROUTES, '/app/my%20app/program/worker%201')
    expect(prog?.id).toBe('program')
    expect(prog?.params['app']).toBe('my%20app')
    expect(prog?.params['program']).toBe('worker%201')
  })
})

// -- guard-remove-until-stopped 4.3: sidebar/overview convergence -----------

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

function program(app: string, name: string): ProgramInfo {
  return {
    app,
    name,
    state: 'stopped',
    pid: null,
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

function snapshotFrame(doc: StatusDoc): Uint8Array {
  const payload = encode(doc)
  const out = new Uint8Array(payload.length + 1)
  out[0] = MSG.SNAPSHOT
  out.set(payload, 1)
  return out
}

function statusFrame(body: unknown): Uint8Array {
  const text = new TextEncoder().encode(JSON.stringify(body))
  const out = new Uint8Array(text.length + 1)
  out[0] = MSG.STATUS
  out.set(text, 1)
  return out
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
      system: { cpu_percent: 5, mem_used_bytes: 1, mem_total_bytes: 2 },
      monitor_interval: 1,
      uptime_secs: 10,
      config_source: '/t',
    },
    pending: { programs: [], apps_added: [], apps_removed: [], daemon_hints: [], errors: [] },
    programs,
  }
}

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0))

// -- show-build-version 3.1/3.3: version slot + about dialog ----------------

describe('brand version slot and about dialog (show-build-version 3.1/3.3)', () => {
  beforeEach(() => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket)
    // Fully offline: no REST fallback may inject a projection behind the
    // assertions (the shared store keeps the last doc across tests).
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')))
    FakeWebSocket.instances = []
    history.replaceState({}, '', '/')
  })

  afterEach(() => {
    consoleStore.stop()
    document.body.innerHTML = ''
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  async function mount(): Promise<AkApp> {
    const el = document.createElement('ak-app') as AkApp
    document.body.appendChild(el)
    await el.updateComplete
    return el
  }

  function versionButton(el: AkApp): HTMLButtonElement {
    return el.shadowRoot!.querySelector<HTMLButtonElement>('.version')!
  }

  function dialog(el: AkApp): HTMLElement | null {
    return el.shadowRoot!.querySelector('ak-version-dialog')
  }

  it('keeps the brand mark and shows a version placeholder with no projection', async () => {
    const shell = await mount()
    const mark = shell.shadowRoot!.querySelector('.brand .mark')
    expect(mark?.textContent?.trim()).toBe('ak')
    expect(shell.shadowRoot!.querySelector('.brand .name')?.textContent?.trim()).toBe('appkeeper')
    // The version button sits outside the anchor: a click cannot navigate.
    expect(shell.shadowRoot!.querySelector('.brand .version')).toBeNull()
    const button = versionButton(shell)
    expect(button.textContent?.trim()).toBe('v—')
    expect(button.getAttribute('aria-haspopup')).toBe('dialog')
    expect(button.getAttribute('title')).toContain('双击查看版本详情')
    expect(button.getAttribute('aria-label')).toContain('双击查看版本详情')
    button.click()
    expect(location.pathname).toBe('/')
    shell.remove()
  })

  it('opens the dialog on double-click (offline: only the notice) and closes on Esc', async () => {
    const shell = await mount()
    const button = versionButton(shell)
    expect(dialog(shell)).toBeNull()

    button.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, composed: true }))
    await tick()
    await shell.updateComplete

    const host = dialog(shell) as unknown as { shadowRoot: ShadowRoot }
    expect(host).not.toBeNull()
    expect(host.shadowRoot.textContent).toContain('当前未连接守护进程')

    host.shadowRoot
      .querySelector('dialog')!
      .dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await tick()
    await shell.updateComplete
    await tick()
    await shell.updateComplete

    expect(dialog(shell)).toBeNull()
    expect(shell.shadowRoot!.activeElement).toBe(versionButton(shell))
    shell.remove()
  })

  it('shows the projected version and build facts once a snapshot arrives', async () => {
    consoleStore.start()
    const ws = FakeWebSocket.instances[0]!
    ws.serverOpen()
    ws.serverFrame(snapshotFrame(doc([program('alpha', 'web')])))
    await tick()

    const shell = await mount()
    const button = versionButton(shell)
    expect(button.textContent?.trim()).toBe('v0.1.0')

    button.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, composed: true }))
    await tick()
    await shell.updateComplete

    const text = dialog(shell)!.shadowRoot!.textContent!.replace(/\s+/g, ' ')
    expect(text).toContain('git 提交')
    expect(text).toContain('02d37e02dedc')
    expect(text).toContain('/run/appkeeper/1000/ak.sock')
    shell.remove()
  })

  it('opens the dialog with Enter and Space without double-firing', async () => {
    const shell = await mount()
    const button = versionButton(shell)

    button.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
    await tick()
    await shell.updateComplete
    expect(dialog(shell)).not.toBeNull()

    // Close again, then Space.
    const host = dialog(shell) as unknown as { shadowRoot: ShadowRoot }
    host.shadowRoot.querySelector<HTMLButtonElement>('.close')!.click()
    await tick()
    await shell.updateComplete
    await tick()
    await shell.updateComplete
    expect(dialog(shell)).toBeNull()

    versionButton(shell).dispatchEvent(new KeyboardEvent('keydown', { key: ' ', bubbles: true }))
    await tick()
    await shell.updateComplete
    expect(dialog(shell)).not.toBeNull()
    shell.remove()
  })

  it('hides the version slot on narrow screens like the old brand caption', () => {
    const cssText = (AkApp.styles as unknown as { cssText: string }).cssText
    expect(cssText).toContain('.brand-block')
    expect(cssText).toMatch(/@media \(max-width: 720px\)[\s\S]*\.version\s*\{\s*display: none/)
    // Alignment to the app name: mark width + brand gap.
    expect(cssText).toContain('margin-left: calc(var(--ak-space-2) + 28px + var(--ak-space-3))')
  })
})

describe('shell convergence after a removal (guard-remove-until-stopped 4.3)', () => {
  let ws: FakeWebSocket

  beforeEach(async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => doc([]) }))
    FakeWebSocket.instances = []
    history.replaceState({}, '', '/app/alpha')
    // `ak-app` binds the shared singleton store.
    consoleStore.start()
    ws = FakeWebSocket.instances[0]!
    ws.serverOpen()
    ws.serverFrame(snapshotFrame(doc([program('alpha', 'web'), program('beta', 'job')])))
    await tick()
  })

  afterEach(() => {
    consoleStore.stop()
    document.body.innerHTML = ''
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  async function mountShell(): Promise<AkApp> {
    const el = document.createElement('ak-app') as AkApp
    document.body.appendChild(el)
    await el.updateComplete
    return el
  }

  function sidebarApps(el: AkApp): string[] {
    const tree = el.shadowRoot!.querySelector('ak-console-tree') as unknown as {
      shadowRoot: ShadowRoot
    }
    return [...tree.shadowRoot.querySelectorAll('.app-link')].map((a) =>
      [...a.querySelectorAll('span')]
        .filter((s) => !s.classList.contains('count') && s.getAttribute('aria-hidden') !== 'true')
        .map((s) => s.textContent!.trim())
        .join(''),
    )
  }

  it('drops the removed app from the sidebar and leaves the dead page', async () => {
    const shell = await mountShell()
    expect(sidebarApps(shell)).toEqual(['alpha', 'beta'])

    // The removal lands as a STATUS delta (another client removed alpha).
    ws.serverFrame(
      statusFrame({ programs: [], removed: ['web'], apps_gone: ['alpha'] }),
    )
    await tick()
    await shell.updateComplete
    await shell.updateComplete
    await tick()
    await shell.updateComplete

    expect(sidebarApps(shell)).toEqual(['beta'])
    expect(location.pathname).toBe('/app/beta')
    expect(shell.shadowRoot!.querySelector('ak-app-overview')).not.toBeNull()
    expect(
      (shell.shadowRoot!.querySelector('ak-app-overview') as unknown as { app: string }).app,
    ).toBe('beta')
    shell.remove()
  })

  it('keeps an app that is registered but declares no programs', async () => {
    history.replaceState({}, '', '/app/empty')
    ws.serverFrame(snapshotFrame(doc([program('empty', 'placeholder')])))
    await tick()
    const shell = await mountShell()
    // No removal signal: an app whose programs were deleted by an apply keeps
    // its record and its (empty) overview.
    ws.serverFrame(statusFrame({ programs: [], removed: ['placeholder'] }))
    await tick()
    await shell.updateComplete
    expect(consoleStore.wasAppRemoved('empty')).toBe(false)
    expect(location.pathname).toBe('/app/empty')
    shell.remove()
  })

  it('falls back to the dashboard when no app is left', async () => {
    const shell = await mountShell()
    ws.serverFrame(
      statusFrame({ programs: [], removed: ['web', 'job'], apps_gone: ['alpha', 'beta'] }),
    )
    await tick()
    await shell.updateComplete
    await shell.updateComplete

    expect(sidebarApps(shell)).toEqual([])
    expect(location.pathname).toBe('/')
    expect(shell.shadowRoot!.querySelector('ak-dashboard')).not.toBeNull()
    shell.remove()
  })
})

