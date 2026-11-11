// @vitest-environment happy-dom
//
// webui-ui: 全局程序总览 / 列表行快捷操作 — the shared program table owns
// column order, placeholders, status badges, the rate-window switch and the
// row-tail actions. Both routed views consume it, so these assertions cover
// the contract they rely on.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import './program-table.js'
import type { AkProgramTable, ProgramSelectDetail } from './program-table.js'
import type { AkActionMenu } from './action-menu.js'
import type { ConsoleStore } from '../lib/store.js'
import type { ProgramInfo } from '../lib/types.js'

function program(name: string, overrides: Partial<ProgramInfo> = {}): ProgramInfo {
  return {
    app: 'demo',
    name,
    state: 'running',
    pid: 42,
    unhealthy: false,
    uptime_secs: 65,
    total_exits: 0,
    restart_backoff: 1,
    last_exit: null,
    fatal_reason: null,
    wait_reason: null,
    cpu_percent: 12.34,
    mem_bytes: 1024 * 1024,
    log_rate: {
      out: { w1: 1, w10: 2, w60: 3, w300: 4 },
      err: { w1: 0, w10: 0, w60: 0, w300: 0 },
    },
    command: 'sleep 300',
    args: [],
    work_dir: '',
    ...overrides,
  }
}

function stubStore(): { store: ConsoleStore; action: ReturnType<typeof vi.fn> } {
  const action = vi.fn().mockResolvedValue('ok')
  return { store: { programAction: action } as unknown as ConsoleStore, action }
}

/** Controllable store for the busy/confirm paths. */
function gatedStore(): {
  store: ConsoleStore
  action: ReturnType<typeof vi.fn>
  gate: { resolve: (t: string) => void; reject: (e: Error) => void }
} {
  let resolve!: (t: string) => void
  let reject!: (e: Error) => void
  const gate = {
    resolve: (t: string) => resolve(t),
    reject: (e: Error) => reject(e),
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

async function openMenuByRowClick(
  el: AkProgramTable,
  row = 0,
): Promise<{ prevented: boolean }> {
  const tr = el.shadowRoot!.querySelectorAll('tbody tr[data-program]')[row] as HTMLElement
  const prevented = !tr.dispatchEvent(
    new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 24, clientY: 24 }),
  )
  await el.updateComplete
  return { prevented }
}

function menuEl(el: AkProgramTable): AkActionMenu {
  return el.shadowRoot!.querySelector('ak-action-menu')!
}

function menuItems(el: AkProgramTable): HTMLButtonElement[] {
  return [...menuEl(el).shadowRoot!.querySelectorAll<HTMLButtonElement>('.menu-item[data-kind]')]
}

function confirmButton(el: AkProgramTable): HTMLButtonElement | null {
  return menuEl(el).shadowRoot!.querySelector<HTMLButtonElement>('.confirm-action')
}

