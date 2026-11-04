/**
 * Daemon overview bar (webui-ui: 守护进程概况): version, config source,
 * monitor interval, uptime, system CPU/memory — the page-level strip that
 * covers every app — plus the refresh-frequency selector (1/3/5/10s, default
 * 3s, persisted). The selector throttles LIST/METRIC display updates only;
 * log following is realtime regardless (webui-ui: 刷新频率控制).
 */

import { LitElement, css, html, nothing } from 'lit'
import { customElement, property, state } from 'lit/decorators.js'
import { ConsoleController, consoleStore, type ConsoleStore } from '../lib/store.js'
import type { PendingDoc } from '../lib/types.js'
import { formatBytes, formatPercent, formatUptime } from '../lib/format.js'
import { REFRESH_CHOICES_MS, loadRefreshMs, saveRefreshMs, type RefreshMs } from '../lib/prefs.js'

@customElement('ak-overview-bar')
export class AkOverviewBar extends LitElement {
  /** Injectable for tests; defaults to the shared singleton. */
  @property({ attribute: false }) store: ConsoleStore = consoleStore

  private console: ConsoleController | null = null

  private get consoleState() {
    return this.console?.snapshot ?? { doc: null, transport: 'connecting' as const }
  }

  @state() private refreshMs: RefreshMs = loadRefreshMs()
  @state() private applying = false
  @state() private applyResult: string | null = null
  @state() private applyError: string | null = null

  static styles = css`
    :host {
      display: flex;
      flex-direction: column;
      gap: var(--ak-space-2);
      /* No horizontal padding: ak-app main owns the single page gutter so
         this bar's left/right edges match every routed view. */
    }
    .pending {
      display: flex;
      align-items: center;
      flex-wrap: wrap;
      gap: var(--ak-space-2) var(--ak-space-3);
      padding: var(--ak-space-2) var(--ak-space-3);
      background: color-mix(in srgb, var(--ak-accent-strong, #6366f1) 12%, var(--ak-surface));
      border: 1px solid color-mix(in srgb, var(--ak-accent-strong, #6366f1) 35%, var(--ak-border));
      border-radius: var(--ak-radius-lg, 10px);
      font-size: var(--ak-fs-sm, 0.8rem);
      color: var(--ak-text);
    }
    .pending .badge {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
      font-weight: 600;
      color: var(--ak-text-bright);
    }
    .pending .count {
      display: inline-grid;
      place-items: center;
      min-width: 18px;
      height: 18px;
      padding: 0 var(--ak-space-1);
      border-radius: var(--ak-radius-full);
      background: var(--ak-accent-strong, #6366f1);
      color: var(--ak-accent-ink, #fff);
      font-size: var(--ak-fs-xs, 0.72rem);
      font-weight: 700;
    }
    .pending .detail {
      color: var(--ak-text-dim);
      max-width: 46ch;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .pending .actions {
      margin-left: auto;
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
    }
    .pending .restart-opt {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-1);
      color: var(--ak-text-dim);
      font-size: var(--ak-fs-xs, 0.72rem);
      cursor: pointer;
    }
    .pending button {
      height: var(--ak-control-h, 28px);
      padding: 0 var(--ak-space-3);
      border: 1px solid var(--ak-border);
      background: var(--ak-surface-2);
      color: var(--ak-text);
      font: inherit;
      font-size: var(--ak-fs-sm, 0.8rem);
      border-radius: var(--ak-radius-sm, 6px);
      cursor: pointer;
      box-sizing: border-box;
    }
    .pending button[data-primary='true'] {
      background: var(--ak-accent-strong, #6366f1);
      border-color: transparent;
      color: var(--ak-accent-ink, #fff);
      font-weight: 600;
    }
    .pending button:disabled {
      opacity: 0.55;
      cursor: default;
    }
    .apply-result {
      padding: var(--ak-space-2) var(--ak-space-3);
      font-size: var(--ak-fs-sm, 0.8rem);
      white-space: pre-wrap;
      background: var(--ak-surface);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-lg, 10px);
      color: var(--ak-text-dim);
      max-height: 12em;
      overflow-y: auto;
    }
    .apply-result[data-error='true'] {
      border-color: var(--ak-status-failed, #ef4444);
      color: var(--ak-text);
    }

    /* Daemon overview: an explicit grid, never flex-wrap. Fixed breakpoints:
     *   ≥1120px — one row of seven columns;
     *   720–1119px — two rows (version/config/uptime/monitor, then CPU/mem/refresh);
     *   ≤720px — two columns so nothing is pushed off-screen.
     * No margin-left:auto anywhere: a lone control cannot jump to a row's
     * right edge (webui-visual-system: 断点行为). */
    .bar {
      display: grid;
      grid-template-areas: 'version config uptime monitor cpu mem refresh';
      grid-template-columns:
        max-content minmax(0, 1fr) max-content max-content
        minmax(160px, 0.9fr) minmax(180px, 1fr) max-content;
      align-items: center;
      gap: var(--ak-space-2);
      padding: var(--ak-space-2) var(--ak-space-3);
      background: var(--ak-surface);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-lg, 10px);
      font-size: var(--ak-fs-sm, 0.8rem);
      color: var(--ak-text-dim);
    }
    @media (max-width: 1119.98px) {
      .bar {
        grid-template-areas:
          'version config uptime monitor refresh'
          'cpu cpu mem mem refresh';
        /* Only the config path stretches; every metric track stays at its
           natural width and the refresh control spans both rows. This keeps
           row 1 free of a wide empty hole while the CPU/memory meters get the
           width they need on row 2. */
        grid-template-columns: max-content minmax(0, 1fr) max-content max-content max-content;
      }
    }
    @media (max-width: 719.98px) {
      .bar {
        grid-template-areas:
          'version config'
          'uptime monitor'
          'cpu cpu'
          'mem mem'
          'refresh refresh';
        grid-template-columns: max-content minmax(0, 1fr);
      }
    }
    .item {
      display: flex;
      align-items: baseline;
      gap: var(--ak-space-1);
      min-width: 0;
      white-space: nowrap;
    }
    .version {
      grid-area: version;
    }
    .config {
      grid-area: config;
    }
    .uptime {
      grid-area: uptime;
    }
    .monitor {
      grid-area: monitor;
    }
    .cpu {
      grid-area: cpu;
    }
    .mem {
      grid-area: mem;
    }
    .item .k {
      font-size: var(--ak-fs-xs, 0.72rem);
      letter-spacing: 0.06em;
      text-transform: uppercase;
      color: var(--ak-text-faint);
      flex: 0 0 auto;
    }
    .item .v {
      min-width: 0;
      color: var(--ak-text);
      font-variant-numeric: tabular-nums;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    /* A long config path truncates instead of widening the row; the title
       attribute on the item keeps the full path reachable. */
    .config .v {
      flex: 0 1 auto;
    }
    .meter {
      position: relative;
      flex: 1 1 48px;
      height: 4px;
      min-width: 32px;
      align-self: center;
      border-radius: var(--ak-radius-full);
      background: var(--ak-surface-3);
      overflow: hidden;
    }
    .meter > span {
      position: absolute;
      inset: 0 auto 0 0;
      border-radius: var(--ak-radius-full);
      background: var(--ak-accent-strong);
      transition: width 0.4s var(--ak-ease, ease);
    }
    .meter > span[data-hot='true'] {
      background: var(--ak-signal);
    }
    .refresh {
      grid-area: refresh;
      justify-self: end;
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
    }
    .refresh select {
      height: var(--ak-control-h, 28px);
      padding: 0 var(--ak-space-2);
      background: var(--ak-surface-2);
      color: var(--ak-text);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-sm, 6px);
      font: inherit;
      font-size: var(--ak-fs-sm, 0.8rem);
      box-sizing: border-box;
    }
    .refresh label {
      font-size: var(--ak-fs-xs, 0.72rem);
      letter-spacing: 0.06em;
      text-transform: uppercase;
      color: var(--ak-text-faint);
    }
  `

