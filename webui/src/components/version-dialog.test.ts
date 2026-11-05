// @vitest-environment happy-dom
/**
 * About dialog audit (show-build-version D4): complete metadata, the
 * `unknown` degradation, the offline placeholder, and every close path.
 * Nothing here depends on `showModal()` — the component falls back to the
 * plain `open` attribute, which is what these assertions read.
 */

import { afterEach, describe, expect, it } from 'vitest'
import { AkVersionDialog, formatBuiltAt } from './version-dialog.js'
import type { DaemonInfo } from '../lib/types.js'

function daemon(overrides: Partial<DaemonInfo> = {}): DaemonInfo {
  return {
    version: '0.1.0',
    build: {
      git_commit: '02d37e02dedc-dirty',
      built_at: '2026-01-01T00:00:00Z',
      target: 'x86_64-unknown-linux-gnu',
      rustc: 'rustc 1.99.1 (58cf8f9c3 2026-10-13)',
    },
    socket: '/run/appkeeper/1000/ak.sock',
    port: 0,
    apps: 2,
    system: { cpu_percent: 5, mem_used_bytes: 1, mem_total_bytes: 2 },
    monitor_interval: 1,
    uptime_secs: 90,
    config_source: '/etc/appkeeper/appkeeper.toml',
    ...overrides,
  }
}

function mount(input: DaemonInfo | null, programCount = 0): AkVersionDialog {
  const el = document.createElement('ak-version-dialog') as AkVersionDialog
  el.daemon = input
  el.programCount = programCount
  document.body.appendChild(el)
  return el
}

function dialogOf(el: AkVersionDialog): HTMLDialogElement {
  return el.shadowRoot!.querySelector('dialog')!
}

function textOf(el: AkVersionDialog): string {
  return el.shadowRoot!.textContent!.replace(/\s+/g, ' ')
}

const pad = (n: number): string => String(n).padStart(2, '0')

afterEach(() => {
  document.body.innerHTML = ''
})

describe('ak-version-dialog', () => {
  it('opens as a dialog and lists build and runtime facts', async () => {
    const el = mount(daemon(), 3)
    await el.updateComplete

    expect(dialogOf(el).hasAttribute('open')).toBe(true)
    expect(textOf(el)).toContain('关于 appkeeper')
    // Opening moves focus to the close button (webui-ui: 键盘与关闭).
    expect(el.shadowRoot!.activeElement).toBe(el.shadowRoot!.querySelector('.close'))

    // Build section: version, commit, time (raw UTC in the tooltip), target, rustc.
    expect(textOf(el)).toContain('0.1.0')
    expect(textOf(el)).toContain('02d37e02dedc-dirty')
    expect(textOf(el)).toContain('x86_64-unknown-linux-gnu')
    expect(textOf(el)).toContain('rustc 1.99.1 (58cf8f9c3 2026-10-13)')
    expect(el.shadowRoot!.querySelector('dd[title="2026-01-01T00:00:00Z"]')).toBeTruthy()

    // Runtime section wired to the projection.
    expect(textOf(el)).toContain('/run/appkeeper/1000/ak.sock')
    expect(textOf(el)).toContain('/etc/appkeeper/appkeeper.toml')
    expect(textOf(el)).toContain('1m 30s')
    expect(textOf(el)).toContain('2 / 3')
    el.remove()
  })

  it('renders build time in the local timezone', async () => {
    const el = mount(daemon({ build: { git_commit: 'x', built_at: '2026-01-01T00:00:00Z', target: 't', rustc: 'r' } }))
    await el.updateComplete
    const d = new Date('2026-01-01T00:00:00Z')
    const local =
      `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
      `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
    expect(textOf(el)).toContain(local)
    el.remove()
  })

  it('shows `unknown` fields verbatim instead of hiding them', async () => {
    const el = mount(
      daemon({
        build: {
          git_commit: 'unknown',
          built_at: 'unknown',
          target: 'unknown',
          rustc: 'unknown',
        },
      }),
    )
    await el.updateComplete
    const values = [...el.shadowRoot!.querySelectorAll('dd')].map((dd) => dd.textContent!.trim())
    expect(values.filter((v) => v === 'unknown')).toHaveLength(4)
    el.remove()
  })

  it('renders only the offline notice when no projection exists', async () => {
    const el = mount(null)
    await el.updateComplete
    expect(dialogOf(el).hasAttribute('open')).toBe(true)
    expect(textOf(el)).toContain('当前未连接守护进程')
    // No fabricated build/runtime tables.
    expect(el.shadowRoot!.querySelectorAll('section')).toHaveLength(0)
    expect(el.shadowRoot!.querySelectorAll('dd')).toHaveLength(0)
    el.remove()
  })

  it('closes from the close button, emits ak-version-closed and drops `open`', async () => {
    const el = mount(daemon())
    await el.updateComplete
    let closed = 0
    el.addEventListener('ak-version-closed', () => closed++)

    el.shadowRoot!.querySelector<HTMLButtonElement>('.close')!.click()
    await el.updateComplete

    expect(closed).toBe(1)
    expect(dialogOf(el).hasAttribute('open')).toBe(false)
    el.remove()
  })

  it('closes on Escape', async () => {
    const el = mount(daemon())
    await el.updateComplete
    let closed = 0
    el.addEventListener('ak-version-closed', () => closed++)

    dialogOf(el).dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await el.updateComplete

    expect(closed).toBe(1)
    expect(dialogOf(el).hasAttribute('open')).toBe(false)
    el.remove()
  })
})

describe('formatBuiltAt', () => {
  it('passes `unknown` and unparseable values through unchanged', () => {
    expect(formatBuiltAt('unknown')).toBe('unknown')
    expect(formatBuiltAt('not-a-date')).toBe('not-a-date')
    expect(formatBuiltAt('')).toBe('unknown')
  })

  it('converts RFC 3339 UTC to the same instant in local time', () => {
    const out = formatBuiltAt('2026-01-01T00:00:00Z')
    const d = new Date('2026-01-01T00:00:00Z')
    expect(out).toBe(
      `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
        `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`,
    )
  })
})
