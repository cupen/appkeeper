/**
 * App overview (webui-ui: App 概况): the app's program summary table —
 * name, status, restarts, uptime, CPU, memory, log line rate — plus the
 * rate-window switch (1s/10s/1m/5m, shared by every rate column) and links
 * into each program's detail. Not-running programs show placeholders, never
 * fake zeros; fatal rows are highlighted.
 */

import { LitElement, css, html } from 'lit'
import { customElement, property, state } from 'lit/decorators.js'
import { nothing } from 'lit'
import { ApiError } from '../lib/api.js'
import { ConsoleController, consoleStore, type ConsoleStore } from '../lib/store.js'
import type { ProgramInfo } from '../lib/types.js'
import {
  formatBytes,
  formatPercent,
  formatRate,
  formatUptime,
  formatWindow,
  stateGlyph,
} from '../lib/format.js'
import {
  RATE_WINDOW_CHOICES_S,
  loadRateWindowS,
  saveRateWindowS,
  type RateWindowS,
} from '../lib/prefs.js'
import { navigate } from '../router.js'

@customElement('ak-app-overview')
export class AkAppOverview extends LitElement {
  @property() app = ''

  /** Injectable for tests; defaults to the shared singleton. */
  @property({ attribute: false }) store: ConsoleStore = consoleStore

  private console: ConsoleController | null = null

  connectedCallback(): void {
    super.connectedCallback()
    this.console = new ConsoleController(this, this.store)
  }

  @state() private rateWindow: RateWindowS = loadRateWindowS()
  @state() private applying = false
  @state() private applyFeedback: { ok: boolean; text: string } | null = null
  @state() private removing = false
  @state() private removeFeedback: { ok: boolean; text: string } | null = null

  static styles = css`
    :host {
      flex: 1;
      min-height: 0;
      overflow-y: auto;
      display: flex;
      flex-direction: column;
      gap: var(--ak-space-4);
      padding: var(--ak-space-4) var(--ak-space-5);
      box-sizing: border-box;
    }
    header {
      display: flex;
      align-items: baseline;
      gap: var(--ak-space-3);
    }
    h1 {
      margin: 0;
      font-size: 1.25rem;
      color: var(--ak-text-bright);
    }
    .sub {
      color: var(--ak-text-faint);
      font-size: 0.85rem;
    }
    .window-switch {
      margin-left: auto;
      display: inline-flex;
      align-items: center;
      gap: 6px;
      font-size: 0.8rem;
      color: var(--ak-text-dim);
    }
    .window-switch select {
      background: var(--ak-surface-2);
      color: var(--ak-text);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-sm, 6px);
      font: inherit;
      padding: 3px 6px;
    }
    .remove {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
    }
    .remove button {
      font: inherit;
      font-size: 0.8rem;
      padding: 4px 12px;
      border-radius: var(--ak-radius-sm, 6px);
      border: 1px solid var(--ak-status-failed-border, #b91c1c);
      background: transparent;
      color: var(--ak-status-failed, #ef4444);
      cursor: pointer;
    }
    .remove button:hover:not(:disabled) {
      background: color-mix(in srgb, var(--ak-status-failed, #ef4444) 12%, transparent);
    }
    .remove button:disabled {
      opacity: 0.5;
      cursor: not-allowed;
    }
    .remove-strip {
      display: flex;
      align-items: flex-start;
      gap: var(--ak-space-2);
      padding: var(--ak-space-2) var(--ak-space-4);
      border-radius: var(--ak-radius-lg, 10px);
      border: 1px solid var(--ak-border);
      background: var(--ak-surface);
      font-size: 0.82rem;
      color: var(--ak-text-dim);
      white-space: pre-wrap;
    }
    .remove-strip[data-error='true'] {
      border-color: var(--ak-status-failed, #ef4444);
      background: var(--ak-status-failed-bg);
      color: var(--ak-text);
    }
    .remove-strip[data-ok='true'] {
      border-color: var(--ak-status-done-border, #15803d);
      background: var(--ak-status-done-bg);
      color: var(--ak-status-done, #22c55e);
    }
    table {
      width: 100%;
      border-collapse: collapse;
      background: var(--ak-surface);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-lg, 10px);
      overflow: hidden;
      font-size: 0.86rem;
    }
    th,
    td {
      text-align: left;
      padding: 9px 14px;
      border-bottom: 1px solid var(--ak-border);
      font-variant-numeric: tabular-nums;
      white-space: nowrap;
    }
    th {
      font-size: 0.72rem;
      letter-spacing: 0.06em;
      text-transform: uppercase;
      color: var(--ak-text-faint);
      font-weight: 600;
      background: var(--ak-surface-2);
    }
    tr:last-child td {
      border-bottom: none;
    }
    tbody tr {
      cursor: pointer;
      transition: background var(--ak-dur, 150ms) var(--ak-ease, ease);
    }
    tbody tr:hover {
      background: var(--ak-surface-2);
    }
    tr[data-fatal='true'] {
      background: var(--ak-status-failed-bg);
      box-shadow: inset 3px 0 0 var(--ak-status-failed);
    }
    tr[data-fatal='true']:hover {
      background: color-mix(in srgb, var(--ak-status-failed-bg) 60%, var(--ak-surface-2));
    }
    td.num {
      text-align: right;
    }
    .name-cell {
      display: inline-flex;
      align-items: center;
      gap: 8px;
      color: var(--ak-text-bright);
      font-weight: 600;
    }
    .name-cell .glyph {
      color: var(--ak-text-faint);
    }
    .prog-count {
      color: var(--ak-text-faint);
      font-size: 0.9rem;
    }
    .apply-strip {
      display: flex;
      align-items: center;
      gap: var(--ak-space-3);
      padding: var(--ak-space-2) var(--ak-space-4);
      background: color-mix(in srgb, var(--ak-accent-strong, #6366f1) 12%, var(--ak-surface));
      border: 1px solid color-mix(in srgb, var(--ak-accent-strong, #6366f1) 35%, var(--ak-border));
      border-radius: var(--ak-radius-lg, 10px);
      font-size: 0.84rem;
    }
    .apply-strip .count {
      color: var(--ak-text-bright);
      font-weight: 600;
    }
    .apply-strip button {
      margin-left: auto;
      border: 1px solid transparent;
      background: var(--ak-accent-strong, #6366f1);
      color: var(--ak-accent-ink, #fff);
      font: inherit;
      font-weight: 600;
      padding: 4px 14px;
      border-radius: var(--ak-radius-sm, 6px);
      cursor: pointer;
    }
    .apply-strip button:disabled {
      opacity: 0.55;
      cursor: default;
    }
    .apply-strip .feedback {
      color: var(--ak-text-dim);
      white-space: pre-wrap;
      max-width: 52ch;
      max-height: 8em;
      overflow-y: auto;
      font-size: 0.78rem;
    }
    .apply-strip .feedback[data-error='true'] {
      color: var(--ak-status-failed, #ef4444);
    }
    .changed-mark {
      margin-left: 6px;
      padding: 1px 7px;
      border-radius: 999px;
      font-size: 0.68rem;
      font-weight: 700;
      letter-spacing: 0.04em;
      background: color-mix(in srgb, var(--ak-accent-strong, #6366f1) 22%, transparent);
      color: var(--ak-text-bright);
    }
    .empty {
      padding: var(--ak-space-6);
      text-align: center;
      color: var(--ak-text-faint);
    }
  `

