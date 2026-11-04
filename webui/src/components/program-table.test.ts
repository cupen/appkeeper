// @vitest-environment happy-dom
//
// webui-ui: 全局程序总览 / 列表行快捷操作 — the shared program table owns
// column order, placeholders, status badges, the rate-window switch and the
// row-tail actions. Both routed views consume it, so these assertions cover
// the contract they rely on.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import './program-table.js'
import type { AkProgramTable, ProgramSelectDetail } from './program-table.js'
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

  it('emits program-select on row click but not from an action button', async () => {
    const { store } = stubStore()
    const el = await mount({ store })
    const seen: ProgramSelectDetail[] = []
    el.addEventListener('program-select', (e) =>
      seen.push((e as CustomEvent<ProgramSelectDetail>).detail),
    )

    el.shadowRoot!.querySelector<HTMLElement>('td.status')!.click()
    expect(seen).toEqual([{ app: 'demo', program: 'web' }])

    vi.stubGlobal('confirm', vi.fn().mockReturnValue(false))
    el.shadowRoot!.querySelector<HTMLButtonElement>('button[data-kind="stop"]')!.click()
    await new Promise((r) => setTimeout(r, 0))
    expect(seen).toHaveLength(1)
    el.remove()
  })

  it('confirms destructive actions before requesting anything', async () => {
    const { store, action } = stubStore()
    const el = await mount({ store })
    const confirm = vi.fn().mockReturnValue(false)
    vi.stubGlobal('confirm', confirm)

    el.shadowRoot!.querySelector<HTMLButtonElement>('button[data-kind="stop"]')!.click()
    await new Promise((r) => setTimeout(r, 0))
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(String(confirm.mock.calls[0]![0])).toContain('停止')
    expect(action).not.toHaveBeenCalled()

    confirm.mockReturnValue(true)
    el.shadowRoot!.querySelector<HTMLButtonElement>('button[data-kind="stop"]')!.click()
    await new Promise((r) => setTimeout(r, 0))
    expect(action).toHaveBeenCalledWith('web', 'stop')
    await el.updateComplete
    const feedback = el.shadowRoot!.querySelector('.table-feedback')!
    expect(feedback.getAttribute('role')).toBe('status')
    expect(feedback.textContent).toContain('ok')
    el.remove()
  })

  it('starts a stopped program without a confirmation step', async () => {
    const { store, action } = stubStore()
    const el = await mount({ store, programs: [program('web', { state: 'stopped' })] })
    const confirm = vi.fn()
    vi.stubGlobal('confirm', confirm)

    expect(el.shadowRoot!.querySelector('button[data-kind="stop"]')).toBeNull()
    el.shadowRoot!.querySelector<HTMLButtonElement>('button[data-kind="start"]')!.click()
    await new Promise((r) => setTimeout(r, 0))
    expect(confirm).not.toHaveBeenCalled()
    expect(action).toHaveBeenCalledWith('web', 'start')
    el.remove()
  })

  it('surfaces an action failure in an alert strip instead of failing silently', async () => {
    const action = vi.fn().mockRejectedValue(new Error('program is stopping'))
    const el = await mount({ store: { programAction: action } as unknown as ConsoleStore })
    vi.stubGlobal('confirm', vi.fn().mockReturnValue(true))

    el.shadowRoot!.querySelector<HTMLButtonElement>('button[data-kind="restart"]')!.click()
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
