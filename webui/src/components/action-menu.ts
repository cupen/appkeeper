/**
 * Unified program action menu (webui-ui: 程序操作菜单).
 *
 * One component serves every entry point — row right-click, the ⋯ trigger
 * button and the keyboard Menu key all open the same floating layer, so the
 * state machine, confirmation step and action colours cannot drift between
 * surfaces (design D1):
 *
 * - `closed → open → confirming(stop|restart) → busy`; `start` is loss-free
 *   and runs straight from `open`, `stop`/`restart` morph the same overlay
 *   into an inline confirm step (program name + action-coloured button).
 * - Esc walks back one level at a time (confirming → open → closed) and the
 *   final close hands focus back to the trigger.
 * - The overlay is `position: fixed`; scroll (captured), resize and a
 *   pointerdown outside it all close it, so it can never be caught drifting.
 * - Actions run through the injected store (`/api/programs/<name>/<action>`);
 *   the outcome is re-raised as `program-action-result` (ok/text) for the
 *   owning surface's existing feedback channel — this component stays silent
 *   about success/failure itself.
 * - Action colours reuse the status token families (design D3): start green
 *   (`--ak-status-done*`), restart amber (`--ak-status-working*`), stop red
 *   (`--ak-status-failed*`). Colour is never the only channel — every item
 *   keeps its action word and a shape-distinct glyph (▶/⟳/■).
 */

import { LitElement, css, html, nothing } from 'lit'
import { customElement, property, state } from 'lit/decorators.js'
import { consoleStore, type ConsoleStore } from '../lib/store.js'
import type { ProgramInfo } from '../lib/types.js'

export type ProgramAction = 'start' | 'stop' | 'restart'
export type MenuPhase = 'closed' | 'open' | 'confirming' | 'busy'

export const ACTION_LABEL: Record<ProgramAction, string> = {
  start: '启动',
  stop: '停止',
  restart: '重启',
}

/** Shape-distinct glyphs so the three actions survive greyscale. */
export const ACTION_ICON: Record<ProgramAction, string> = {
  start: '▶',
  restart: '⟳',
  stop: '■',
}

/**
 * Which actions a program in `state` offers (webui-ui: 菜单项与状态匹配):
 * running/starting → stop + restart, the transient stopping phase offers
 * nothing, everything else offers start. Migrated here so both surfaces (and
 * the menu itself) share one source of truth.
 */
export function actionsFor(state: string): ProgramAction[] {
  if (state === 'running' || state === 'starting') return ['stop', 'restart']
  // "stopping" is transient: neither start nor stop is meaningful.
  if (state === 'stopping') return []
  return ['start']
}

export interface ProgramActionResultDetail {
  ok: boolean
  text: string
  program: string
  action: ProgramAction
}

export interface MenuToggleDetail {
  open: boolean
  program: string
}

@customElement('ak-action-menu')
export class AkActionMenu extends LitElement {
  /** Injectable for tests; defaults to the shared singleton. */
  @property({ attribute: false }) store: ConsoleStore = consoleStore

  @state() private phase: MenuPhase = 'closed'
  @state() private program: ProgramInfo | null = null
  /** Meaningful only while `phase === 'confirming'`. */
  @state() private confirmKind: 'stop' | 'restart' = 'stop'
  @state() private posX = 0
  @state() private posY = 0

  /** Element focus returns to when the menu closes (design D1). */
  private trigger: HTMLElement | null = null
  private detachCloseListeners: (() => void) | null = null
  /** `performance.now()` of the last close; drives the click-through guard. */
  private closedAt = -Infinity

