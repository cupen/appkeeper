/**
 * Shared program table (webui-ui: 全局程序总览 / App 概况 / 列表行快捷操作).
 *
 * Presentational and dependency-light: the caller supplies the rows (already
 * scoped/sorted), whether the app column applies and which programs carry a
 * pending change; the component owns the column layout, number alignment,
 * fatal styling, the log-rate window switch in its title bar and the row-tail
 * ⋯ trigger that opens the shared `ak-action-menu` (row right-click opens the
 * same menu — webui-ui: 列表行快捷操作).
 *
 * One component for both places is the point (design D6): column widths,
 * placeholders and status rendering cannot drift between the dashboard and
 * an app's overview.
 *
 * Hooks kept deliberately stable for e2e: rows carry `data-app`,
 * `data-program`, `data-status` and `data-fatal`; the pending marker keeps
 * the `.changed-mark` class; the menu trigger keeps `.op-trigger` +
 * `data-program`.
 */

import { LitElement, css, html, nothing } from 'lit'
import { customElement, property, query, state } from 'lit/decorators.js'
import type { ProgramInfo } from '../lib/types.js'
import {
  formatBytes,
  formatPercent,
  formatRate,
  formatUptime,
  formatWindow,
  stateLabel,
} from '../lib/format.js'
import {
  RATE_WINDOW_CHOICES_S,
  loadRateWindowS,
  saveRateWindowS,
  type RateWindowS,
} from '../lib/prefs.js'
import { consoleStore, type ConsoleStore } from '../lib/store.js'
import {
  AkActionMenu,
  actionsFor,
  type MenuToggleDetail,
  type ProgramActionResultDetail,
} from './action-menu.js'
import './action-menu.js'
import './status-badge.js'

export interface ProgramSelectDetail {
  app: string
  program: string
}

@customElement('ak-program-table')
export class AkProgramTable extends LitElement {
  /** Rows to render, in the order the caller wants them shown. */
  @property({ type: Array }) programs: ProgramInfo[] = []
  /** The app column is redundant inside an app view; the dashboard shows it. */
  @property({ type: Boolean }) showApp = false
  /** Programs carrying a pending (not-yet-applied) config change. */
  @property({ type: Array }) pending: string[] = []
  /** Row-tail quick actions; views may turn them off for a read-only table. */
  @property({ type: Boolean }) actions = true
  @property() heading = ''
  @property() caption = ''
  @property() emptyText = '暂无程序'

  /** Injectable for tests; defaults to the shared singleton. */
  @property({ attribute: false }) store: ConsoleStore = consoleStore

  /** Rate window lives in this table's title bar (design D5). */
  @state() private rateWindow: RateWindowS = loadRateWindowS()

  /** The shared floating action menu rendered at the end of the template. */
  @query('ak-action-menu') private menu!: AkActionMenu
  @state() private feedback: { ok: boolean; text: string } | null = null

  /**
   * The preference is window-wide, but the dashboard renders one table per
   * app group. Every table exposes the switch, so a change in one must move
   * the others (webui-ui: 切换即时对全部列表生效).
   */
  private onExternalRate = (e: Event): void => {
    const value = (e as CustomEvent<RateWindowS>).detail
    if (!RATE_WINDOW_CHOICES_S.includes(value)) return
    this.rateWindow = value
  }

