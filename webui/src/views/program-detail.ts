/**
 * Program detail (webui-ui: 进程详情 + 单程序控制): one single-column
 * information flow — title row (breadcrumb / name / badge / actions) →
 * compact metric strip (PID, uptime, exits, CPU, memory, health) → mono
 * command block (command + working directory) → fatal / last-exit notices →
 * the log viewer, which takes the remaining height (design D8).
 *
 * Status shape is owned by `ak-status-badge`; the title carries no duplicate
 * glyph. Destructive actions keep their inline confirmation step and the
 * `data-kind` hooks; controls share the top bar's control height so the two
 * rows never look staggered.
 */

import { LitElement, css, html, nothing } from 'lit'
import { customElement, property, state } from 'lit/decorators.js'
import { apiFetch } from '../lib/api.js'
import { ConsoleController, consoleStore, type ConsoleStore } from '../lib/store.js'
import { formatBytes, formatPercent, formatUptime, stateLabel } from '../lib/format.js'
import type { ProgramInfo } from '../lib/types.js'
import '../components/status-badge.js'
import '../components/log-viewer.js'

type PendingAction = 'stop' | 'restart' | null

interface HealthView {
  text: string
  tone: 'ok' | 'bad' | 'pending' | 'none'
}

@customElement('ak-program-detail')
export class AkProgramDetail extends LitElement {
  @property() app = ''
  @property() program = ''

  /** Injectable for tests; defaults to the shared singleton. */
  @property({ attribute: false }) store: ConsoleStore = consoleStore

  private console: ConsoleController | null = null

  private get consoleState() {
    return this.console?.snapshot ?? { doc: null, transport: 'connecting' as const }
  }

  @state() private confirm: PendingAction = null
  @state() private notice: { ok: boolean; text: string } | null = null
  @state() private busy = false

