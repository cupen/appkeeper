/**
 * App overview (webui-ui: App 概况): the app's programs in the shared
 * `ak-program-table` — name, status badge, restarts, uptime, CPU, memory and
 * the two log-rate columns, with the rate-window switch in the table's title
 * bar. Row-tail quick actions and row navigation come from the shared table,
 * so this page and the dashboard cannot drift apart. Not-running programs
 * show placeholders, never fake zeros; fatal rows are highlighted.
 *
 * Page title area keeps the app-level apply entry and the destructive
 * `移除应用` action (same control height/radius as any other button).
 */

import { LitElement, css, html, nothing } from 'lit'
import { customElement, property, state } from 'lit/decorators.js'
import { ApiError } from '../lib/api.js'
import { ConsoleController, consoleStore, type ConsoleStore } from '../lib/store.js'
import type { ProgramInfo } from '../lib/types.js'
import { navigate } from '../router.js'
import { viewStyles } from '../styles/shared.js'
import '../components/program-table.js'
import type { ProgramSelectDetail } from '../components/program-table.js'

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

  @state() private applying = false
  @state() private applyFeedback: { ok: boolean; text: string } | null = null
  @state() private removing = false
  @state() private removeFeedback: { ok: boolean; text: string } | null = null

  static styles = [
    viewStyles,
    css`
      :host {
        flex: 1;
        min-height: 0;
        overflow-y: auto;
        display: flex;
        flex-direction: column;
        gap: var(--ak-space-4);
        /* Horizontal padding belongs to ak-app main alone (shell alignment). */
        padding: var(--ak-space-4) 0;
        box-sizing: border-box;
      }
      /* One page-head for every routed view: same structure, same control
         heights — the destructive action is not a different-sized control. */
      .page-head {
        margin-bottom: 0;
      }
      .head-actions {
        display: flex;
        align-items: center;
        gap: var(--ak-space-2);
        flex-wrap: wrap;
      }
      .btn {
        display: inline-flex;
        align-items: center;
        height: var(--ak-control-h);
        padding: 0 var(--ak-space-3);
        box-sizing: border-box;
        border: 1px solid var(--ak-border);
        border-radius: var(--ak-radius-sm);
        background: var(--ak-surface-2);
        color: var(--ak-text);
        font: inherit;
        font-size: var(--ak-fs-sm);
        cursor: pointer;
      }
      .btn:hover:not(:disabled) {
        border-color: var(--ak-border-strong);
        color: var(--ak-text-bright);
      }
      .btn-danger {
        border-color: var(--ak-status-failed-border, #b91c1c);
        background: transparent;
        color: var(--ak-status-failed, #ef4444);
      }
      .btn-danger:hover:not(:disabled) {
        background: color-mix(in srgb, var(--ak-status-failed, #ef4444) 12%, transparent);
      }
      .btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
      }
      .remove-strip {
        display: flex;
        align-items: flex-start;
        gap: var(--ak-space-2);
        padding: var(--ak-space-2) var(--ak-space-4);
        border-radius: var(--ak-radius-lg);
        border: 1px solid var(--ak-border);
        background: var(--ak-surface);
        font-size: var(--ak-fs-sm);
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
      .apply-strip {
        display: flex;
        align-items: center;
        gap: var(--ak-space-3);
        padding: var(--ak-space-2) var(--ak-space-4);
        background: color-mix(in srgb, var(--ak-accent-strong, #6366f1) 12%, var(--ak-surface));
        border: 1px solid color-mix(in srgb, var(--ak-accent-strong, #6366f1) 35%, var(--ak-border));
        border-radius: var(--ak-radius-lg);
        font-size: var(--ak-fs-sm);
      }
      .apply-strip .count {
        color: var(--ak-text-bright);
        font-weight: 600;
      }
      .apply-strip button {
        display: inline-flex;
        align-items: center;
        height: var(--ak-control-h);
        padding: 0 var(--ak-space-3);
        box-sizing: border-box;
        margin-left: auto;
        border: 1px solid transparent;
        border-radius: var(--ak-radius-sm);
        background: var(--ak-accent-strong, #6366f1);
        color: var(--ak-accent-ink, #fff);
        font: inherit;
        font-weight: 600;
        font-size: var(--ak-fs-sm);
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
        font-size: var(--ak-fs-xs);
      }
      .apply-strip .feedback[data-error='true'] {
        color: var(--ak-status-failed, #ef4444);
      }
      ak-program-table {
        display: block;
        min-width: 0;
      }
    `,
  ]

  private programsOf(app: string): ProgramInfo[] {
    return this.console?.snapshot.doc?.programs.filter((p) => p.app === app) ?? []
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

  private onProgramSelect(e: Event): void {
    const detail = (e as CustomEvent<ProgramSelectDetail>).detail
    if (!detail?.program) return
    navigate(
      `/app/${encodeURIComponent(this.app)}/program/${encodeURIComponent(detail.program)}`,
    )
  }

  render() {
    const programs = this.programsOf(this.app)
    const pendingSet = this.pendingSet()
    const fatal = programs.filter((p) => p.state === 'fatal')
    return html`
      <header class="page-head">
        <div>
          <h1 class="page-title">${this.app}</h1>
          <p class="page-sub">${programs.length} 个程序</p>
        </div>
        <div class="head-actions">
          <button
            class="btn btn-danger"
            data-kind="remove"
            ?disabled=${this.removing}
            @click=${() => void this.onRemove()}
            title="移除整个应用（不会自动停止程序；运行中的程序会阻止移除）"
          >
            ${this.removing ? '移除中…' : '移除应用'}
          </button>
        </div>
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
      ${fatal.length > 0
        ? html`<div class="callout callout-error" role="alert">
            ${fatal.length} 个程序处于故障：${fatal.map((p) => p.name).join(', ')}
          </div>`
        : nothing}
      ${pendingSet.size > 0 || this.applyFeedback
        ? html`<div class="apply-strip" role="alert">
            <span><span class="count">${pendingSet.size}</span> 个程序配置有变化（待应用）</span>
            <button ?disabled=${this.applying} @click=${() => void this.onApply()}>
              ${this.applying ? '应用中…' : 'Apply 此应用'}
            </button>
            ${this.applyFeedback
              ? html`<span class="feedback" data-error=${!this.applyFeedback.ok} role="status"
                  >${this.applyFeedback.text}</span
                >`
              : nothing}
          </div>`
        : nothing}
      <ak-program-table
        .programs=${programs}
        .pending=${[...pendingSet]}
        .store=${this.store}
        heading="程序"
        caption=${`${programs.length} 个程序`}
        emptyText="该应用没有声明任何程序"
        @program-select=${this.onProgramSelect}
      ></ak-program-table>
    `
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-app-overview': AkAppOverview
  }
}