  static styles = css`
    :host {
      display: block;
      min-width: 0;
    }
    .panel {
      background: var(--ak-surface);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-lg);
      box-shadow: var(--ak-shadow-1);
      overflow: hidden;
      min-width: 0;
    }
    .table-head {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: var(--ak-space-3);
      flex-wrap: wrap;
      padding: var(--ak-space-3) var(--ak-space-4);
      border-bottom: 1px solid var(--ak-border);
    }
    .titles {
      display: flex;
      align-items: baseline;
      gap: var(--ak-space-2);
      min-width: 0;
    }
    .table-title {
      margin: 0;
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: var(--ak-fs-lg);
      font-weight: 600;
      color: var(--ak-text-bright);
    }
    .caption {
      flex: 0 0 auto;
      color: var(--ak-text-faint);
      font-size: var(--ak-fs-xs);
      font-variant-numeric: tabular-nums;
    }
    .tools {
      display: flex;
      align-items: center;
      gap: var(--ak-space-3);
      margin-left: auto;
      flex-wrap: wrap;
    }
    .rate {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
      font-size: var(--ak-fs-xs);
      color: var(--ak-text-dim);
      white-space: nowrap;
    }
    .rate select {
      height: var(--ak-control-h);
      padding: 0 var(--ak-space-2);
      box-sizing: border-box;
      background: var(--ak-surface-2);
      color: var(--ak-text);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-sm);
      font: inherit;
    }
    .table-feedback {
      display: flex;
      align-items: flex-start;
      gap: var(--ak-space-2);
      padding: var(--ak-space-2) var(--ak-space-4);
      font-size: var(--ak-fs-sm);
      color: var(--ak-text-dim);
      border-bottom: 1px solid var(--ak-border);
      white-space: pre-wrap;
    }
    .table-feedback[data-error='true'] {
      color: var(--ak-status-failed);
      background: var(--ak-status-failed-bg);
    }
    /* Wide tables scroll inside their own container, never the page
       (webui-visual-system: 长文本与溢出). */
    .table-scroll {
      overflow-x: auto;
      min-width: 0;
    }
    table {
      width: 100%;
      min-width: 680px;
      border-collapse: collapse;
      font-size: var(--ak-fs-sm);
    }
    th,
    td {
      text-align: left;
      padding: var(--ak-space-2) var(--ak-space-3);
      border-bottom: 1px solid var(--ak-border);
      white-space: nowrap;
      vertical-align: middle;
    }
    th {
      font-size: var(--ak-fs-xs);
      font-weight: 600;
      letter-spacing: 0.06em;
      text-transform: uppercase;
      color: var(--ak-text-faint);
      background: var(--ak-surface-2);
    }
    tbody tr:last-child td {
      border-bottom: none;
    }
    tbody tr {
      cursor: pointer;
      transition: background var(--ak-dur) var(--ak-ease);
    }
    tbody tr:hover {
      background: var(--ak-surface-2);
    }
    tbody tr:focus-within {
      background: var(--ak-surface-2);
    }
    tr[data-fatal='true'] {
      background: var(--ak-status-failed-bg);
      box-shadow: inset 3px 0 0 var(--ak-status-failed);
    }
    tr[data-fatal='true']:hover {
      background: color-mix(in srgb, var(--ak-status-failed-bg) 60%, var(--ak-surface-2));
    }
    td.num,
    th.num {
      text-align: right;
      font-variant-numeric: tabular-nums;
    }
    .name-cell {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
      min-width: 0;
    }
    .name-cell a {
      color: var(--ak-text-bright);
      font-weight: 600;
      text-decoration: none;
      max-width: 32ch;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .name-cell a:hover {
      color: var(--ak-accent-hover);
    }
    .app-cell {
      max-width: 20ch;
      overflow: hidden;
      text-overflow: ellipsis;
      color: var(--ak-text-dim);
    }
    /* Status column: fixed floor so a word swap never re-flows the table. */
    .status {
      min-width: 168px;
      white-space: normal;
    }
    .status .note {
      display: block;
      margin-top: var(--ak-space-1);
      color: var(--ak-text-faint);
      font-size: var(--ak-fs-xs);
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
      max-width: 32ch;
    }
    .changed-mark {
      flex: 0 0 auto;
      padding: 1px var(--ak-space-2);
      border-radius: var(--ak-radius-full);
      font-size: var(--ak-fs-xs);
      font-weight: 700;
      letter-spacing: 0.04em;
      background: color-mix(in srgb, var(--ak-accent-strong) 22%, transparent);
      color: var(--ak-text-bright);
    }
    /* Reserved action column: the trigger is always in the layout, only its
       visibility follows hover/focus, so rows never widen on hover. */
    th.ops,
    td.ops {
      width: 56px;
      padding-left: 0;
      text-align: right;
    }
    .row-actions {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-1);
      opacity: 0;
      transition: opacity var(--ak-dur) var(--ak-ease);
    }
    tbody tr:hover .row-actions,
    tbody tr:focus-within .row-actions {
      opacity: 1;
    }
    @media (hover: none) {
      .row-actions {
        opacity: 1;
      }
    }
    /* The single ⋯ trigger (webui-ui: 列表行快捷操作) — it opens the shared
       action menu; the destructive confirm lives inside the menu now. */
    .op-trigger {
      height: var(--ak-control-h);
      min-width: 32px;
      padding: 0 var(--ak-space-2);
      box-sizing: border-box;
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-sm);
      background: var(--ak-surface-2);
      color: var(--ak-text);
      font: inherit;
      font-size: var(--ak-fs-md);
      line-height: 1;
      letter-spacing: 0.08em;
      cursor: pointer;
    }
    .op-trigger:hover {
      border-color: var(--ak-border-strong);
      color: var(--ak-text-bright);
    }
    .op-trigger:focus-visible {
      outline: var(--ak-focus-ring, 1px solid var(--ak-accent-strong));
      outline-offset: 1px;
    }
    .empty-cell {
      padding: var(--ak-space-8) var(--ak-space-4);
      text-align: center;
      color: var(--ak-text-faint);
      white-space: normal;
    }
  `

  private get columns(): number {
    // name + (app) + status + restart + uptime + cpu + mem + out + err + ops
    return this.showApp ? 10 : 9
  }

