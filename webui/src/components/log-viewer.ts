/**
 * Program log viewer (webui-ui: 日志查看 + 高速日志降级显示).
 *
 * - stdout/stderr switchable; starts at the ring-buffer tail, then follows.
 * - Follow mode keeps the viewport pinned to the newest content; scrolling
 *   up pauses following until the user returns to the bottom.
 * - Bounded buffer (~5000 lines): the oldest rows are evicted; when not
 *   following, the scroll position is compensated so history browsing is
 *   not yanked around.
 * - Backend loss surfaces as explicit "跳过 N 行" marker rows (LOG_GAP
 *   frames) — distinct styling, never mixed into log content.
 * - Rendering is rAF-batched: log floods append DOM at most once per frame
 *   (~10 fps cap), so render cost is decoupled from output rate.
 * - Fixed three-palette colour scheme (webui-visual-system: 日志窗口配色档位):
 *   terminal black (default) / paper white / cream, chosen in the toolbar and
 *   persisted per-browser. The palette lives in component-local variables —
 *   it never joins tokens.css and never flips with the app's `wa-dark` theme.
 */

import { LitElement, css, html } from 'lit'
import { customElement, property, state } from 'lit/decorators.js'
import { consoleStore, type ConsoleStore, type LogListener } from '../lib/store.js'
import type { LogEntry, StreamId } from '../lib/types.js'
import { LOG_STYLE_CHOICES, loadLogStyle, saveLogStyle, type LogStyle } from '../lib/prefs.js'

const MAX_ENTRIES = 5000
const MAX_RENDER_FPS_MS = 100

@customElement('ak-log-viewer')
export class AkLogViewer extends LitElement {
  @property() program = ''
  /** Injectable for tests; defaults to the shared singleton. */
  @property({ attribute: false }) store: ConsoleStore = consoleStore

  /**
   * Log palette (terminal black / paper white / cream), reflected to the host
   * as `data-log-palette` — the styles key the three variable groups off it.
   * Persisted per-browser; never follows the app's light/dark theme.
   */
  @property({ attribute: 'data-log-palette', reflect: true })
  palette: LogStyle = loadLogStyle()

  @state() private stream: StreamId = 0
  @state() following = true
  @state() entryCount = 0
  private entries: LogEntry[] = []
  /** Partial line carried between frames (frames split mid-line). */
  private carry: string[] = []
  /** Rows appended/evicted since the last paint. */
  private pending: LogEntry[] = []
  private evicted = 0
  private evictedHeight = 0
  private renderTimer: ReturnType<typeof setTimeout> | null = null
  private lastPaint = 0
  private logListener: LogListener | null = null
  private unsubscribeGap: (() => void) | null = null
  private scroller: HTMLElement | null = null

  static styles = css`
    :host {
      display: flex;
      flex-direction: column;
      /* Fills the host's remaining height (program detail: 日志占满剩余高度);
         260px is the floor before the page itself scrolls. */
      flex: 1 1 auto;
      min-height: 260px;
      min-width: 0;
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-lg, 10px);
      overflow: hidden;
      /* Fixed log palette (webui-visual-system: 日志窗口配色档位) — component
         locals, not theme tokens: the scroller never flips with the app's
         wa-dark class. Terminal black is the default档. */
      --log-bg: #0f1115;
      --log-fg: #e6e9ef;
      --log-err: #f47174;
      --log-gap: #ff9457;
      background: var(--log-bg);
    }
    :host([data-log-palette='white']) {
      --log-bg: #ffffff;
      --log-fg: #1f2328;
      --log-err: #b42318;
      --log-gap: #b45309;
    }
    :host([data-log-palette='cream']) {
      --log-bg: #f2eddf;
      --log-fg: #4a4234;
      --log-err: #a63d40;
      --log-gap: #8a6d3b;
    }
    .toolbar {
      display: flex;
      align-items: center;
      gap: var(--ak-space-3);
      padding: var(--ak-space-1) var(--ak-space-2);
      background: var(--ak-surface);
      border-bottom: 1px solid var(--ak-border);
      font-size: var(--ak-fs-sm);
      flex: 0 0 auto;
    }
    .tabs {
      display: inline-flex;
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-sm, 6px);
      overflow: hidden;
    }
    .tabs button {
      height: calc(var(--ak-control-h) - var(--ak-space-2));
      padding: 0 var(--ak-space-3);
      box-sizing: border-box;
      border: none;
      background: transparent;
      color: var(--ak-text-dim);
      font: inherit;
      font-size: var(--ak-fs-sm);
      cursor: pointer;
    }
    .tabs button[aria-pressed='true'] {
      background: var(--ak-accent-soft);
      color: var(--ak-accent-hover);
      font-weight: 600;
    }
    .follow {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-1);
      color: var(--ak-text-dim);
      cursor: pointer;
      user-select: none;
      font-size: var(--ak-fs-sm);
    }
    .follow input {
      accent-color: var(--ak-accent-strong);
    }
    /* Palette switch: the toolbar keeps the surface's own colours — only the
       log scroller below changes with the selected档. */
    .palette {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-1);
      color: var(--ak-text-dim);
      font-size: var(--ak-fs-sm);
      white-space: nowrap;
    }
    .palette select {
      height: calc(var(--ak-control-h) - var(--ak-space-2));
      padding: 0 var(--ak-space-1);
      box-sizing: border-box;
      background: var(--ak-surface-2);
      color: var(--ak-text);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-sm, 6px);
      font: inherit;
      font-size: var(--ak-fs-xs);
      cursor: pointer;
    }
    .spacer {
      flex: 1;
    }
    .meta {
      color: var(--ak-text-faint);
      font-size: var(--ak-fs-xs);
      font-variant-numeric: tabular-nums;
      white-space: nowrap;
    }
    .scroller {
      flex: 1;
      overflow-y: auto;
      min-height: 0;
      padding: var(--ak-space-2) var(--ak-space-3);
      font-family: var(--ak-font-mono, monospace);
      font-size: 0.78rem;
      line-height: 1.5;
    }
    .row {
      white-space: pre-wrap;
      word-break: break-all;
      color: var(--log-fg);
      min-height: 1.5em;
    }
    .row[data-stream='1'] {
      /* stderr stays in the red family on every palette. */
      color: var(--log-err);
    }
    .gap-row {
      display: flex;
      align-items: center;
      gap: 8px;
      margin: 2px 0;
      padding: 2px 8px;
      border-left: 3px solid var(--log-gap);
      background: color-mix(in srgb, var(--log-gap) 12%, transparent);
      color: var(--log-gap);
      font-family: var(--ak-font-sans, sans-serif);
      font-size: 0.76rem;
      font-weight: 600;
      user-select: none;
    }
  `