  connectedCallback(): void {
    super.connectedCallback()
    this.console = new ConsoleController(this, this.store)
  }

  private onApply(): void {
    if (this.applying) return
    const detail = this.pendingSummary(this.consoleState.doc?.pending)
    if (!window.confirm(
      `应用全部待应用变更？\n${detail || '（无）'}\n变更的程序将被停止并以新定义重启（原先停止的保持停止）。`,
    )) {
      return
    }
    this.applying = true
    this.applyError = null
    this.applyResult = null
    this.store
      .apply({})
      .then((r) => {
        this.applyResult = r
      })
      .catch((e: unknown) => {
        this.applyError = String(e instanceof Error ? e.message : e)
      })
      .finally(() => {
        this.applying = false
      })
  }

  private pendingOneLiner(p: PendingDoc | undefined): string {
    if (!p) return ''
    const parts: string[] = []
    if (p.programs.length) parts.push(`${p.programs.length} 个程序配置有变化`)
    if (p.apps_added.length) parts.push(`新增 app: ${p.apps_added.join(', ')}`)
    if (p.apps_removed.length) parts.push(`注销 app: ${p.apps_removed.join(', ')}`)
    if (p.daemon_hints.length) parts.push(p.daemon_hints[0]!)
    if (p.errors.length) parts.push(`${p.errors.length} 个应用检出失败`)
    return parts.join(' · ')
  }