  static styles = css`
    :host {
      display: contents;
    }
    .overlay {
      position: fixed;
      z-index: 1000;
      min-width: 148px;
      box-sizing: border-box;
      padding: var(--ak-space-1);
      background: var(--ak-surface);
      border: 1px solid var(--ak-border-strong, var(--ak-border));
      border-radius: var(--ak-radius-md, 8px);
      box-shadow: var(--ak-shadow-2, 0 10px 32px rgba(0, 0, 0, 0.35));
      font-size: var(--ak-fs-sm);
      color: var(--ak-text);
    }
    .menu-item {
      display: flex;
      align-items: center;
      gap: var(--ak-space-2);
      width: 100%;
      box-sizing: border-box;
      padding: 0 var(--ak-space-3);
      height: 32px;
      border: 1px solid transparent;
      border-radius: var(--ak-radius-sm, 6px);
      background: transparent;
      font: inherit;
      font-weight: 600;
      cursor: pointer;
      text-align: left;
    }
    .menu-item .icon {
      flex: 0 0 auto;
      width: 1em;
      text-align: center;
    }
    /* Action colours (design D3): hue foreground over an 8–14% mix of the
       same family, deepened on hover. Tokens come from the status families so
       light/dark themes each get their remapped values. */
    .menu-item[data-kind='start'] {
      color: var(--ak-status-done);
      background: color-mix(in srgb, var(--ak-status-done) 12%, transparent);
    }
    .menu-item[data-kind='start']:hover:not(:disabled),
    .menu-item[data-kind='start']:focus-visible {
      background: color-mix(in srgb, var(--ak-status-done) 24%, transparent);
      border-color: var(--ak-status-done-border);
    }
    .menu-item[data-kind='restart'] {
      color: var(--ak-status-working);
      background: color-mix(in srgb, var(--ak-status-working) 14%, transparent);
    }
    .menu-item[data-kind='restart']:hover:not(:disabled),
    .menu-item[data-kind='restart']:focus-visible {
      background: color-mix(in srgb, var(--ak-status-working) 28%, transparent);
      border-color: var(--ak-status-working-border);
    }
    .menu-item[data-kind='stop'] {
      color: var(--ak-status-failed);
      background: color-mix(in srgb, var(--ak-status-failed) 12%, transparent);
    }
    .menu-item[data-kind='stop']:hover:not(:disabled),
    .menu-item[data-kind='stop']:focus-visible {
      background: color-mix(in srgb, var(--ak-status-failed) 24%, transparent);
      border-color: var(--ak-status-failed-border);
    }
    .menu-item:disabled {
      opacity: 0.55;
      cursor: default;
    }
    .menu-item:focus-visible {
      outline: var(--ak-focus-ring, 1px solid var(--ak-accent-strong));
      outline-offset: 1px;
    }
    /* Inline confirm step: same overlay, mutated (webui-ui: 二次确认). */
    .confirm {
      display: flex;
      flex-direction: column;
      gap: var(--ak-space-2);
      padding: var(--ak-space-2);
      min-width: 196px;
    }
    .confirm-text {
      color: var(--ak-text-dim);
      line-height: 1.4;
    }
    .confirm-text strong {
      color: var(--ak-text-bright);
      font-weight: 600;
    }
    .confirm-buttons {
      display: flex;
      gap: var(--ak-space-2);
    }
    .confirm-action {
      flex: 1;
      height: 30px;
      border: none;
      border-radius: var(--ak-radius-sm, 6px);
      font: inherit;
      font-weight: 700;
      color: #fff;
      cursor: pointer;
    }
    .confirm-action[data-kind='stop'] {
      background: var(--ak-status-failed);
    }
    .confirm-action[data-kind='restart'] {
      background: var(--ak-status-working);
    }
    .confirm-action:hover:not(:disabled) {
      filter: brightness(1.08);
    }
    .confirm-action:disabled {
      opacity: 0.6;
      cursor: default;
    }
    .confirm-cancel {
      height: 30px;
      padding: 0 var(--ak-space-3);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-sm, 6px);
      background: var(--ak-surface-2);
      color: var(--ak-text);
      font: inherit;
      cursor: pointer;
    }
    .confirm-cancel:hover:not(:disabled) {
      border-color: var(--ak-border-strong, var(--ak-border));
      color: var(--ak-text-bright);
    }
    .confirm-action:focus-visible,
    .confirm-cancel:focus-visible {
      outline: var(--ak-focus-ring, 1px solid var(--ak-accent-strong));
      outline-offset: 1px;
    }
  `

  /** True while any visible phase is active (open / confirming / busy). */
  get isOpen(): boolean {
    return this.phase !== 'closed'
  }

  /**
   * True when a click lands within `withinMs` of the last close. The
   * document-level `pointerdown` capture closes the menu *before* the click
   * dispatches, so the dismissing click is still on its way — the owning
   * surface must swallow it instead of treating it as navigation
   * (design D2: 防伪事件穿透).
   */
  wasRecentlyClosed(withinMs = 150): boolean {
    return performance.now() - this.closedAt < withinMs
  }

  /** The program the menu is currently attached to, if any. */
  get openProgramName(): string | null {
    return this.isOpen ? (this.program?.name ?? null) : null
  }

