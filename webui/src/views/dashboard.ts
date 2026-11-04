/**
 * Dashboard (route `/`): the global program table. Every registered app
 * contributes a group whose title carries the app name and program count;
 * apps that contain a fatal or backing-off program are listed first, and
 * inside a group those rows lead, so "who is down" is visible without
 * drilling in (webui-ui: 全局程序总览). The sidebar already is the app
 * directory — this view intentionally has no app cards.
 *
 * Daemon system metrics live in the overview bar; the degraded-transport
 * banners stay here because they qualify the data this table shows.
 */

import { LitElement, css, html, nothing } from 'lit'
import { customElement, property } from 'lit/decorators.js'
import { ConsoleController, consoleStore, type ConsoleStore } from '../lib/store.js'
import type { ProgramInfo } from '../lib/types.js'
import { navigate } from '../router.js'
import { viewStyles } from '../styles/shared.js'
import '../components/program-table.js'
import type { ProgramSelectDetail } from '../components/program-table.js'

/** Fatal first, then backing off, then everything else. */
const STATE_RANK: Record<string, number> = { fatal: 0, backoff: 1 }

function rank(state: string): number {
  return STATE_RANK[state] ?? 2
}

interface AppGroup {
  app: string
  programs: ProgramInfo[]
  worst: number
}

@customElement('ak-dashboard')
export class AkDashboard extends LitElement {
  /** Injectable for tests; defaults to the shared singleton. */
  @property({ attribute: false }) store: ConsoleStore = consoleStore

  private console: ConsoleController | null = null

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
      .page-head {
        margin-bottom: 0;
      }
      .banner {
        padding: var(--ak-space-2) var(--ak-space-3);
        border-radius: var(--ak-radius-sm);
        font-size: var(--ak-fs-sm);
      }
      .banner[data-mode='poll'] {
        background: color-mix(in srgb, var(--ak-status-working) 12%, transparent);
        color: var(--ak-status-working);
        border: 1px solid var(--ak-status-working-border, transparent);
      }
      .banner[data-mode='offline'] {
        background: var(--ak-status-failed-bg);
        color: var(--ak-status-failed);
        border: 1px solid var(--ak-status-failed-border);
      }
      ak-program-table {
        display: block;
        min-width: 0;
      }
    `,
  ]

  connectedCallback(): void {
    super.connectedCallback()
    this.console = new ConsoleController(this, this.store)
  }

  private get consoleState() {
    return this.console?.snapshot ?? { doc: null, transport: 'connecting' as const }
  }

  /**
   * Group by app; apps holding a problem program come first, then by name.
   * Within a group the problem rows lead (spec: 故障程序无需下钻可见).
   */
  private groups(): AppGroup[] {
    const map = new Map<string, ProgramInfo[]>()
    for (const p of this.consoleState.doc?.programs ?? []) {
      const list = map.get(p.app) ?? []
      list.push(p)
      map.set(p.app, list)
    }
    const groups: AppGroup[] = [...map].map(([app, programs]) => ({
      app,
      programs: [...programs].sort(
        (a, b) => rank(a.state) - rank(b.state) || a.name.localeCompare(b.name),
      ),
      worst: Math.min(...programs.map((p) => rank(p.state))),
    }))
    groups.sort((a, b) => a.worst - b.worst || a.app.localeCompare(b.app))
    return groups
  }

  private pendingOf(app: string): string[] {
    return (this.consoleState.doc?.pending.programs ?? [])
      .filter((x) => x.app === app)
      .map((x) => x.program)
  }

  private onSelect(e: Event): void {
    const detail = (e as CustomEvent<ProgramSelectDetail>).detail
    if (!detail?.app || !detail.program) return
    navigate(`/app/${encodeURIComponent(detail.app)}/program/${encodeURIComponent(detail.program)}`)
  }

  render() {
    const { doc, transport } = this.consoleState
    const groups = this.groups()
    const total = doc?.programs.length ?? 0
    return html`
      <header class="page-head">
        <div>
          <h1 class="page-title">总览</h1>
          <p class="page-sub">${groups.length} 个应用 · ${total} 个程序</p>
        </div>
      </header>
      ${transport === 'poll'
        ? html`<div class="banner" data-mode="poll" role="status">
            实时连接已断开 — 正以轮询方式刷新（数据可能略有延迟）
          </div>`
        : nothing}
      ${transport === 'offline'
        ? html`<div class="banner" data-mode="offline" role="alert">
            后端不可达 — 显示的是最后已知数据（已过期）；恢复后将自动更新
          </div>`
        : nothing}
      ${doc && total === 0
        ? html`<div class="panel">
            <div class="empty">
              <span class="title">还没有应用</span>
              <p class="hint">
                <code class="mono">ak compose &lt;目录&gt;</code>
                注册后，这里的程序总表会按应用分组列出全部程序。
              </p>
            </div>
          </div>`
        : html`${groups.map(
            (g) => html`<ak-program-table
              .programs=${g.programs}
              .pending=${this.pendingOf(g.app)}
              .store=${this.store}
              heading=${g.app}
              caption=${`${g.programs.length} 个程序`}
              .showApp=${true}
              @program-select=${this.onSelect}
            ></ak-program-table>`,
          )}`}
    `
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-dashboard': AkDashboard
  }
}