  private pendingSummary(p: PendingDoc | undefined): string {
    if (!p) return ''
    const lines: string[] = []
    for (const x of p.programs.slice(0, 20)) {
      lines.push(`  ${x.app}.${x.program} [${x.running ? 'running' : 'stopped'}] 配置有变化`)
    }
    if (p.programs.length > 20) lines.push(`  … 共 ${p.programs.length} 个`)
    for (const a of p.apps_added) lines.push(`  app[${a}] 新注册`)
    for (const a of p.apps_removed) lines.push(`  app[${a}] 已注销`)
    for (const h of p.daemon_hints) lines.push(`  ${h}`)
    for (const e of p.errors) lines.push(`  ${e}`)
    return lines.join('\n')
  }

  private onRefreshChange(e: Event): void {
    const value = Number((e.target as HTMLSelectElement).value) as RefreshMs
    this.refreshMs = value
    saveRefreshMs(value)
    this.store.setRefreshInterval(value)
  }

  render() {
    const { doc } = this.consoleState
    const daemon = doc?.daemon
    const sys = daemon?.system
    const memPct =
      sys && sys.mem_total_bytes > 0
        ? Math.min(100, (sys.mem_used_bytes / sys.mem_total_bytes) * 100)
        : null
    const pending = this.consoleState.doc?.pending
    const pendingCount =
      (pending?.programs.length ?? 0) +
      (pending?.apps_added.length ?? 0) +
      (pending?.apps_removed.length ?? 0) +
      (pending?.daemon_hints.length ?? 0)
    const flash = this.store.flash
    return html`
      ${flash
        ? html`<div
            class="pending"
            role=${flash.ok ? 'status' : 'alert'}
            aria-label="操作结果"
          >
            <span class="detail">${flash.text}</span>
            <span class="actions">
              <button @click=${() => this.store.clearFlash()} title="关闭提示">关闭</button>
            </span>
          </div>`
        : nothing}
      ${pendingCount > 0
        ? html`<div class="pending" role="alert" aria-label="待应用变更">
            <span class="badge">待应用变更 <span class="count">${pendingCount}</span></span>
            <span class="detail">${this.pendingOneLiner(pending)}</span>
            <span class="actions">
              <button
                data-primary="true"
                ?disabled=${this.applying}
                @click=${this.onApply}
                title="应用全部待应用变更（变更程序停止后重建重启，手动停止的保持停止）"
              >
                ${this.applying ? '应用中…' : 'Apply 全部'}
              </button>
            </span>
          </div>`
        : nothing}
      ${this.applyResult || this.applyError
        ? html`<div
            class="pending"
            role=${this.applyError ? 'alert' : 'status'}
            aria-label="apply 结果"
          >
            ${this.applyResult
              ? html`<span class="detail">${this.applyResult}</span>`
              : nothing}
            ${this.applyError
              ? html`<span class="detail" data-error="true">${this.applyError}</span>`
              : nothing}
          </div>`
        : nothing}
      <div class="bar" role="status" aria-label="守护进程概况">
        <span class="item version"
          ><span class="k">版本</span><span class="v">${daemon?.version ?? '—'}</span></span
        >
        <span class="item config" title=${daemon?.config_source ?? ''}>
          <span class="k">配置</span>
          <span class="v">${daemon?.config_source ?? '—'}</span>
        </span>
        <span class="item uptime"
          ><span class="k">运行</span><span class="v">${formatUptime(daemon?.uptime_secs)}</span></span
        >
        <span class="item monitor"
          ><span class="k">巡检</span
          ><span class="v">${daemon ? `${daemon.monitor_interval}s` : '—'}</span></span
        >
        <span class="item cpu">
          <span class="k">系统 CPU</span>
          <span class="v">${formatPercent(sys?.cpu_percent ?? null)}</span>
          <span class="meter" role="presentation"
            ><span data-hot=${(sys?.cpu_percent ?? 0) > 75} style=${`width:${sys?.cpu_percent ?? 0}%`}></span
          ></span>
        </span>
        <span class="item mem">
          <span class="k">系统内存</span>
          <span class="v">
            ${sys ? `${formatBytes(sys.mem_used_bytes)} / ${formatBytes(sys.mem_total_bytes)}` : '—'}
          </span>
          <span class="meter" role="presentation"
            ><span data-hot=${(memPct ?? 0) > 90} style=${`width:${memPct ?? 0}%`}></span
          ></span>
        </span>
        <span class="refresh">
          <label for="refresh-select">刷新</label>
          <select id="refresh-select" .value=${String(this.refreshMs)} @change=${this.onRefreshChange}>
            ${REFRESH_CHOICES_MS.map(
              (ms) => html`<option value=${ms} ?selected=${ms === this.refreshMs}>${ms / 1000}s</option>`,
            )}
          </select>
        </span>
      </div>
    `
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-overview-bar': AkOverviewBar
  }
}
