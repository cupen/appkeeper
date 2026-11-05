/**
 * "About appkeeper" dialog (show-build-version D4): what this binary is —
 * version, git commit, build time, target platform and rustc — plus where the
 * daemon it is talking to lives and how long it has been up.
 *
 * A native `<dialog>` opened with `showModal()`; environments without it (the
 * unit-test DOM) fall back to the plain `open` attribute so nothing in the
 * tests depends on modal support. Closing (button or Esc) dispatches
 * `ak-version-closed` (composed) so the shell can drop the element and hand
 * focus back to the version trigger.
 */

import { LitElement, css, html } from 'lit'
import { customElement, property } from 'lit/decorators.js'
import type { DaemonInfo } from '../lib/types.js'
import { formatUptime } from '../lib/format.js'

/** RFC 3339 UTC → local time; `unknown`/unparseable input is shown verbatim. */
export function formatBuiltAt(raw: string): string {
  const text = raw.trim()
  if (text === '' || text === 'unknown') return 'unknown'
  const date = new Date(text)
  if (Number.isNaN(date.getTime())) return text
  const pad = (n: number): string => String(n).padStart(2, '0')
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  )
}

@customElement('ak-version-dialog')
export class AkVersionDialog extends LitElement {
  /** Daemon projection; `null` means "never connected" (offline placeholder). */
  @property({ attribute: false }) daemon: DaemonInfo | null = null
  /** Programs in the projection — shown next to the daemon's app count. */
  @property({ type: Number }) programCount = 0

  /** Set while closing: Esc reaches both the keydown and the native `cancel`
   * handler in real browsers, and the shell must only be told once. */
  private closing = false

  private onCancel = (e: Event): void => {
    // Esc: close ourselves so the shell converges in every environment.
    e.preventDefault()
    this.close()
  }

  private onKeydown = (e: KeyboardEvent): void => {
    if (e.key !== 'Escape') return
    e.preventDefault()
    this.close()
  }

  protected firstUpdated(): void {
    const dialog = this.dialogEl
    if (!dialog) return
    try {
      if (typeof dialog.showModal === 'function') dialog.showModal()
      else dialog.setAttribute('open', '')
    } catch {
      // Not connected to a document / modal unsupported: still show content.
      dialog.setAttribute('open', '')
    }
    this.renderRoot.querySelector<HTMLButtonElement>('.close')?.focus()
  }

  private get dialogEl(): HTMLDialogElement | null {
    return this.renderRoot.querySelector('dialog')
  }

  /** Close the dialog and tell the shell to drop it (focus return is its job). */
  close(): void {
    if (this.closing) return
    this.closing = true
    const dialog = this.dialogEl
    if (dialog) {
      try {
        if (typeof dialog.close === 'function') dialog.close()
      } finally {
        dialog.removeAttribute('open')
      }
    }
    this.dispatchEvent(new CustomEvent('ak-version-closed', { bubbles: true, composed: true }))
  }

  private row(label: string, value: string, title = ''): unknown {
    return html`<dt>${label}</dt>
      <dd title=${title}>${value}</dd>`
  }

  render(): unknown {
    const daemon = this.daemon
    return html`
      <dialog
        aria-labelledby="ak-version-title"
        @cancel=${this.onCancel}
        @keydown=${this.onKeydown}
      >
        <header>
          <h2 id="ak-version-title">关于 appkeeper</h2>
          <button class="close" type="button" aria-label="关闭" @click=${() => this.close()}>
            ✕
          </button>
        </header>
        ${daemon
          ? html`
              <section aria-labelledby="ak-version-build">
                <h3 id="ak-version-build">构建</h3>
                <dl>
                  ${this.row('版本', daemon.version)}
                  ${this.row('git 提交', daemon.build.git_commit)}
                  ${this.row(
                    '构建时间',
                    formatBuiltAt(daemon.build.built_at),
                    daemon.build.built_at,
                  )}
                  ${this.row('目标平台', daemon.build.target)}
                  ${this.row('Rust 版本', daemon.build.rustc)}
                </dl>
              </section>
              <section aria-labelledby="ak-version-runtime">
                <h3 id="ak-version-runtime">运行</h3>
                <dl>
                  ${this.row('控制端点', daemon.socket)}
                  ${this.row('配置来源', daemon.config_source)}
                  ${this.row('运行时长', formatUptime(daemon.uptime_secs))}
                  ${this.row('应用 / 程序', `${daemon.apps} / ${this.programCount}`)}
                </dl>
              </section>
            `
          : html`<p class="offline">当前未连接守护进程</p>`}
      </dialog>
    `
  }

  static styles = css`
    dialog {
      width: min(520px, calc(100vw - 32px));
      max-height: calc(100vh - 32px);
      box-sizing: border-box;
      padding: var(--ak-space-5, 20px);
      border: 1px solid var(--ak-border, #2a2f45);
      border-radius: var(--ak-radius-lg, 12px);
      background: var(--ak-surface, #141826);
      color: var(--ak-text, #e8ebf5);
      box-shadow: 0 18px 48px rgba(0, 0, 0, 0.45);
    }
    dialog::backdrop {
      background: rgba(8, 10, 18, 0.62);
    }
    header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: var(--ak-space-3, 12px);
      margin-bottom: var(--ak-space-4, 16px);
    }
    h2 {
      margin: 0;
      font-size: var(--ak-fs-lg, 1rem);
      color: var(--ak-text-bright, #f3f5ff);
    }
    h3 {
      margin: 0 0 var(--ak-space-2, 8px);
      font-size: var(--ak-fs-sm, 0.8rem);
      letter-spacing: 0.06em;
      text-transform: uppercase;
      color: var(--ak-text-faint, #9aa3b7);
    }
    section + section {
      margin-top: var(--ak-space-4, 16px);
    }
    dl {
      display: grid;
      grid-template-columns: max-content minmax(0, 1fr);
      gap: var(--ak-space-1, 4px) var(--ak-space-4, 16px);
      margin: 0;
      font-size: var(--ak-fs-sm, 0.8rem);
    }
    dt {
      color: var(--ak-text-dim, #9aa3bd);
    }
    dd {
      margin: 0;
      min-width: 0;
      overflow-wrap: anywhere;
      font-family: var(--ak-font-mono);
      color: var(--ak-text, #e8ebf5);
    }
    .offline {
      margin: 0;
      font-size: var(--ak-fs-sm, 0.8rem);
      color: var(--ak-text-dim, #9aa3bd);
    }
    .close {
      flex: 0 0 auto;
      display: grid;
      place-items: center;
      width: 26px;
      height: 26px;
      padding: 0;
      border: 1px solid var(--ak-border, #2a2f45);
      border-radius: var(--ak-radius-sm, 6px);
      background: var(--ak-surface-2, #0e1220);
      color: inherit;
      font: inherit;
      cursor: pointer;
    }
    .close:focus-visible {
      outline: var(--ak-focus-ring, 1px solid #7c86ff);
      outline-offset: 2px;
    }
  `
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-version-dialog': AkVersionDialog
  }
}
