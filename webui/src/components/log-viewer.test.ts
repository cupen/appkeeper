// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import './log-viewer.js'
import type { AkLogViewer } from './log-viewer.js'
import { ConsoleStore } from '../lib/store.js'
import { MSG, type StreamId } from '../lib/types.js'

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

function logFrame(program: string, stream: StreamId, text: string): Uint8Array {
  const name = new TextEncoder().encode(program)
  const body = new TextEncoder().encode(text)
  const out = new Uint8Array(1 + 2 + name.length + 1 + body.length)
  let i = 0
  out[i++] = MSG.LOG
  out[i++] = name.length >> 8
  out[i++] = name.length & 0xff
  out.set(name, i)
  i += name.length
  out[i++] = stream
  out.set(body, i)
  return out
}

function gapFrame(program: string, stream: StreamId, skipped: number): Uint8Array {
  const name = new TextEncoder().encode(program)
  const out = new Uint8Array(1 + 2 + name.length + 1 + 8)
  let i = 0
  out[i++] = MSG.LOG_GAP
  out[i++] = name.length >> 8
  out[i++] = name.length & 0xff
  out.set(name, i)
  i += name.length
  out[i++] = stream
  new DataView(out.buffer).setBigUint64(i, BigInt(skipped))
  return out
}

describe('ak-log-viewer', () => {
  let store: ConsoleStore
  let ws: FakeWebSocket
  let el: AkLogViewer

  beforeEach(async () => {
    localStorage.clear()
    vi.useFakeTimers()
    vi.stubGlobal('WebSocket', FakeWebSocket as unknown as typeof WebSocket)
    FakeWebSocket.instances = []
    store = new ConsoleStore()
    store.start()
    ws = FakeWebSocket.instances[0]!
    ws.serverOpen()
    el = document.createElement('ak-log-viewer') as AkLogViewer
    el.store = store
    el.program = 'p'
    document.body.appendChild(el)
    await vi.advanceTimersByTimeAsync(10)
    expect(el.shadowRoot).not.toBeNull()
  })

  afterEach(() => {
    store.stop()
    el.remove()
    vi.useRealTimers()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  function rows(): (Element | null | undefined)[] {
    return [...el.shadowRoot!.querySelectorAll('.row, .gap-row')]
  }

  it('subscribes on attach and renders incoming lines', async () => {
    expect(JSON.parse(new TextDecoder().decode(ws.sent.at(-1)!))).toEqual({
      action: 'subscribe',
      program: 'p',
      stream: 'out',
    })
    ws.serverFrame(logFrame('p', 0, 'line-1\nline-2\n'))
    await vi.advanceTimersByTimeAsync(120)
    expect(rows().map((r) => r!.textContent)).toEqual(['line-1', 'line-2'])
  })

  it('renders gap markers with distinct styling, never as log lines', async () => {
    ws.serverFrame(gapFrame('p', 0, 250))
    ws.serverFrame(logFrame('p', 0, 'after\n'))
    await vi.advanceTimersByTimeAsync(120)
    const gap = el.shadowRoot!.querySelector('.gap-row')
    expect(gap).not.toBeNull()
    expect(gap!.textContent).toContain('250')
    expect(el.shadowRoot!.textContent).toContain('after')
  })

  it('stays bounded under a flood (MAX_ENTRIES)', async () => {
    for (let b = 0; b < 12; b++) {
      const lines = Array.from({ length: 500 }, (_, i) => `b${b}l${i}`).join('\n')
      ws.serverFrame(logFrame('p', 0, `${lines}\n`))
    }
    // 6000 lines pushed; render is rAF/throttle-batched.
    await vi.advanceTimersByTimeAsync(2000)
    expect(el.entryCount).toBeLessThanOrEqual(5000)
    expect(rows().length).toBeLessThanOrEqual(5000)
    // The OLDEST content is gone: b0l0 must not survive a 6000-line flood.
    const texts = rows().map((r) => r!.textContent)
    expect(texts).not.toContain('b0l0')
    expect(texts.at(-1)).toBe('b11l499')
  })

  it('switching to stderr unsubscribes stdout and re-subscribes', async () => {
    const before = ws.sent.length
    const buttons = el.shadowRoot!.querySelectorAll('.tabs button')
    ;(buttons[1] as HTMLButtonElement).click()
    await vi.advanceTimersByTimeAsync(10)
    expect(JSON.parse(new TextDecoder().decode(ws.sent.at(-2)!))).toEqual({
      action: 'unsubscribe',
      program: 'p',
      stream: 'out',
    })
    expect(JSON.parse(new TextDecoder().decode(ws.sent.at(-1)!))).toEqual({
      action: 'subscribe',
      program: 'p',
      stream: 'err',
    })
    expect(before).toBeGreaterThan(0)
  })

  it('follow pins to bottom; manual scroll-up suspends following', async () => {
    ws.serverFrame(logFrame('p', 0, Array.from({ length: 200 }, (_, i) => `l${i}`).join('\n') + '\n'))
    await vi.advanceTimersByTimeAsync(150)
    const scroller = el.shadowRoot!.querySelector('.scroller') as HTMLElement
    // happy-dom has no real layout: scrollHeight is 0, so "at bottom" holds
    // and the follow checkbox stays on after paint.
    expect(el.following).toBe(true)
    // Simulate the user scrolling up (viewport not at bottom).
    Object.defineProperty(scroller, 'scrollHeight', { value: 5000, configurable: true })
    Object.defineProperty(scroller, 'clientHeight', { value: 200, configurable: true })
    scroller.scrollTop = 100
    scroller.dispatchEvent(new Event('scroll'))
    expect(el.following).toBe(false)
    // Return to bottom → following resumes.
    scroller.scrollTop = 4800
    scroller.dispatchEvent(new Event('scroll'))
    expect(el.following).toBe(true)
  })

  it('defaults to the terminal-black palette (webui-ui: 缺省黑底白字)', async () => {
    expect(el.palette).toBe('black')
    expect(el.getAttribute('data-log-palette')).toBe('black')
    const select = el.shadowRoot!.querySelector<HTMLSelectElement>('select.palette-select')!
    expect(select.value).toBe('black')
  })

  it('switches palettes immediately and persists the choice (webui-ui: 档位切换即时生效且持久)', async () => {
    const select = el.shadowRoot!.querySelector<HTMLSelectElement>('select.palette-select')!
    select.value = 'cream'
    select.dispatchEvent(new Event('change', { bubbles: true }))
    await el.updateComplete
    // Immediate effect: the host attribute that drives the variable groups.
    expect(el.getAttribute('data-log-palette')).toBe('cream')
    expect(localStorage.getItem('ak.log_style')).toBe('cream')
    el.remove()

    // A fresh viewer (page reload equivalent) comes back on the chosen档.
    const el2 = document.createElement('ak-log-viewer') as AkLogViewer
    el2.store = store
    el2.program = 'p'
    document.body.appendChild(el2)
    await el2.updateComplete
    expect(el2.palette).toBe('cream')
    expect(el2.getAttribute('data-log-palette')).toBe('cream')
    el2.remove()
  })

  it('renders no waiting placeholder for an empty program (webui-ui: 空输出无占位文案)', async () => {
    // No frames sent: the program has produced no output yet.
    await vi.advanceTimersByTimeAsync(120)
    const root = el.shadowRoot!
    expect(root.textContent).not.toContain('等待输出')
    expect(root.querySelector('.empty-hint')).toBeNull()
    // The row counter is unaffected: 0 lines, buffer cap still named.
    expect(root.querySelector('.meta')?.textContent).toContain('0 行')
  })

  it('keeps the palette fixed when the app theme flips to wa-dark (webui-visual-system: 不随应用主题翻转)', async () => {
    // Component-level equivalent: the compiled stylesheet must bind every log
    // colour to the palette-local variables (never theme tokens), and the
    // three spec'd palettes must all be present.
    const css = (el.constructor as typeof AkLogViewer).styles.cssText as string
    expect(css).toContain(':host([data-log-palette=\'white\'])')
    expect(css).toContain(':host([data-log-palette=\'cream\'])')
    // Spec-locked cream values (webui-visual-system: 米黄档).
    expect(css).toContain('#f2eddf')
    expect(css).toContain('#4a4234')
    // The scroller colours come from the locals only — no theme token can
    // reach the log area, so a `wa-dark` flip cannot re-skin it.
    expect(css).toMatch(/\.row\s*\{[^}]*var\(--log-fg\)/)
    expect(css).toMatch(/\.row\[data-stream='1'\]\s*\{[^}]*var\(--log-err\)/)
    expect(css).toMatch(/\.gap-row\s*\{[^}]*var\(--log-gap\)/)
    expect(css).not.toContain('--ak-well')
    expect(css).not.toContain('--ak-status-failed')
    expect(css).not.toContain('--ak-signal')
    // And the runtime attribute that keys the groups is theme-independent.
    document.documentElement.classList.add('wa-dark')
    expect(el.getAttribute('data-log-palette')).toBe('black')
    document.documentElement.classList.remove('wa-dark')
  })
})
