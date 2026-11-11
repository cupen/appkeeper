// @vitest-environment happy-dom
//
// webui-ui: 程序操作菜单 — the single floating layer behind every entry point
// (row right-click, ⋯ trigger, keyboard Menu key). These assertions cover the
// whole state machine (closed → open → confirming → busy), the keyboard path,
// the dismiss paths and the result event the surfaces consume.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import './action-menu.js'
import {
  AkActionMenu,
  actionsFor,
  type ProgramAction,
  type ProgramActionResultDetail,
} from './action-menu.js'
import type { ConsoleStore } from '../lib/store.js'
import type { ProgramInfo } from '../lib/types.js'

function program(name: string, state = 'running'): ProgramInfo {
  return {
    app: 'demo',
    name,
    state,
    pid: state === 'running' ? 42 : null,
    unhealthy: false,
    uptime_secs: 65,
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

type Deferred = { resolve: (t: string) => void; reject: (e: Error) => void }

function stubStore(): { store: ConsoleStore; action: ReturnType<typeof vi.fn>; gate: Deferred } {
  let resolve!: (t: string) => void
  let reject!: (e: Error) => void
  const gate: Deferred = {
    resolve: (t) => resolve(t),
    reject: (e) => reject(e),
  }
  const action = vi.fn().mockImplementation(
    () =>
      new Promise<string>((res, rej) => {
        resolve = res
        reject = rej
      }),
  )
  return { store: { programAction: action } as unknown as ConsoleStore, action, gate }
}

describe('actionsFor (menu items match the program state)', () => {
  it('running/starting → stop + restart, stopping → nothing, else → start', () => {
    expect(actionsFor('running')).toEqual<ProgramAction[]>(['stop', 'restart'])
    expect(actionsFor('starting')).toEqual<ProgramAction[]>(['stop', 'restart'])
    expect(actionsFor('stopping')).toEqual<ProgramAction[]>([])
    expect(actionsFor('stopped')).toEqual<ProgramAction[]>(['start'])
    expect(actionsFor('exited')).toEqual<ProgramAction[]>(['start'])
    expect(actionsFor('backoff')).toEqual<ProgramAction[]>(['start'])
    expect(actionsFor('fatal')).toEqual<ProgramAction[]>(['start'])
  })
})

describe('ak-action-menu', () => {
  let el: AkActionMenu
  let store: ConsoleStore
  let action: ReturnType<typeof vi.fn>
  let gate: Deferred
  let results: ProgramActionResultDetail[]

  beforeEach(async () => {
    localStorage.clear()
    ;({ store, action, gate } = stubStore())
    el = document.createElement('ak-action-menu') as AkActionMenu
    el.store = store
    document.body.appendChild(el)
    await el.updateComplete
    results = []
    el.addEventListener('program-action-result', (e) =>
      results.push((e as CustomEvent<ProgramActionResultDetail>).detail),
    )
  })

  afterEach(() => {
    el.remove()
    vi.restoreAllMocks()
  })

  function overlay(): HTMLElement | null {
    return el.shadowRoot!.querySelector('.overlay')
  }

  function items(): HTMLButtonElement[] {
    return [...el.shadowRoot!.querySelectorAll<HTMLButtonElement>('.menu-item[data-kind]')]
  }

  function confirmButton(): HTMLButtonElement | null {
    return el.shadowRoot!.querySelector<HTMLButtonElement>('.confirm-action')
  }

  /** Focused element inside the overlay (activeElement retargets to the host). */
  function focusedItem(): Element | null {
    return el.shadowRoot!.activeElement
  }

  function open(p: ProgramInfo, x = 20, y = 20, trigger: HTMLElement | null = null): void {
    el.openFor(p, x, y, trigger)
  }

  it('starts closed and renders nothing', () => {
    expect(el.isOpen).toBe(false)
    expect(overlay()).toBeNull()
  })

  it('opens for a running program with stop + restart only', async () => {
    open(program('web'))
    await el.updateComplete
    expect(el.isOpen).toBe(true)
    expect(overlay()?.getAttribute('role')).toBe('menu')
    expect(overlay()?.getAttribute('aria-label')).toContain('web')
    expect(items().map((b) => b.dataset.kind)).toEqual(['stop', 'restart'])
    // Items expose the action word and glyph (colour is never the only channel).
    const stop = items()[0]!
    expect(stop.textContent).toContain('停止')
    expect(stop.textContent).toContain('■')
    expect(stop.getAttribute('aria-label')).toBe('停止 web')
    expect(items()[1]!.textContent).toContain('⟳')
  })

  it('offers only start for a stopped program and runs it without a confirm step', async () => {
    open(program('idle', 'stopped'))
    await el.updateComplete
    expect(items().map((b) => b.dataset.kind)).toEqual(['start'])
    items()[0]!.click()
    await el.updateComplete
    // Straight to the request: no confirm button ever rendered.
    expect(confirmButton()).toBeNull()
    expect(action).toHaveBeenCalledWith('idle', 'start')
    gate.resolve('started')
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    // Result re-raised to the surface, then the menu closes.
    expect(results).toEqual([{ ok: true, text: 'started', program: 'idle', action: 'start' }])
    expect(el.isOpen).toBe(false)
  })

  it('asks for inline confirmation on stop: nothing requests before it', async () => {
    open(program('web'))
    await el.updateComplete
    el.shadowRoot!.querySelector<HTMLButtonElement>('.menu-item[data-kind="stop"]')!.click()
    await el.updateComplete
    expect(action).not.toHaveBeenCalled()
    const confirm = confirmButton()!
    expect(confirm.dataset.kind).toBe('stop')
    expect(confirm.textContent).toContain('停止')
    // The step names the program and the text sits in the same overlay.
    expect(overlay()!.textContent).toContain('web')
    expect(el.shadowRoot!.querySelector('.confirm-cancel')).not.toBeNull()
  })

  it('confirm submits; cancel walks back to the menu with zero side effects', async () => {
    open(program('web'))
    await el.updateComplete
    el.shadowRoot!.querySelector<HTMLButtonElement>('.menu-item[data-kind="stop"]')!.click()
    await el.updateComplete
    // Cancel first: back to the item list, nothing sent.
    el.shadowRoot!.querySelector<HTMLButtonElement>('.confirm-cancel')!.click()
    await el.updateComplete
    expect(action).not.toHaveBeenCalled()
    expect(overlay()?.getAttribute('role')).toBe('menu')
    expect(items().map((b) => b.dataset.kind)).toEqual(['stop', 'restart'])
    // Now confirm for real.
    el.shadowRoot!.querySelector<HTMLButtonElement>('.menu-item[data-kind="restart"]')!.click()
    await el.updateComplete
    confirmButton()!.click()
    await el.updateComplete
    expect(action).toHaveBeenCalledWith('web', 'restart')
    gate.resolve('restarted')
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    expect(results).toEqual([{ ok: true, text: 'restarted', program: 'web', action: 'restart' }])
    expect(el.isOpen).toBe(false)
  })

  it('disables every item while busy and surfaces failures instead of failing silently', async () => {
    open(program('web'))
    await el.updateComplete
    el.shadowRoot!.querySelector<HTMLButtonElement>('.menu-item[data-kind="stop"]')!.click()
    await el.updateComplete
    confirmButton()!.click()
    await el.updateComplete
    // busy morphs the overlay back to the item list with everything disabled.
    expect(items().every((b) => b.disabled)).toBe(true)
    gate.reject(new Error('program is stopping'))
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    expect(results).toHaveLength(1)
    expect(results[0]!.ok).toBe(false)
    expect(results[0]!.text).toContain('停止失败')
    expect(results[0]!.text).toContain('program is stopping')
    expect(el.isOpen).toBe(false)
  })

  it('Escape walks back one level at a time and finally restores trigger focus', async () => {
    const trigger = document.createElement('button')
    document.body.appendChild(trigger)
    trigger.focus()
    open(program('web'), 20, 20, trigger)
    await el.updateComplete
    // Focus lands on the first menu item.
    await new Promise((r) => setTimeout(r, 0))
    expect(focusedItem()).toBe(items()[0])

    el.shadowRoot!.querySelector<HTMLButtonElement>('.menu-item[data-kind="stop"]')!.click()
    await el.updateComplete
    // confirming → open.
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await el.updateComplete
    expect(confirmButton()).toBeNull()
    expect(items()).toHaveLength(2)
    // open → closed, focus returns to the trigger.
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await el.updateComplete
    expect(el.isOpen).toBe(false)
    expect(document.activeElement).toBe(trigger)
    trigger.remove()
  })

  it('closes on scroll, resize and pointerdown outside (never mid-navigation)', async () => {
    open(program('web'))
    await el.updateComplete
    window.dispatchEvent(new Event('scroll'))
    expect(el.isOpen).toBe(false)

    open(program('web'))
    await el.updateComplete
    window.dispatchEvent(new Event('resize'))
    expect(el.isOpen).toBe(false)

    open(program('web'))
    await el.updateComplete
    // A pointerdown inside the overlay keeps it open…
    overlay()!.dispatchEvent(
      new MouseEvent('pointerdown', { bubbles: true, composed: true }),
    )
    expect(el.isOpen).toBe(true)
    // …anything outside closes it.
    document.body.dispatchEvent(
      new MouseEvent('pointerdown', { bubbles: true, composed: true }),
    )
    expect(el.isOpen).toBe(false)
    // No request was ever sent by any dismiss path.
    expect(action).not.toHaveBeenCalled()
  })

  it('moves focus with arrow keys cyclically and triggers with Enter', async () => {
    open(program('web'))
    await el.updateComplete
    await new Promise((r) => setTimeout(r, 0))
    expect(focusedItem()).toBe(items()[0])
    items()[0]!.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, composed: true }),
    )
    expect(focusedItem()).toBe(items()[1])
    items()[1]!.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true, composed: true }),
    )
    expect(focusedItem()).toBe(items()[0])
    // Enter on a focused item activates it (stop → inline confirm).
    items()[0]!.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, composed: true }),
    )
    await el.updateComplete
    expect(confirmButton()).not.toBeNull()
    expect(action).not.toHaveBeenCalled()
  })

  it('announces open/close via menu-toggle for surface-side row markers', async () => {
    const toggles: { open: boolean; program: string }[] = []
    el.addEventListener('menu-toggle', (e) =>
      toggles.push((e as CustomEvent<{ open: boolean; program: string }>).detail),
    )
    open(program('web'))
    el.close()
    expect(toggles).toEqual([
      { open: true, program: 'web' },
      { open: false, program: 'web' },
    ])
  })

  it('wasRecentlyClosed spans only the dismiss-click window (click-through guard)', () => {
    const now = vi.spyOn(performance, 'now')
    open(program('web'))
    expect(el.wasRecentlyClosed()).toBe(false) // never closed yet
    now.mockReturnValueOnce(5000).mockReturnValue(5050)
    el.close()
    // 50ms after close: still inside the window.
    expect(el.wasRecentlyClosed(150)).toBe(true)
    // 200ms after close: the window has passed.
    now.mockReturnValue(5200)
    expect(el.wasRecentlyClosed(150)).toBe(false)
    now.mockRestore()
  })
})