  /** Log rates only exist while the process runs; stopped rows read "—". */
  private rateOf(p: ProgramInfo, stream: 'out' | 'err'): number | null {
    if (p.state !== 'running' && p.state !== 'starting') return null
    const r = p.log_rate[stream]
    switch (this.rateWindow) {
      case 1:
        return r.w1
      case 60:
        return r.w60
      case 300:
        return r.w300
      default:
        return r.w10
    }
  }

  /** Uptime is only meaningful while the process is actually up. */
  private uptimeOf(p: ProgramInfo): string {
    const live = p.state === 'running' || p.state === 'starting'
    return live ? formatUptime(p.uptime_secs) : '—'
  }

  /**
   * Backoff delay as secondary text beside the badge (webui-ui: 状态展示 /
   * backoff 状态可见). Kept out of the pill so the status column's width does
   * not jump between states; a missing/zero value renders nothing.
   */
  private backoffNote(p: ProgramInfo): string | null {
    if (p.state !== 'backoff') return null
    if (!Number.isFinite(p.restart_backoff) || p.restart_backoff <= 0) return null
    return `${p.restart_backoff}s 后重启`
  }

  private detailPath(app: string, program: string): string {
    return `/app/${encodeURIComponent(app)}/program/${encodeURIComponent(program)}`
  }

  private emitSelect(p: ProgramInfo): void {
    this.dispatchEvent(
      new CustomEvent<ProgramSelectDetail>('program-select', {
        detail: { app: p.app, program: p.name },
        bubbles: true,
        composed: true,
      }),
    )
  }

  /**
   * The menu's document-level pointerdown capture closes it before the click
   * dispatches, so `isOpen` is already false by the time a row click arrives.
   * A click within the recent-close window is the dismissal gesture itself and
   * must not ALSO navigate (design D2: 菜单打开时防伪事件穿透).
   */
  private menuClickThrough(): boolean {
    return !!this.menu && (this.menu.isOpen || this.menu.wasRecentlyClosed())
  }

  private onNameClick(e: MouseEvent, p: ProgramInfo): void {
    // Modified clicks keep the browser's new-tab/window behaviour.
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
    if (this.menuClickThrough()) return
    e.preventDefault()
    e.stopPropagation()
    this.emitSelect(p)
  }

  private onRowClick(e: MouseEvent, p: ProgramInfo): void {
    if (e.defaultPrevented) return
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
    const target = e.target as Element | null
    // Interactive children own their clicks (and must never bounce the user
    // into a detail page, webui-ui: 快捷操作不触发行导航).
    if (target?.closest('a, button, select, input')) return
    if (this.menuClickThrough()) return
    this.emitSelect(p)
  }

  /**
   * Row right-click opens the same menu the ⋯ button does
   * (webui-ui: 右键与 ⋯ 打开同一菜单). The native context menu is suppressed
   * for the whole row; a state with no actions (stopping) has no menu to show.
   */
  private onRowContextmenu(e: MouseEvent, p: ProgramInfo): void {
    if (!this.actions) return
    e.preventDefault()
    if (actionsFor(p.state).length === 0) return
    this.menu?.openFor(p, e.clientX, e.clientY)
  }

  private onTriggerClick(e: MouseEvent, p: ProgramInfo): void {
    // Anchor on the trigger button so the menu opens where the pointer is not
    // required to be; focus returns here on close (design D1).
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect()
    this.menu?.openFor(p, rect.left, rect.bottom + 4, e.currentTarget as HTMLElement)
  }

  /** The menu marks its row so a dismiss-click cannot double-fire a select. */
  private onMenuToggle = (e: Event): void => {
    const { open, program } = (e as CustomEvent<MenuToggleDetail>).detail
    for (const tr of this.renderRoot.querySelectorAll<HTMLElement>('tbody tr[data-program]')) {
      if (tr.dataset.program === program) {
        if (open) tr.setAttribute('data-menu-open', 'true')
        else tr.removeAttribute('data-menu-open')
      }
    }
  }

  /** The surface owns feedback visuals (webui-ui: 菜单操作失败有反馈). */
  private onActionResult = (e: Event): void => {
    const { ok, text } = (e as CustomEvent<ProgramActionResultDetail>).detail
    this.feedback = { ok, text }
  }

  private onRateChange(e: Event): void {
    const value = Number((e.target as HTMLSelectElement).value) as RateWindowS
    if (!RATE_WINDOW_CHOICES_S.includes(value)) return
    this.rateWindow = value
    saveRateWindowS(value)
    window.dispatchEvent(new CustomEvent<RateWindowS>('ak-rate-window', { detail: value }))
  }

  connectedCallback(): void {
    super.connectedCallback()
    window.addEventListener('ak-rate-window', this.onExternalRate)
    this.rateWindow = loadRateWindowS()
  }

