// @vitest-environment happy-dom
/**
 * Tree geometry and status presentation (webui-ui: 左侧两层导航,
 * webui-visual-system: 长文本与溢出 / 状态的颜色与形状分工). happy-dom does
 * not lay out, so the invariants that keep long and short names on the same
 * row height are asserted structurally: a fixed row min-height plus a
 * single-line ellipsis on the name, and status hue confined to the dot.
 */

import { describe, expect, it } from 'vitest'
import { AkConsoleTree, type TreeApp } from './console-tree.js'
import type { ProgramInfo } from '../lib/types.js'

function program(app: string, name: string, state: string): ProgramInfo {
  return {
    app,
    name,
    state,
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

const LONG = 'a-very-long-program-name-that-would-wrap-without-an-ellipsis'

function apps(): TreeApp[] {
  return [
    {
      name: 'demo',
      programs: [
        program('demo', 'web', 'running'),
        program('demo', LONG, 'backoff'),
        program('demo', 'shadow', 'fatal'),
        program('demo', 'never-started', 'stopped'),
        program('demo', 'mystery', 'unheard-of'),
      ],
    },
  ]
}

async function mount(selectedProgram = 'web'): Promise<AkConsoleTree> {
  const el = document.createElement('ak-console-tree') as AkConsoleTree
  el.apps = apps()
  el.selectedApp = 'demo'
  el.selectedProgram = selectedProgram
  document.body.appendChild(el)
  await el.updateComplete
  return el
}

function cssText(): string {
  return (AkConsoleTree.styles as unknown as { cssText: string }).cssText
}

describe('ak-console-tree', () => {
  it('lists every program and keeps the stable data hooks', async () => {
    const el = await mount()
    const links = [...el.shadowRoot!.querySelectorAll('a.prog')]
    expect(links).toHaveLength(5)
    expect(links.map((a) => a.getAttribute('data-state'))).toEqual([
      'running',
      'backoff',
      'fatal',
      'stopped',
      'unheard-of',
    ])
    expect(links[0]!.getAttribute('href')).toBe('/app/demo/program/web')
    expect(links[0]!.getAttribute('data-program')).toBe('web')
    expect(el.shadowRoot!.textContent).toContain('never-started')
    el.remove()
  })

  it('puts the Chinese state word in title and aria-label', async () => {
    const el = await mount()
    const links = [...el.shadowRoot!.querySelectorAll('a.prog')]
    expect(links[0]!.getAttribute('title')).toBe('web — 运行中')
    expect(links[0]!.getAttribute('aria-label')).toBe('web — 运行中')
    expect(links[1]!.getAttribute('aria-label')).toBe(`${LONG} — 退避中`)
    expect(links[2]!.getAttribute('aria-label')).toBe('shadow — 故障')
    // Unknown slugs read back verbatim, never an empty label.
    expect(links[4]!.getAttribute('aria-label')).toBe('mystery — unheard-of')
    el.remove()
  })

  it('keeps the status hue on the dot, not on the name text', async () => {
    const el = await mount()
    const styles = cssText()
    // Every status selector targets the dot…
    for (const state of ['running', 'starting', 'stopping', 'backoff', 'fatal']) {
      expect(styles).toContain(`a.prog[data-state='${state}'] .dot`)
    }
    // …and no status block colours the link itself.
    expect(styles).not.toMatch(
      /a\.prog\[data-state='[a-z]+'\]\s*\{[^}]*color:/,
    )
    const names = [...el.shadowRoot!.querySelectorAll('a.prog .name')]
    expect(names).toHaveLength(5)
    for (const name of names) {
      // Neutral text: no inline status colour anywhere on the name node.
      expect(name.getAttribute('style')).toBeNull()
    }
    // Each row carries exactly one leading dot, hidden from a11y.
    const dots = [...el.shadowRoot!.querySelectorAll('a.prog .dot')]
    expect(dots).toHaveLength(5)
    for (const dot of dots) expect(dot.getAttribute('aria-hidden')).toBe('true')
    el.remove()
  })

  it('keeps long and short names on one fixed row height with ellipsis', async () => {
    const el = await mount()
    const styles = cssText()
    // One control-height row for every program regardless of name length.
    expect(styles).toMatch(/a\.prog\s*\{[^}]*min-height:\s*var\(--ak-control-h/)
    // Names are single line and truncate; the row cannot grow.
    expect(styles).toMatch(/a\.prog \.name\s*\{[^}]*white-space:\s*nowrap/)
    expect(styles).toMatch(/a\.prog \.name\s*\{[^}]*text-overflow:\s*ellipsis/)
    expect(styles).toMatch(/a\.prog \.name\s*\{[^}]*min-width:\s*0/)
    // The app row's program count keeps its fixed trailing slot.
    expect(styles).toMatch(/\.app-link \.count\s*\{[^}]*flex:\s*0 0 auto/)
    expect(styles).toMatch(/\.app-link \.count\s*\{[^}]*margin-left:\s*auto/)
    const appLink = el.shadowRoot!.querySelector('.app-link')
    expect(appLink?.querySelector('.count')?.textContent?.trim()).toBe('5')
    el.remove()
  })

  it('highlights the selected program without a second colour language', async () => {
    const el = await mount('web')
    const active = el.shadowRoot!.querySelector('a.prog[data-active="true"]')
    expect(active?.getAttribute('data-program')).toBe('web')
    el.remove()
  })
})