  connectedCallback(): void {
    super.connectedCallback()
    this.attachLogRouting()
  }

  disconnectedCallback(): void {
    this.detachLogRouting()
    if (this.renderTimer) clearTimeout(this.renderTimer)
    super.disconnectedCallback()
  }

  updated(changed: Map<string, unknown>): void {
    // The scroller exists only after the first render — capture lazily.
    if (!this.scroller) {
      this.scroller = this.renderRoot.querySelector('.scroller')
      this.scroller?.addEventListener('scroll', this.onScroll)
    }
    if (changed.has('program')) {
      // Program switched: drop the old stream's content and resubscribe.
      this.entries = []
      this.pending = []
      this.carry = []
      this.evicted = 0
      this.evictedHeight = 0
      this.entryCount = 0
      this.scroller?.querySelectorAll('.row, .gap-row').forEach((n) => n.remove())
      this.resubscribe()
      if (this.following) this.scrollToBottom()
    }
    this.schedulePaint(true)
  }

  private attachLogRouting(): void {
    this.detachLogRouting()
    this.logListener = (frame) => {
      if (frame.program !== this.program || frame.stream !== this.stream) return
      this.ingestText(frame.text)
    }
    this.logUnsub = this.store.onLogFrame(this.logListener)
    this.unsubscribeGap = this.store.onGap((program, stream, skipped) => {
      if (program !== this.program || stream !== this.stream) return
      this.pushEntries([{ kind: 'gap', skipped }])
    })
  }

  private logUnsub: (() => void) | null = null

  private detachLogRouting(): void {
    this.logUnsub?.()
    this.logUnsub = null
    this.unsubscribeGap?.()
    this.unsubscribeGap = null
  }

  private resubscribe(): void {
    if (!this.program) return
    // (Re)subscribe for the current program/stream; the server replays the
    // ring tail first, then follows.
    this.store.subscribeLogs(this.program, this.stream)
  }

  private setStream(s: StreamId): void {
    if (this.stream === s) return
    this.store.unsubscribeLogs(this.program, this.stream)
    this.stream = s
    this.entries = []
    this.pending = []
    this.carry = []
    this.evicted = 0
    this.evictedHeight = 0
    this.entryCount = 0
    this.scroller?.querySelectorAll('.row, .gap-row').forEach((n) => n.remove())
    this.resubscribe()
  }

  private ingestText(text: string): void {
    // Import lazily to keep the module graph flat in tests.
    const entries = splitLines(text, this.carry)
    this.pushEntries(entries)
  }

  private pushEntries(entries: LogEntry[]): void {
    if (entries.length === 0) return
    this.pending.push(...entries)
    this.entries.push(...entries)
    // Bound the retained history (webui-ui: 有界缓冲).
    while (this.entries.length > MAX_ENTRIES) {
      const dropped = this.entries.shift()
      this.evicted++
      if (dropped?.kind === 'line') this.evictedHeight += 24 // approx row height
      else this.evictedHeight += 28
    }
    this.entryCount = this.entries.length
    this.schedulePaint()
  }