  disconnectedCallback(): void {
    window.removeEventListener('ak-rate-window', this.onExternalRate)
    super.disconnectedCallback()
  }

  private renderActions(p: ProgramInfo) {
    if (!this.actions) return nothing
    // No actions for this state (stopping): no trigger, no menu to open.
    if (actionsFor(p.state).length === 0) return nothing
    return html`<span class="row-actions">
      <button
        class="op-trigger"
        data-program=${p.name}
        aria-haspopup="menu"
        aria-label=${`程序 ${p.name} 操作菜单`}
        title="操作菜单"
        @click=${(e: MouseEvent) => this.onTriggerClick(e, p)}
      >
        ⋯
      </button>
    </span>`
  }

  render() {
    const pending = new Set(this.pending)
    return html`<section class="panel">
      <div class="table-head">
        <div class="titles">
          ${this.heading
            ? html`<h2 class="table-title" title=${this.heading}>${this.heading}</h2>`
            : nothing}
          <span class="caption">${this.caption || `${this.programs.length} 个程序`}</span>
        </div>
        <div class="tools">
          <label class="rate">
            速率窗口
            <select
              class="rate-select"
              aria-label="日志速率窗口"
              .value=${String(this.rateWindow)}
              @change=${this.onRateChange}
            >
              ${RATE_WINDOW_CHOICES_S.map(
                (s) => html`<option value=${s} ?selected=${s === this.rateWindow}>
                  ${formatWindow(s)}
                </option>`,
              )}
            </select>
          </label>
        </div>
      </div>
      ${this.feedback
        ? html`<div
            class="table-feedback"
            data-error=${!this.feedback.ok}
            role=${this.feedback.ok ? 'status' : 'alert'}
          >
            ${this.feedback.text}
          </div>`
        : nothing}
      <div class="table-scroll">
        <table>
          <thead>
            <tr>
              <th>程序</th>
              ${this.showApp ? html`<th>应用</th>` : nothing}
              <th>状态</th>
              <th class="num">重启</th>
              <th class="num">uptime</th>
              <th class="num">CPU</th>
              <th class="num">内存</th>
              <th class="num">stdout/s</th>
              <th class="num">stderr/s</th>
              <th class="ops" aria-label="快捷操作"></th>
            </tr>
          </thead>
          <tbody>
            ${this.programs.length === 0
              ? html`<tr class="empty-row">
                  <td class="empty-cell" colspan=${this.columns}>${this.emptyText}</td>
                </tr>`
              : this.programs.map(
                  (p) => html`<tr
                    data-app=${p.app}
                    data-program=${p.name}
                    data-status=${p.state}
                    data-fatal=${p.state === 'fatal'}
                    @click=${(e: MouseEvent) => this.onRowClick(e, p)}
                    @contextmenu=${(e: MouseEvent) => this.onRowContextmenu(e, p)}
                  >
                    <td class="name">
                      <span class="name-cell">
                        <a
                          href=${this.detailPath(p.app, p.name)}
                          title=${p.name}
                          @click=${(e: MouseEvent) => this.onNameClick(e, p)}
                          >${p.name}</a
                        >
                        ${pending.has(p.name)
                          ? html`<span class="changed-mark">待应用</span>`
                          : nothing}
                      </span>
                    </td>
                    ${this.showApp
                      ? html`<td class="app-cell" title=${p.app}>${p.app}</td>`
                      : nothing}
                    <td class="status">
                      <ak-status-badge
                        slug=${p.state}
                        label=${stateLabel(p.state)}
                      ></ak-status-badge>
                      ${this.backoffNote(p)
                        ? html`<span class="note backoff-note" title=${this.backoffNote(p)!}
                            >${this.backoffNote(p)}</span
                          >`
                        : nothing}
                      ${p.wait_reason
                        ? html`<span class="note" title=${p.wait_reason}>${p.wait_reason}</span>`
                        : nothing}
                      ${p.fatal_reason
                        ? html`<span class="note" title=${p.fatal_reason}>${p.fatal_reason}</span>`
                        : nothing}
                    </td>
                    <td class="num">${p.total_exits}</td>
                    <td class="num">${this.uptimeOf(p)}</td>
                    <td class="num">${formatPercent(p.cpu_percent)}</td>
                    <td class="num">${formatBytes(p.mem_bytes)}</td>
                    <td class="num">${formatRate(this.rateOf(p, 'out'))}</td>
                    <td class="num">${formatRate(this.rateOf(p, 'err'))}</td>
                    <td class="ops">${this.renderActions(p)}</td>
                  </tr>`,
                )}
          </tbody>
        </table>
      </div>
      <ak-action-menu
        .store=${this.store}
        @menu-toggle=${this.onMenuToggle}
        @program-action-result=${this.onActionResult}
      ></ak-action-menu>
    </section>`
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-program-table': AkProgramTable
  }
}