describe('program-table', () => {
  beforeEach(() => {
    localStorage.clear()
  })
  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  async function mount(props: Partial<AkProgramTable> = {}): Promise<AkProgramTable> {
    const el = document.createElement('ak-program-table') as AkProgramTable
    el.programs = [program('web')]
    Object.assign(el, props)
    document.body.appendChild(el)
    await el.updateComplete
    return el
  }

  function headers(el: AkProgramTable): string[] {
    return [...el.shadowRoot!.querySelectorAll('thead th')].map((th) =>
      (th.textContent ?? '').trim(),
    )
  }

  function cells(el: AkProgramTable, index = 0): HTMLElement[] {
    return [...el.shadowRoot!.querySelectorAll(`tbody tr[data-program]`)[index]!.querySelectorAll('td')]
  }

  it('renders the documented column order, app column only when asked', async () => {
    const plain = await mount()
    expect(headers(plain)).toEqual([
      '程序',
      '状态',
      '重启',
      'uptime',
      'CPU',
      '内存',
      'stdout/s',
      'stderr/s',
      '',
    ])
    plain.remove()

    const withApp = await mount({ showApp: true })
    expect(headers(withApp)).toEqual([
      '程序',
      '应用',
      '状态',
      '重启',
      'uptime',
      'CPU',
      '内存',
      'stdout/s',
      'stderr/s',
      '',
    ])
    withApp.remove()
  })

  it('shows placeholders for absent metrics and keeps the restart count real', async () => {
    const el = await mount({
      programs: [
        program('idle', {
          state: 'stopped',
          pid: null,
          uptime_secs: 0,
          total_exits: 3,
          cpu_percent: null,
          mem_bytes: null,
          log_rate: {
            out: { w1: 0, w10: 0, w60: 0, w300: 0 },
            err: { w1: 0, w10: 0, w60: 0, w300: 0 },
          },
        }),
      ],
    })
    const tds = cells(el)
    expect(tds[2]!.textContent?.trim()).toBe('3')
    // uptime, CPU, memory, stdout, stderr — never fake zeros.
    expect(tds.slice(3, 8).map((td) => td.textContent?.trim())).toEqual(['—', '—', '—', '—', '—'])
    el.remove()
  })

  it('shows the backoff delay as secondary text beside the badge', async () => {
    const el = await mount({
      programs: [
        program('waiting', { state: 'backoff', pid: null, restart_backoff: 5 }),
        program('unknown-delay', { state: 'backoff', pid: null, restart_backoff: 0 }),
        program('web'),
      ],
    })
    const rows = [...el.shadowRoot!.querySelectorAll('tbody tr[data-program]')]
    // The countdown lives beside the pill, not inside it, and uses the
    // existing secondary note style (webui-ui: backoff 状态可见).
    const note = rows[0]!.querySelector('.status .note')
    expect(note?.textContent).toBe('5s 后重启')
    expect(rows[0]!.querySelector('ak-status-badge')).not.toBeNull()
    // Zero/unknown delays and non-backoff rows render no note.
    expect(rows[1]!.querySelector('.status .note')).toBeNull()
    expect(rows[2]!.querySelector('.status .note')).toBeNull()
    el.remove()
  })

  it('exposes row hooks and highlights fatal rows', async () => {
    const el = await mount({
      programs: [program('sick', { state: 'fatal', fatal_reason: 'start failed 5 times' })],
    })
    const row = el.shadowRoot!.querySelector('tbody tr[data-program]')!
    expect(row.getAttribute('data-app')).toBe('demo')
    expect(row.getAttribute('data-program')).toBe('sick')
    expect(row.getAttribute('data-status')).toBe('fatal')
    expect(row.getAttribute('data-fatal')).toBe('true')
    expect(row.querySelector('.status')!.textContent).toContain('start failed 5 times')
    el.remove()
  })

  it('renders the Chinese status word in a real badge (never an empty cell)', async () => {
    const el = await mount({
      programs: [program('web', { state: 'running' }), program('sick', { state: 'fatal' })],
    })
    const badges = [...el.shadowRoot!.querySelectorAll('td.status ak-status-badge')]
    expect(badges).toHaveLength(2)
    const labels = badges.map((b) => b.shadowRoot!.querySelector('.label')!.textContent)
    expect(labels).toEqual(['运行中', '故障'])
    for (const b of badges) {
      expect(b.shadowRoot!.querySelector('.badge')!.getAttribute('data-status')).toBeTruthy()
    }
    el.remove()
  })

  it('keeps the action column present (and fixed) whether or not actions are enabled', async () => {
    const withActions = await mount()
    const without = await mount({ actions: false })
    // Same column count: the reserved action column never appears/disappears.
    expect(withActions.shadowRoot!.querySelectorAll('thead th.ops')).toHaveLength(1)
    expect(without.shadowRoot!.querySelectorAll('thead th.ops')).toHaveLength(1)
    expect(cells(withActions).length).toBe(cells(without).length)
    expect(withActions.shadowRoot!.querySelector('td.ops button')).not.toBeNull()
    expect(without.shadowRoot!.querySelector('td.ops button')).toBeNull()
    withActions.remove()
    without.remove()
  })

  it('changes the rate columns when the title-bar window changes (prefs shared)', async () => {
    const el = await mount()
    const stdout = () => cells(el)[6]!.textContent?.trim()
    expect(stdout()).toBe('2.0/s')
    const select = el.shadowRoot!.querySelector<HTMLSelectElement>('select.rate-select')!
    expect(select.value).toBe('10')
    select.value = '60'
    select.dispatchEvent(new Event('change', { bubbles: true }))
    await el.updateComplete
    expect(stdout()).toBe('3.0/s')
    expect(localStorage.getItem('ak.rate_window_s')).toBe('60')
    el.remove()
  })

  it('keeps every table on the page on the same window', async () => {
    const first = await mount()
    const second = await mount()
    const select = first.shadowRoot!.querySelector<HTMLSelectElement>('select.rate-select')!
    select.value = '1'
    select.dispatchEvent(new Event('change', { bubbles: true }))
    await Promise.all([first.updateComplete, second.updateComplete])
    expect(second.shadowRoot!.querySelector<HTMLSelectElement>('select.rate-select')!.value).toBe('1')
    expect(cells(second)[6]!.textContent?.trim()).toBe('1.0/s')
    first.remove()
    second.remove()
  })

  it('emits program-select on row click but not from the ⋯ trigger', async () => {
    const { store } = stubStore()
    const el = await mount({ store })
    const seen: ProgramSelectDetail[] = []
    el.addEventListener('program-select', (e) =>
      seen.push((e as CustomEvent<ProgramSelectDetail>).detail),
    )

    el.shadowRoot!.querySelector<HTMLElement>('td.status')!.click()
    expect(seen).toEqual([{ app: 'demo', program: 'web' }])

    el.shadowRoot!.querySelector<HTMLButtonElement>('.op-trigger')!.click()
    await el.updateComplete
    expect(seen).toHaveLength(1)
    el.remove()
  })

  it('renders a single ⋯ trigger with stable hooks, no resident button group', async () => {
    const el = await mount()
    const trigger = el.shadowRoot!.querySelector<HTMLButtonElement>('td.ops .op-trigger')!
    expect(trigger).not.toBeNull()
    expect(trigger.getAttribute('data-program')).toBe('web')
    expect(trigger.getAttribute('aria-haspopup')).toBe('menu')
    expect(trigger.getAttribute('aria-label')).toContain('web')
    // No per-action row buttons anymore (webui-ui: 列表行快捷操作).
    expect(el.shadowRoot!.querySelectorAll('td.ops button')).toHaveLength(1)
    expect(el.shadowRoot!.querySelectorAll('td.ops button[data-kind]')).toHaveLength(0)
    // A stopping program offers nothing: no trigger cell content at all.
    const stopping = await mount({ programs: [program('web', { state: 'stopping' })] })
    expect(stopping.shadowRoot!.querySelector('td.ops .op-trigger')).toBeNull()
    stopping.remove()
    el.remove()
  })

  it('opens the same menu from the ⋯ trigger and from a row right-click', async () => {
    const { store, action } = stubStore()
    const el = await mount({ store })

    // ⋯ path.
    el.shadowRoot!.querySelector<HTMLButtonElement>('.op-trigger')!.click()
    await el.updateComplete
    expect(menuEl(el).isOpen).toBe(true)
    expect(menuItems(el).map((b) => b.dataset.kind)).toEqual(['stop', 'restart'])
    expect(el.shadowRoot!.querySelectorAll('ak-action-menu')).toHaveLength(1)
    // The row gets the defensive open marker while the menu is up.
    expect(el.shadowRoot!.querySelector('tbody tr[data-menu-open="true"]')).not.toBeNull()

    // Esc closes it (document-level key handler), then the right-click path
    // opens the SAME menu with the same items (webui-ui: 双入口同一菜单).
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    await el.updateComplete
    expect(menuEl(el).isOpen).toBe(false)
    expect(el.shadowRoot!.querySelector('tbody tr[data-menu-open="true"]')).toBeNull()

    const { prevented } = await openMenuByRowClick(el)
    expect(prevented).toBe(true) // the native context menu is suppressed
    expect(menuEl(el).isOpen).toBe(true)
    expect(menuItems(el).map((b) => b.dataset.kind)).toEqual(['stop', 'restart'])
    expect(action).not.toHaveBeenCalled()
    el.remove()
  })

  it('right-clicking a stopping row does not open an empty menu', async () => {
    const el = await mount({ programs: [program('web', { state: 'stopping' })] })
    const { prevented } = await openMenuByRowClick(el)
    expect(prevented).toBe(true)
    expect(menuEl(el).isOpen).toBe(false)
    el.remove()
  })

  it('swallows the dismiss click that closes the menu without navigating or POSTing', async () => {
    const { store, action } = stubStore()
    const el = await mount({ store })
    const seen: ProgramSelectDetail[] = []
    el.addEventListener('program-select', (e) =>
      seen.push((e as CustomEvent<ProgramSelectDetail>).detail),
    )
    // Freeze the clock around the dismissal so the window is deterministic.
    const now = vi.spyOn(performance, 'now').mockReturnValue(5000)

    await openMenuByRowClick(el)
    // The closing gesture: pointerdown outside (captured) closes the menu,
    // then the SAME interaction's click lands on the row.
    document.body.dispatchEvent(
      new MouseEvent('pointerdown', { bubbles: true, composed: true }),
    )
    await el.updateComplete
    expect(menuEl(el).isOpen).toBe(false)
    const tr = el.shadowRoot!.querySelector('tbody tr[data-program]') as HTMLElement
    tr.dispatchEvent(new MouseEvent('click', { bubbles: true, composed: true, button: 0 }))
    await el.updateComplete
    expect(seen).toEqual([]) // the dismissal click must not navigate
    expect(action).not.toHaveBeenCalled() // …nor POST anything

    // A normal click once the window has passed still navigates.
    now.mockReturnValue(5200)
    tr.dispatchEvent(new MouseEvent('click', { bubbles: true, composed: true, button: 0 }))
    await el.updateComplete
    expect(seen).toEqual([{ app: 'demo', program: 'web' }])
    now.mockRestore()
    el.remove()
  })

  it('confirms destructive actions inside the menu before requesting anything', async () => {
    const { store, action, gate } = gatedStore()
    const el = await mount({ store })

    await openMenuByRowClick(el)
    menuItems(el)[0]!.click() // 停止
    await el.updateComplete
    expect(action).not.toHaveBeenCalled()
    // The inline confirm names the program and carries the red action colour.
    const confirm = confirmButton(el)!
    expect(confirm).not.toBeNull()
    expect(confirm.dataset.kind).toBe('stop')
    expect(menuEl(el).shadowRoot!.textContent).toContain('web')

    // Cancel: zero side effects, back to the item list.
    menuEl(el).shadowRoot!.querySelector<HTMLButtonElement>('.confirm-cancel')!.click()
    await el.updateComplete
    expect(action).not.toHaveBeenCalled()
    expect(menuItems(el)).toHaveLength(2)

    // Confirm: the request goes out, the result rides the table's feedback strip.
    menuItems(el)[0]!.click()
    await el.updateComplete
    confirmButton(el)!.click()
    await el.updateComplete
    expect(action).toHaveBeenCalledWith('web', 'stop')
    gate.resolve('ok')
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    expect(menuEl(el).isOpen).toBe(false)
    const feedback = el.shadowRoot!.querySelector('.table-feedback')!
    expect(feedback.getAttribute('role')).toBe('status')
    expect(feedback.textContent).toContain('ok')
    el.remove()
  })

  it('starts a stopped program from the menu without a confirmation step', async () => {
    const { store, action } = stubStore()
    const el = await mount({ store, programs: [program('web', { state: 'stopped' })] })

    el.shadowRoot!.querySelector<HTMLButtonElement>('.op-trigger')!.click()
    await el.updateComplete
    expect(menuItems(el).map((b) => b.dataset.kind)).toEqual(['start'])
    menuItems(el)[0]!.click()
    await el.updateComplete
    expect(confirmButton(el)).toBeNull()
    expect(action).toHaveBeenCalledWith('web', 'start')
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    expect(menuEl(el).isOpen).toBe(false)
    expect(el.shadowRoot!.querySelector('.table-feedback')!.textContent).toContain('ok')
    el.remove()
  })

  it('surfaces an action failure in an alert strip instead of failing silently', async () => {
    const { store, gate } = gatedStore()
    const el = await mount({ store })

    await openMenuByRowClick(el)
    menuItems(el)[1]!.click() // 重启
    await el.updateComplete
    confirmButton(el)!.click()
    await el.updateComplete
    gate.reject(new Error('program is stopping'))
    await new Promise((r) => setTimeout(r, 0))
    await el.updateComplete
    const feedback = el.shadowRoot!.querySelector('.table-feedback')!
    expect(feedback.getAttribute('role')).toBe('alert')
    expect(feedback.getAttribute('data-error')).toBe('true')
    expect(feedback.textContent).toContain('program is stopping')
    el.remove()
  })

  it('marks pending programs and shows the empty state', async () => {
    const el = await mount({ pending: ['web'] })
    expect(el.shadowRoot!.querySelector('.changed-mark')!.textContent).toContain('待应用')
    el.remove()

    const empty = await mount({ programs: [], emptyText: '该应用没有声明任何程序' })
    expect(empty.shadowRoot!.querySelector('.empty-cell')!.textContent).toContain(
      '该应用没有声明任何程序',
    )
    empty.remove()
  })
})