  static styles = css`
    :host {
      flex: 1;
      min-height: 0;
      overflow-y: auto;
      display: flex;
      flex-direction: column;
      gap: var(--ak-space-3);
      /* Horizontal padding belongs to ak-app main alone (shell alignment). */
      padding: var(--ak-space-4) 0;
      box-sizing: border-box;
    }
    /* Title row: breadcrumb / name / badge / actions. */
    header {
      display: flex;
      align-items: center;
      gap: var(--ak-space-3);
      flex-wrap: wrap;
      min-width: 0;
    }
    .crumb {
      flex: 0 0 auto;
      color: var(--ak-text-dim);
      font-size: var(--ak-fs-md);
      text-decoration: none;
      max-width: 28ch;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .crumb:hover {
      color: var(--ak-accent-hover);
      text-decoration: underline;
      text-underline-offset: 3px;
    }
    .crumb-sep {
      flex: 0 0 auto;
      color: var(--ak-text-faint);
      font-size: var(--ak-fs-md);
    }
    /* Backoff delay beside the badge, not inside it (webui-ui: 状态展示). */
    .backoff-note {
      flex: 0 0 auto;
      color: var(--ak-text-faint);
      font-size: var(--ak-fs-xs);
      white-space: nowrap;
    }
    h1 {
      margin: 0;
      min-width: 0;
      font-size: var(--ak-fs-xl);
      font-weight: 650;
      letter-spacing: -0.015em;
      color: var(--ak-text-bright);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .head-actions {
      margin-left: auto;
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
      flex-wrap: wrap;
    }
    /* Every control on this page matches the top bar's control height. */
    button {
      display: inline-flex;
      align-items: center;
      height: var(--ak-control-h);
      padding: 0 var(--ak-space-3);
      box-sizing: border-box;
      font: inherit;
      font-size: var(--ak-fs-sm);
      border-radius: var(--ak-radius-sm);
      border: 1px solid var(--ak-border);
      background: var(--ak-surface-2);
      color: var(--ak-text);
      cursor: pointer;
      transition:
        background var(--ak-dur) var(--ak-ease),
        border-color var(--ak-dur) var(--ak-ease);
    }
    button:hover:not(:disabled) {
      border-color: var(--ak-border-strong);
      background: var(--ak-surface-3);
    }
    button:disabled {
      opacity: 0.5;
      cursor: not-allowed;
    }
    button[data-kind='start'] {
      border-color: var(--ak-status-done-border);
      color: var(--ak-status-done);
    }
    button[data-kind='stop'] {
      border-color: var(--ak-status-failed-border);
      color: var(--ak-status-failed);
    }
    /* Inline confirmation: same control height, no extra box around it. */
    .confirm {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
      flex-wrap: wrap;
      margin-left: auto;
      font-size: var(--ak-fs-sm);
    }
    .confirm-text {
      color: var(--ak-signal);
      white-space: nowrap;
    }
    .confirm-text strong {
      color: var(--ak-text-bright);
      font-weight: 600;
    }
    .notice {
      padding: var(--ak-space-2) var(--ak-space-3);
      border-radius: var(--ak-radius-md);
      border: 1px solid var(--ak-border);
      background: var(--ak-surface);
      color: var(--ak-text-dim);
      font-size: var(--ak-fs-sm);
      white-space: pre-wrap;
    }
    .notice[data-tone='ok'] {
      border-color: var(--ak-status-done-border);
      background: var(--ak-status-done-bg);
      color: var(--ak-status-done);
    }
    .notice[data-tone='bad'] {
      border-color: var(--ak-status-failed-border);
      background: var(--ak-status-failed-bg);
      color: var(--ak-status-failed);
    }
    .notice[data-tone='fatal'] {
      border-color: var(--ak-status-failed-border);
      background: var(--ak-status-failed-bg);
      color: var(--ak-text);
    }
    .notice[data-tone='fatal'] strong {
      color: var(--ak-status-failed);
    }
    /* Compact metric strip: six inline key/value cells; folds to two rows of
       three at the narrow breakpoint (task 5.3). */
    .metrics {
      display: grid;
      grid-template-columns: repeat(6, minmax(0, 1fr));
      margin: 0;
      padding: 0;
      list-style: none;
      background: var(--ak-surface);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-lg);
      box-shadow: var(--ak-shadow-1);
      overflow: hidden;
    }
    .metrics > li {
      display: flex;
      flex-direction: column;
      gap: 2px;
      min-width: 0;
      padding: var(--ak-space-2) var(--ak-space-3);
      border-right: 1px solid var(--ak-border);
    }
    .metrics > li:last-child {
      border-right: none;
    }
    .metrics .k {
      font-size: var(--ak-fs-xs);
      letter-spacing: 0.06em;
      text-transform: uppercase;
      color: var(--ak-text-faint);
      white-space: nowrap;
    }
    .metrics .v {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2);
      min-width: 0;
      font-size: var(--ak-fs-md);
      color: var(--ak-text-bright);
      font-variant-numeric: tabular-nums;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .metrics .dot {
      flex: 0 0 auto;
      width: 6px;
      height: 6px;
      border-radius: 50%;
      background: var(--ak-status-dormant);
    }
    .metrics .v[data-tone='ok'] .dot {
      background: var(--ak-status-done);
    }
    .metrics .v[data-tone='bad'] .dot {
      background: var(--ak-status-failed);
    }
    .metrics .v[data-tone='pending'] .dot {
      background: var(--ak-status-working);
    }
    /* No health signal when the process is not up. */
    .metrics .v[data-tone='none'] .dot {
      display: none;
    }
    /* Mono command block: no wrapping, scrolls horizontally on its own so the
       page never gains a horizontal scrollbar. */
    .command {
      display: flex;
      flex-direction: column;
      gap: var(--ak-space-1);
      padding: var(--ak-space-2) var(--ak-space-3);
      background: var(--ak-well);
      border: 1px solid var(--ak-border);
      border-radius: var(--ak-radius-lg);
      min-width: 0;
    }
    .cmd-row {
      display: flex;
      align-items: baseline;
      gap: var(--ak-space-3);
      min-width: 0;
    }
    .cmd-row .k {
      flex: 0 0 6em;
      font-size: var(--ak-fs-xs);
      letter-spacing: 0.06em;
      text-transform: uppercase;
      color: var(--ak-text-faint);
    }
    .cmd-row code {
      flex: 1;
      min-width: 0;
      overflow-x: auto;
      white-space: pre;
      font-family: var(--ak-font-mono);
      font-size: var(--ak-fs-sm);
      color: var(--ak-text);
    }
    /* The log viewer takes whatever height the blocks above leave over
       (design D8); 260px is the floor before the page starts scrolling. */
    ak-log-viewer {
      flex: 1 1 auto;
      min-height: 260px;
      min-width: 0;
    }
    .missing {
      padding: var(--ak-space-6);
      text-align: center;
      color: var(--ak-text-faint);
    }
    @media (max-width: 719.98px) {
      .metrics {
        grid-template-columns: repeat(3, minmax(0, 1fr));
      }
      .metrics > li {
        border-right: 1px solid var(--ak-border);
      }
      .metrics > li:nth-child(3n) {
        border-right: none;
      }
      .metrics > li:nth-child(-n + 3) {
        border-bottom: 1px solid var(--ak-border);
      }
    }
  `

  connectedCallback(): void {
    super.connectedCallback()
    this.console = new ConsoleController(this, this.store)
  }

  private get info(): ProgramInfo | null {
    return (
      this.consoleState.doc?.programs.find(
        (p) => p.app === this.app && p.name === this.program,
      ) ?? null
    )
  }

  /** Health is meaningful only while the process is up (webui-ui: 进程详情). */
  private healthOf(p: ProgramInfo): HealthView {
    if (p.state !== 'running' && p.state !== 'starting') {
      return { text: '—', tone: 'none' }
    }
    if (p.unhealthy) return { text: '异常', tone: 'bad' }
    if (p.health_confirmed) return { text: '正常', tone: 'ok' }
    return { text: '待确认', tone: 'pending' }
  }