  private programsOf(app: string): ProgramInfo[] {
    return this.console?.snapshot.doc?.programs.filter((p) => p.app === app) ?? []
  }

  private rateOf(p: ProgramInfo, stream: 'out' | 'err'): number {
    const w = this.rateWindow
    const r = p.log_rate[stream]
    switch (w) {
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

  private pendingSet(): Set<string> {
    return new Set(
      (this.console?.snapshot.doc?.pending.programs ?? [])
        .filter((x) => x.app === this.app)
        .map((x) => x.program),
    )
  }

  private async onApply(): Promise<void> {
    if (this.applying) return
    const count = this.pendingSet().size
    if (
      !window.confirm(
        `应用 app「${this.app}」的待应用变更（${count} 个程序）？\n变更的程序停止后以新定义重建；手动停止的保持停止。\n不影响其他应用。`,
      )
    ) {
      return
    }
    this.applying = true
    this.applyFeedback = null
    try {
      const r = await this.store.apply({ app: this.app })
      this.applyFeedback = { ok: true, text: r }
    } catch (e) {
      this.applyFeedback = { ok: false, text: String(e instanceof Error ? e.message : e) }
    } finally {
      this.applying = false
    }
  }

  private onWindowChange(e: Event): void {
    this.rateWindow = Number((e.target as HTMLSelectElement).value) as RateWindowS
    saveRateWindowS(this.rateWindow)
  }

  /**
   * Remove the whole app (guard-remove-until-stopped). Confirmation states
   * the deletion boundary honestly: the registration goes away, an
   * `app_dir`-generated config file is deleted with it, an external
   * deployment config body is kept — and nothing is stopped for the user.
   * On success the app is gone immediately (no apply); a 409 keeps the page
   * and shows the daemon's blocking programs + the stop-first remedy.
   */
  private async onRemove(): Promise<void> {
    if (this.removing) return
    const programs = this.programsOf(this.app)
    const live = programs.filter(
      (p) => p.state === 'running' || p.state === 'starting' || p.state === 'backoff',
    )
    const liveNote = live.length
      ? `\n注意：当前仍有 ${live.length} 个程序未停止（${live
          .map((p) => `${p.name} ${p.state}`)
          .join(', ')}），移除会被拒绝；请先执行停止。`
      : ''
    if (
      !window.confirm(
        `移除 app「${this.app}」？\n` +
          '将删除其注册记录；若配置位于 app_dir 内且由脚手架生成，该配置文件会一并删除；\n' +
          '外部部署配置本体保留，日志文件不受影响。\n' +
          '移除不会自动停止任何程序，也不影响其他应用。' +
          liveNote,
      )
    ) {
      return
    }
    this.removing = true
    this.removeFeedback = null
    try {
      const r = await this.store.removeApp(this.app)
      const text = r.result
      this.removeFeedback = { ok: true, text }
      // The app no longer exists and this view is about to unmount, so carry
      // the success message across the navigation (the localized notice said
      // "no apply needed") and land on a still-resolving overview.
      this.store.setFlash(text, true)
      navigate(this.fallbackPath())
    } catch (e) {
      const msg = e instanceof ApiError || e instanceof Error ? e.message : String(e)
      // The daemon's own words already name the blockers and `ak stop <app>`;
      // the page (and its navigation entry) stays.
      this.removeFeedback = { ok: false, text: msg }
    } finally {
      this.removing = false
    }
  }

  /** Where to go once this app is gone: another app's overview, else the dashboard. */
  private fallbackPath(): string {
    const others = [
      ...new Set(
        (this.console?.snapshot.doc?.programs ?? [])
          .map((p) => p.app)
          .filter((a) => a !== this.app),
      ),
    ].sort()
    const next = others[0]
    return next ? `/app/${encodeURIComponent(next)}` : '/'
  }

  private openDetail(program: string): void {
    navigate(`/app/${encodeURIComponent(this.app)}/program/${encodeURIComponent(program)}`)
  }

  render() {
    const programs = this.programsOf(this.app)
    const pendingSet = this.pendingSet()
    return html`
      <header>
        <h1>▤ ${this.app}</h1>
        <span class="sub">${programs.length} 个程序</span>
        <label class="window-switch">
          速率窗口
          <select .value=${String(this.rateWindow)} @change=${this.onWindowChange}>
            ${RATE_WINDOW_CHOICES_S.map(
              (s) => html`<option value=${s} ?selected=${s === this.rateWindow}>${formatWindow(s)}</option>`,
            )}
          </select>
        </label>
        <span class="remove">
          <button
            data-kind="remove"
            ?disabled=${this.removing}
            @click=${() => void this.onRemove()}
            title="移除整个应用（不会自动停止程序；运行中的程序会阻止移除）"
          >
            ${this.removing ? '移除中…' : '移除应用'}
          </button>
        </span>
      </header>
      ${this.removeFeedback
        ? html`<div
            class="remove-strip"
            data-ok=${this.removeFeedback.ok}
            data-error=${!this.removeFeedback.ok}
            role=${this.removeFeedback.ok ? 'status' : 'alert'}
            aria-label="移除结果"
          >
            ${this.removeFeedback.text}
          </div>`
        : nothing}
      ${pendingSet.size > 0 || this.applyFeedback
        ? html`<div class="apply-strip" role="alert">
            <span><span class="count">${pendingSet.size}</span> 个程序配置有变化（待应用）</span>
            <button ?disabled=${this.applying} @click=${() => void this.onApply()}>
              ${this.applying ? '应用中…' : 'Apply 此应用'}
            </button>
            ${this.applyFeedback
              ? html`<span
                  class="feedback"
                  data-error=${!this.applyFeedback.ok}
                  role="status"
                  >${this.applyFeedback.text}</span
                >`
              : nothing}
          </div>`
        : nothing}
      ${programs.length === 0
        ? html`<div class="empty">该应用没有声明任何程序</div>`
        : html`
            <table>
              <thead>
                <tr>
                  <th>程序</th>
                  <th>状态</th>
                  <th class="num">重启</th>
                  <th class="num">uptime</th>
                  <th class="num">CPU</th>
                  <th class="num">内存</th>
                  <th class="num">stdout 速率</th>
                  <th class="num">stderr 速率</th>
                </tr>
              </thead>
              <tbody>
                ${programs.map(
                  (p) => html`
                    <tr
                      data-fatal=${p.state === 'fatal'}
                      @click=${() => this.openDetail(p.name)}
                      title="查看 ${p.name} 详情"
                    >
                      <td>
                        <span class="name-cell"
                          ><span class="glyph" aria-hidden="true">${stateGlyph(p.state)}</span
                          >${p.name}</span
                        >${pendingSet.has(p.name) ? html`<span class="changed-mark">待应用</span>` : nothing}
                      </td>
                      <td>
                        <ak-status-badge slug=${p.state} label=${p.state} glyph=${stateGlyph(p.state)}
                        ></ak-status-badge>
                        ${p.wait_reason ? html`<span class="sub">　${p.wait_reason}</span>` : nothing}
                        ${p.fatal_reason ? html`<span class="sub">　${p.fatal_reason}</span>` : nothing}
                      </td>
                      <td class="num">${p.total_exits}</td>
                      <td class="num">${formatUptime(p.uptime_secs)}</td>
                      <td class="num">${formatPercent(p.cpu_percent)}</td>
                      <td class="num">${formatBytes(p.mem_bytes)}</td>
                      <td class="num">${formatRate(this.rateOf(p, 'out'))}</td>
                      <td class="num">${formatRate(this.rateOf(p, 'err'))}</td>
                    </tr>
                  `,
                )}
              </tbody>
            </table>
          `}
    `
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-app-overview': AkAppOverview
  }
}