  /**
   * Open the menu for one program. `(x, y)` is either the pointer position
   * (contextmenu) or the trigger button's anchor point; `trigger` receives
   * focus back when the menu closes. Opening for another program while open
   * simply retargets the same overlay.
   */
  openFor(program: ProgramInfo, x: number, y: number, trigger: HTMLElement | null = null): void {
    this.program = program
    this.posX = x
    this.posY = y
    if (this.phase === 'closed') {
      this.trigger = trigger ?? (document.activeElement as HTMLElement | null)
      this.attachCloseListeners()
    }
    this.phase = 'open'
    // Focus enters the first enabled item (design D1: 打开时焦点入首个可用项).
    this.focusFirstItem()
    this.dispatchEvent(
      new CustomEvent<MenuToggleDetail>('menu-toggle', {
        detail: { open: true, program: program.name },
        bubbles: true,
        composed: true,
      }),
    )
  }

  /** Close unconditionally and hand focus back to the trigger. */
  close(): void {
    if (this.phase === 'closed') return
    this.phase = 'closed'
    const program = this.program?.name ?? ''
    this.detachCloseListeners?.()
    this.detachCloseListeners = null
    this.closedAt = performance.now()
    this.trigger?.focus()
    this.trigger = null
    this.dispatchEvent(
      new CustomEvent<MenuToggleDetail>('menu-toggle', {
        detail: { open: false, program },
        bubbles: true,
        composed: true,
      }),
    )
  }

