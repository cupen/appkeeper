/**
 * Sidebar two-level tree (webui-ui: 左侧两层导航): registered apps at the
 * top level, their declared programs below — including programs that never
 * started. The selected node is highlighted; selection navigates via the
 * SPA router, so deep links and browser history work for free.
 *
 * Status is carried by a 6px leading dot only — the name text stays neutral
 * (webui-visual-system: 状态的颜色与形状分工). The Chinese state word rides
 * along in `title` and `aria-label`, so the dot's colour is never the only
 * channel. Names are single-line with ellipsis; long names cannot grow the
 * row or push the program count out of its fixed trailing slot.
 */

import { LitElement, css, html, nothing } from 'lit'
import { customElement, property } from 'lit/decorators.js'
import { navigate } from '../router.js'
import type { ProgramInfo } from '../lib/types.js'
import { stateLabel } from '../lib/format.js'

export interface TreeApp {
  name: string
  programs: ProgramInfo[]
}

@customElement('ak-console-tree')
export class AkConsoleTree extends LitElement {
  @property({ type: Array }) apps: TreeApp[] = []
  /** Currently selected path segments (raw, still encoded). */
  @property() selectedApp = ''
  @property() selectedProgram = ''

  static styles = css`
    :host {
      display: block;
    }
    .app {
      margin: 0 0 var(--ak-space-2);
    }
    .app-link {
      display: flex;
      align-items: center;
      gap: var(--ak-space-2);
      min-height: var(--ak-control-h, 28px);
      padding: var(--ak-space-2) var(--ak-space-3);
      border-radius: var(--ak-radius-md);
      text-decoration: none;
      color: var(--ak-text-bright);
      font-weight: 600;
      font-size: var(--ak-fs-md, 0.875rem);
      box-sizing: border-box;
    }
    .app-link:hover {
      background: var(--ak-surface-2);
    }
    .app-link[data-active='true'] {
      background: var(--ak-accent-soft);
      color: var(--ak-accent-hover);
    }
    .app-link .name {
      flex: 1 1 auto;
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .app-link .count {
      flex: 0 0 auto;
      margin-left: auto;
      font-size: var(--ak-fs-xs, 0.72rem);
      font-weight: 500;
      color: var(--ak-text-faint);
      font-variant-numeric: tabular-nums;
    }
    .progs {
      margin: var(--ak-space-1) 0 0 var(--ak-space-3);
      padding-left: var(--ak-space-2);
      border-left: 1px solid var(--ak-border);
      display: flex;
      flex-direction: column;
    }
    a.prog {
      display: flex;
      align-items: center;
      gap: var(--ak-space-2);
      min-height: var(--ak-control-h, 28px);
      padding: 0 var(--ak-space-2);
      border-radius: var(--ak-radius-sm, 6px);
      text-decoration: none;
      /* Neutral: status colour lives on the dot, never on the name. */
      color: var(--ak-text-dim);
      font-size: var(--ak-fs-md, 0.875rem);
      font-variant-numeric: tabular-nums;
      box-sizing: border-box;
    }
    a.prog:hover {
      background: var(--ak-surface-2);
      color: var(--ak-text);
    }
    a.prog[data-active='true'] {
      background: var(--ak-accent-soft);
      color: var(--ak-accent-hover);
    }
    /* 6px leading status dot — the only place a status hue appears here.
       Each state also differs by position and the accessible name. */
    a.prog .dot {
      flex: 0 0 6px;
      width: 6px;
      height: 6px;
      border-radius: 50%;
      background: var(--ak-status-dormant, #94a0b4);
    }
    a.prog[data-state='running'] .dot {
      background: var(--ak-status-done, #46d189);
    }
    a.prog[data-state='starting'] .dot,
    a.prog[data-state='stopping'] .dot {
      background: var(--ak-status-starting, #6ea8fe);
    }
    a.prog[data-state='backoff'] .dot {
      background: var(--ak-status-working, #f2b04e);
    }
    a.prog[data-state='fatal'] .dot {
      background: var(--ak-status-failed, #f47174);
    }
    a.prog .name {
      flex: 1 1 auto;
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .empty {
      padding: var(--ak-space-3);
      color: var(--ak-text-faint);
      font-size: var(--ak-fs-sm, 0.8rem);
    }
  `

  private go(e: MouseEvent, path: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || e.button !== 0) return
    e.preventDefault()
    navigate(path)
  }

  render() {
    if (this.apps.length === 0) {
      return html`<div class="empty">没有已注册的应用（ak add 注册后出现在这里）</div>`
    }
    return html`
      ${this.apps.map(
        (app) => html`
          <section class="app" data-app=${app.name}>
            <a
              class="app-link"
              href=${`/app/${encodeURIComponent(app.name)}`}
              data-active=${app.name === this.selectedApp}
              aria-label=${`${app.name}，${app.programs.length} 个程序`}
              title=${app.name}
              @click=${(e: MouseEvent) => this.go(e, `/app/${encodeURIComponent(app.name)}`)}
            >
              <span aria-hidden="true">▤</span>
              <span class="name">${app.name}</span>
              <span class="count">${app.programs.length}</span>
            </a>
            ${app.programs.length > 0
              ? html`<div class="progs" role="group" aria-label=${`${app.name} 的程序`}>
                  ${app.programs.map(
                    (p) => html`
                      <a
                        class="prog"
                        data-state=${p.state}
                        data-app=${app.name}
                        data-program=${p.name}
                        data-active=${p.name === this.selectedProgram && app.name === this.selectedApp}
                        href=${`/app/${encodeURIComponent(app.name)}/program/${encodeURIComponent(p.name)}`}
                        @click=${(
                          e: MouseEvent,
                        ) => this.go(e, `/app/${encodeURIComponent(app.name)}/program/${encodeURIComponent(p.name)}`)}
                        title=${`${p.name} — ${stateLabel(p.state)}`}
                        aria-label=${`${p.name} — ${stateLabel(p.state)}`}
                      >
                        <span class="dot" aria-hidden="true"></span>
                        <span class="name">${p.name}</span>
                      </a>
                    `,
                  )}
                </div>`
              : nothing}
          </section>
        `,
      )}
    `
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-console-tree': AkConsoleTree
  }
}
