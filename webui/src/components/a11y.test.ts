// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import './status-badge.js'
import type { AkStatusBadge } from './status-badge.js'
import './action-menu.js'
import type { AkActionMenu } from './action-menu.js'
import type { ProgramInfo } from '../lib/types.js'

function program(name: string, state = 'running'): ProgramInfo {
  return {
    app: 'demo',
    name,
    state,
    pid: null,
    unhealthy: false,
    uptime_secs: 0,
    total_exits: 0,
    restart_backoff: 0,
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

describe('accessibility baseline', () => {
  it('status badge text remains available in greyscale (label + shape nodes)', async () => {
    const el = document.createElement('ak-status-badge') as AkStatusBadge
    el.slug = 'fatal'
    el.label = '故障'
    el.glyph = '✕'
    document.body.appendChild(el)
    await el.updateComplete
    const label = el.shadowRoot?.querySelector('.label')
    const shape = el.shadowRoot?.querySelector('.glyph svg')
    // Word and shape nodes both exist: greyscale cannot erase the state.
    expect(label?.textContent).toBe('故障')
    expect(shape).toBeTruthy()
    el.remove()
  })

  it('action menu exposes menu semantics and action words (webui-ui: 可访问性)', async () => {
    const el = document.createElement('ak-action-menu') as AkActionMenu
    document.body.appendChild(el)
    await el.updateComplete
    el.openFor(program('web'), 10, 10)
    await el.updateComplete

    const overlay = el.shadowRoot!.querySelector('[role="menu"]')!
    expect(overlay).not.toBeNull()
    expect(overlay.getAttribute('aria-label')).toContain('web')
    const items = [...el.shadowRoot!.querySelectorAll('[role="menuitem"]')]
    expect(items.map((b) => b.getAttribute('data-kind'))).toEqual(['stop', 'restart'])
    for (const item of items) {
      // Accessible name carries the action word + program name, and the
      // glyph adds a shape channel (greyscale scenario, visual-system).
      expect(item.getAttribute('aria-label')).toMatch(/^(停止|重启) web$/)
      expect(item.querySelector('.icon')?.textContent).not.toBe('')
    }
    // Every action colour comes from the status token families only.
    const css = (el.constructor as typeof AkActionMenu).styles.cssText as string
    expect(css).toContain('--ak-status-done')
    expect(css).toContain('--ak-status-working')
    expect(css).toContain('--ak-status-failed')
    el.remove()
  })

  it('reduced motion + theme class hooks ship in tokens.css, focus ring in app.css', async () => {
    const fs = await import('node:fs')
    const path = await import('node:path')
    const { fileURLToPath } = await import('node:url')
    const here = path.dirname(fileURLToPath(import.meta.url))
    const tokens = fs.readFileSync(path.join(here, '../../src/styles/tokens.css'), 'utf8')
    expect(tokens).toContain('prefers-reduced-motion')
    // Light re-map is keyed on the single wa-dark switch (see src/theme.ts),
    // so an explicit appearance override can beat the OS preference.
    expect(tokens).toContain(':root:not(.wa-dark)')
    const app = fs.readFileSync(path.join(here, '../../src/styles/app.css'), 'utf8')
    expect(app).toContain(':focus-visible')
  })
})