  private async act(action: 'start' | 'stop' | 'restart'): Promise<void> {
    this.busy = true
    this.confirm = null
    this.notice = null
    try {
      const res = await apiFetch(
        `/api/programs/${encodeURIComponent(this.program)}/${action}`,
        { method: 'POST' },
      )
      const body = (await res.json().catch(() => ({}))) as { result?: string; error?: string }
      if (res.ok) {
        this.notice = { ok: true, text: body.result ?? `${action} 已提交` }
      } else {
        this.notice = { ok: false, text: body.error ?? `${action} 失败（HTTP ${res.status}）` }
      }
    } catch (e) {
      this.notice = { ok: false, text: `${action} 失败：${String(e)}` }
    }
    this.busy = false
  }

  private renderActions() {
    const p = this.info
    if (!p) return nothing
    const running = p.state === 'running' || p.state === 'starting'
    if (this.confirm) {
      return html`
        <span class="confirm" role="alertdialog" aria-label="确认操作">
          <span class="confirm-text">
            确认对 <strong>${p.name}</strong> 执行
            ${this.confirm === 'stop' ? '停止' : '重启'}？
          </span>
          <button
            data-kind=${this.confirm}
            ?disabled=${this.busy}
            @click=${() => this.act(this.confirm!)}
          >
            确认
          </button>
          <button @click=${() => (this.confirm = null)}>取消</button>
        </span>
      `
    }
    return html`
      <span class="head-actions">
        ${running
          ? html`
              <button data-kind="stop" ?disabled=${this.busy} @click=${() => (this.confirm = 'stop')}>
                停止
              </button>
              <button ?disabled=${this.busy} @click=${() => (this.confirm = 'restart')}>重启</button>
            `
          : html`<button data-kind="start" ?disabled=${this.busy} @click=${() => this.act('start')}>
              启动
            </button>`}
      </span>
    `
  }

  private renderMetrics(p: ProgramInfo) {
    const health = this.healthOf(p)
    return html`<ul class="metrics" aria-label="运行指标">
      <li>
        <span class="k">PID</span>
        <span class="v">${p.pid ?? '—'}</span>
      </li>
      <li>
        <span class="k">uptime</span>
        <span class="v">${formatUptime(p.uptime_secs)}</span>
      </li>
      <li>
        <span class="k">退出 / 重启</span>
        <span class="v">${p.total_exits}</span>
      </li>
      <li>
        <span class="k">CPU</span>
        <span class="v">${formatPercent(p.cpu_percent)}</span>
      </li>
      <li>
        <span class="k">内存</span>
        <span class="v">${formatBytes(p.mem_bytes)}</span>
      </li>
      <li>
        <span class="k">健康</span>
        <span class="v" data-tone=${health.tone}>
          <span class="dot" aria-hidden="true"></span>${health.text}
        </span>
      </li>
    </ul>`
  }

  private renderCommand(p: ProgramInfo) {
    const command = [p.command, ...p.args].filter(Boolean).join(' ')
    return html`<div class="command">
      <div class="cmd-row">
        <span class="k">命令</span>
        <code title=${command || '—'}>${command || '—'}</code>
      </div>
      <div class="cmd-row">
        <span class="k">工作目录</span>
        <code title=${p.work_dir || '（继承守护进程）'}>${p.work_dir || '（继承守护进程）'}</code>
      </div>
    </div>`
  }

  render() {
    const p = this.info
    if (!p) {
      return html`<div class="missing">程序 ${this.program} 不存在（可能已被移除）</div>`
    }
    // Backoff delay: secondary text beside the badge so the pill (and its
    // width) stays independent of the countdown.
    const backoff =
      p.state === 'backoff' && Number.isFinite(p.restart_backoff) && p.restart_backoff > 0
        ? `${p.restart_backoff}s 后重启`
        : ''
    return html`
      <header>
        <a
          class="crumb"
          href=${`/app/${encodeURIComponent(this.app)}`}
          title=${`返回 ${this.app} 概况`}
          >${this.app}</a
        >
        <span class="crumb-sep" aria-hidden="true">/</span>
        <h1>${p.name}</h1>
        <ak-status-badge slug=${p.state} label=${stateLabel(p.state)}></ak-status-badge>
        ${backoff ? html`<span class="backoff-note" title=${backoff}>${backoff}</span>` : nothing}
        ${this.renderActions()}
      </header>
      ${this.notice
        ? html`<div
            class="notice"
            data-tone=${this.notice.ok ? 'ok' : 'bad'}
            role=${this.notice.ok ? 'status' : 'alert'}
          >
            ${this.notice.text}
          </div>`
        : nothing}
      ${this.renderMetrics(p)} ${this.renderCommand(p)}
      ${p.fatal_reason
        ? html`<div class="notice" data-tone="fatal" role="alert">
            <strong>故障</strong>：${p.fatal_reason}
          </div>`
        : nothing}
      ${p.last_exit
        ? html`<div class="notice" data-tone="info">上次退出：${p.last_exit}</div>`
        : nothing}
      <ak-log-viewer .program=${this.program}></ak-log-viewer>
    `
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-program-detail': AkProgramDetail
  }
}