  /** rAF-ish batching: at most one DOM append per MAX_RENDER_FPS_MS. */
  private schedulePaint(immediate = false): void {
    if (this.renderTimer) return
    const elapsed = performance.now() - this.lastPaint
    const delay = immediate || elapsed >= MAX_RENDER_FPS_MS ? 0 : MAX_RENDER_FPS_MS - elapsed
    this.renderTimer = setTimeout(() => {
      this.renderTimer = null
      this.lastPaint = performance.now()
      this.paint()
    }, delay)
  }

  private paint(): void {
    const scroller = this.scroller
    if (!scroller) return
    const batch = this.pending
    this.pending = []
    if (batch.length === 0) return
    const wasFollowing = this.following
    const atBottom = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40
    const frag = document.createDocumentFragment()
    for (const e of batch) {
      if (e.kind === 'line') {
        const row = document.createElement('div')
        row.className = 'row'
        row.dataset.stream = String(this.stream)
        row.textContent = e.text
        frag.appendChild(row)
      } else {
        const row = document.createElement('div')
        row.className = 'gap-row'
        row.textContent = `⏭ 跳过 ${e.skipped.toLocaleString()} 行（输出过快，实时流已截断；文件与缓冲完整）`
        frag.appendChild(row)
      }
    }
    // Compensate scroll when the user is reading history and old rows were
    // evicted (webui-ui: 翻阅时高流量不拉回).
    if (!wasFollowing && this.evicted > 0) {
      scroller.scrollTop -= this.evictedHeight
    }
    this.evicted = 0
    this.evictedHeight = 0
    scroller.appendChild(frag)
    // Cap live DOM rows to the same bound as the entry list.
    const rows = scroller.querySelectorAll('.row, .gap-row')
    const overflow = rows.length - MAX_ENTRIES
    for (let i = 0; i < overflow; i++) rows[i]?.remove()
    if (wasFollowing && atBottom) this.scrollToBottom()
    this.entryCount = this.entries.length
  }

  private scrollToBottom(): void {
    const scroller = this.scroller
    if (scroller) scroller.scrollTop = scroller.scrollHeight
  }

  private onScroll = (): void => {
    const scroller = this.scroller
    if (!scroller) return
    const atBottom = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40
    // Following yields to browsing (webui-ui: 跟随让位于浏览) and resumes at
    // the bottom.
    if (atBottom !== this.following) {
      this.following = atBottom
    }
  }

  private static readonly PALETTE_LABEL: Record<LogStyle, string> = {
    black: '终端黑',
    white: '纸面白',
    cream: '米黄',
  }

  private onPaletteChange(e: Event): void {
    const value = (e.target as HTMLSelectElement).value as LogStyle
    if (!(LOG_STYLE_CHOICES as readonly string[]).includes(value)) return
    // Takes effect immediately (the styles key off the reflected attribute)
    // and persists for the next visit (webui-ui: 档位切换即时生效且持久).
    this.palette = value
    saveLogStyle(value)
  }

  render() {
    return html`
      <div class="toolbar">
        <div class="tabs" role="group" aria-label="输出流">
          <button aria-pressed=${this.stream === 0} @click=${() => this.setStream(0)}>stdout</button>
          <button aria-pressed=${this.stream === 1} @click=${() => this.setStream(1)}>stderr</button>
        </div>
        <label class="follow">
          <input
            type="checkbox"
            .checked=${this.following}
            @change=${(e: Event) => {
              this.following = (e.target as HTMLInputElement).checked
              if (this.following) this.scrollToBottom()
            }}
          />
          跟随
        </label>
        <label class="palette">
          配色
          <select
            class="palette-select"
            aria-label="日志配色档位"
            .value=${this.palette}
            @change=${this.onPaletteChange}
          >
            ${LOG_STYLE_CHOICES.map(
              (style) => html`<option value=${style} ?selected=${style === this.palette}>
                ${AkLogViewer.PALETTE_LABEL[style]}
              </option>`,
            )}
          </select>
        </label>
        <span class="spacer"></span>
        <span class="meta">${this.entryCount} 行 · 缓冲上限 ${MAX_ENTRIES} 行</span>
      </div>
      <div
        class="scroller"
        role="log"
        aria-label=${`${this.program} ${this.stream === 0 ? 'stdout' : 'stderr'} 日志`}
        aria-live=${this.following ? 'polite' : 'off'}
      ></div>
    `
  }
}

/** Split frame text into line entries, carrying a trailing partial line. */
export function splitLines(text: string, carry: string[]): LogEntry[] {
  const entries: LogEntry[] = []
  let rest = text
  if (carry.length > 0) {
    rest = carry.pop()! + text
  }
  let start = 0
  for (let i = 0; i < rest.length; i++) {
    if (rest.charCodeAt(i) === 10) {
      entries.push({ kind: 'line', text: rest.slice(start, i) })
      start = i + 1
    }
  }
  if (start < rest.length) {
    carry.push(rest.slice(start))
  }
  return entries
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-log-viewer': AkLogViewer
  }
}