  private attachCloseListeners(): void {
    const onScroll = () => this.close()
    const onResize = () => this.close()
    const onPointerDown = (e: PointerEvent) => {
      // Anything outside the overlay closes it; the composed path bubbles out
      // of this shadow root, so compose the hit-test with the rendered node.
      const overlay = this.overlay
      if (!overlay || !e.composedPath().includes(overlay)) this.close()
    }
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      // Walk back one level at a time (design D1): confirming → open → closed.
      e.preventDefault()
      if (this.phase === 'confirming') {
        this.phase = 'open'
        this.focusFirstItem()
      } else if (this.phase === 'open') {
        this.close()
      }
      // busy: the request is already in flight; the result closes the menu.
    }
    window.addEventListener('scroll', onScroll, true)
    window.addEventListener('resize', onResize)
    document.addEventListener('pointerdown', onPointerDown, true)
    document.addEventListener('keydown', onKeyDown, true)
    this.detachCloseListeners = () => {
      window.removeEventListener('scroll', onScroll, true)
      window.removeEventListener('resize', onResize)
      document.removeEventListener('pointerdown', onPointerDown, true)
      document.removeEventListener('keydown', onKeyDown, true)
    }
  }

  private get overlay(): HTMLElement | null {
    return this.renderRoot.querySelector('.overlay')
  }

  private itemButtons(): HTMLButtonElement[] {
    return [...this.renderRoot.querySelectorAll<HTMLButtonElement>('.menu-item[data-kind]')]
  }

  private focusFirstItem(): void {
    // Focus goes to the first *enabled* item; the DOM query runs after the
    // phase render via updateComplete in the caller paths.
    this.updateComplete.then(() => {
      this.itemButtons().find((b) => !b.disabled)?.focus()
    })
  }

  protected updated(changed: Map<string, unknown>): void {
    // Clamp the fixed overlay into the viewport once it has real dimensions
    // (a contextmenu near the right/bottom edge must not overflow).
    if (
      changed.has('posX') ||
      changed.has('posY') ||
      changed.has('phase') ||
      changed.has('program')
    ) {
      this.clampPosition()
    }
  }

  private clampPosition(): void {
    const overlay = this.overlay
    if (!overlay || this.phase === 'closed') return
    const rect = overlay.getBoundingClientRect()
    if (rect.width === 0 && rect.height === 0) return
    const margin = 8
    const maxX = Math.max(margin, window.innerWidth - rect.width - margin)
    const maxY = Math.max(margin, window.innerHeight - rect.height - margin)
    const clampedX = Math.min(Math.max(margin, this.posX), maxX)
    const clampedY = Math.min(Math.max(margin, this.posY), maxY)
    if (clampedX !== this.posX || clampedY !== this.posY) {
      this.posX = clampedX
      this.posY = clampedY
    }
  }

  private onItemKeydown = (e: KeyboardEvent): void => {
    // Roving focus among the menu items (webui-ui: 可访问性 — 方向键循环);
    // Enter/Space activate the focused item. Both are handled here (with
    // preventDefault) instead of relying on the browser's button synthesis,
    // so the keyboard path is identical everywhere and never double-fires.
    if (e.key === 'Enter' || e.key === ' ') {
      const target = e.target as HTMLElement | null
      if (target?.matches('.menu-item[data-kind]')) {
        e.preventDefault()
        target.click()
      }
      return
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'Home' && e.key !== 'End') return
    const items = this.itemButtons().filter((b) => !b.disabled)
    if (items.length === 0) return
    e.preventDefault()
    // document.activeElement retargets to the host for shadow content, so
    // resolve the focused item through our own shadow root.
    const root = this.renderRoot as ShadowRoot
    const current = items.indexOf((root.activeElement ?? null) as HTMLButtonElement)
    let next: number
    switch (e.key) {
      case 'ArrowDown':
        next = (current + 1) % items.length
        break
      case 'ArrowUp':
        next = current <= 0 ? items.length - 1 : current - 1
        break
      case 'Home':
        next = 0
        break
      default:
        next = items.length - 1
    }
    items[next]?.focus()
  }

  private choose(action: ProgramAction): void {
    if (this.phase === 'busy') return
    if (action === 'start') {
      // Loss-free: selected = executed (webui-ui: 启动免确认直接执行).
      void this.run('start')
      return
    }
    this.confirmKind = action
    this.phase = 'confirming'
    this.updateComplete.then(() => {
      this.renderRoot.querySelector<HTMLButtonElement>('.confirm-action')?.focus()
    })
  }

  private cancelConfirm(): void {
    if (this.phase !== 'confirming') return
    // Zero side effects: back to the item list, nothing was sent.
    this.phase = 'open'
    this.focusFirstItem()
  }

  private async run(action: ProgramAction): Promise<void> {
    const program = this.program
    if (!program || this.phase === 'busy') return
    const label = ACTION_LABEL[action]
    this.phase = 'busy'
    try {
      const text = await this.store.programAction(program.name, action)
      this.dispatchResult({
        ok: true,
        text: text || `${label}已提交`,
        program: program.name,
        action,
      })
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.dispatchResult({ ok: false, text: `${label}失败：${msg}`, program: program.name, action })
    } finally {
      // The request has settled: the menu closes and the surface shows the
      // result (design D2 — feedback rides the surface's own channel).
      this.close()
    }
  }

  private dispatchResult(detail: ProgramActionResultDetail): void {
    this.dispatchEvent(
      new CustomEvent<ProgramActionResultDetail>('program-action-result', {
        detail,
        bubbles: true,
        composed: true,
      }),
    )
  }

  render() {
    if (this.phase === 'closed' || !this.program) return nothing
    const p = this.program
    const busy = this.phase === 'busy'
    if (this.phase === 'confirming') {
      const label = ACTION_LABEL[this.confirmKind]
      return html`<div
        class="overlay"
        role="alertdialog"
        aria-label=${`确认对 ${p.name} 执行${label}`}
        style=${`left:${this.posX}px;top:${this.posY}px`}
      >
        <div class="confirm">
          <span class="confirm-text">
            确认对 <strong>${p.name}</strong> 执行${label}？
          </span>
          <div class="confirm-buttons">
            <button
              class="confirm-action"
              data-kind=${this.confirmKind}
              ?disabled=${busy}
              @click=${() => void this.run(this.confirmKind)}
            >
              ${label}
            </button>
            <button class="confirm-cancel" ?disabled=${busy} @click=${this.cancelConfirm}>
              取消
            </button>
          </div>
        </div>
      </div>`
    }
    const items = actionsFor(p.state)
    return html`<div
      class="overlay"
      role="menu"
      aria-label=${`程序 ${p.name} 操作`}
      style=${`left:${this.posX}px;top:${this.posY}px`}
      @keydown=${this.onItemKeydown}
    >
      ${items.map(
        (action) => html`<button
          class="menu-item"
          role="menuitem"
          data-kind=${action}
          data-program=${p.name}
          aria-label=${`${ACTION_LABEL[action]} ${p.name}`}
          title=${`${ACTION_LABEL[action]} ${p.name}`}
          ?disabled=${busy}
          @click=${() => this.choose(action)}
        >
          <span class="icon" aria-hidden="true">${ACTION_ICON[action]}</span>
          <span>${ACTION_LABEL[action]}</span>
        </button>`,
      )}
    </div>`
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-action-menu': AkActionMenu
  }
}
