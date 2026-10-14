/**
 * The credential gate: a modal token prompt shown ONLY while the console
 * reports `prompting` (i.e. the server actually answered 401). With no
 * `webui.auth_token` configured this element renders nothing at all, so the
 * unconfigured console is byte-for-byte the console it was before.
 *
 * The token lives in one local variable for the lifetime of the input and is
 * handed straight to the exchange request — never stored, never in the URL,
 * never logged.
 */

import { LitElement, css, html } from 'lit'
import { customElement, state } from 'lit/decorators.js'
import { cancelCredential, onAuthChange, submitToken } from '../lib/credentials.js'

@customElement('ak-credential-gate')
export class AkCredentialGate extends LitElement {
  @state() private open = false
  @state() private busy = false
  @state() private failed = false

  // @state is load-bearing: the submit button's ?disabled binding reads it,
  // and without it input events never re-render the button.
  @state() private token = ''
  private unsubscribe: (() => void) | null = null

  static styles = css`
    :host {
      display: contents;
    }
    .scrim {
      position: fixed;
      inset: 0;
      display: grid;
      place-items: center;
      background: rgba(8, 10, 18, 0.62);
      z-index: 90;
    }
    .panel {
      width: min(420px, calc(100vw - 32px));
      padding: var(--ak-space-5, 20px);
      border-radius: var(--ak-radius-lg, 12px);
      border: 1px solid var(--ak-border, #2a2f45);
      background: var(--ak-surface, #141826);
      color: var(--ak-text, #e8ebf5);
      box-shadow: 0 18px 48px rgba(0, 0, 0, 0.45);
    }
    h2 {
      margin: 0 0 8px;
      font-size: 15px;
    }
    p {
      margin: 0 0 14px;
      font-size: 13px;
      line-height: 1.5;
      color: var(--ak-text-dim, #9aa3bd);
    }
    label {
      display: block;
      font-size: 12px;
      margin-bottom: 6px;
      color: var(--ak-text-dim, #9aa3bd);
    }
    input {
      width: 100%;
      box-sizing: border-box;
      padding: 8px 10px;
      font: inherit;
      color: inherit;
      background: var(--ak-surface-2, #0e1220);
      border: 1px solid var(--ak-border, #2a2f45);
      border-radius: var(--ak-radius-sm, 6px);
    }
    .row {
      display: flex;
      justify-content: flex-end;
      gap: 8px;
      margin-top: 16px;
    }
    button {
      font: inherit;
      padding: 7px 14px;
      border-radius: var(--ak-radius-sm, 6px);
      border: 1px solid var(--ak-border, #2a2f45);
      background: var(--ak-surface-2, #0e1220);
      color: inherit;
      cursor: pointer;
    }
    button.primary {
      background: var(--ak-accent, #5b64f2);
      border-color: transparent;
      color: #fff;
    }
    button[disabled] {
      opacity: 0.55;
      cursor: not-allowed;
    }
    .error {
      margin-top: 10px;
      font-size: 12px;
      color: var(--ak-danger, #f0616d);
    }
  `

  connectedCallback(): void {
    super.connectedCallback()
    this.open = false
    this.unsubscribe = onAuthChange((state) => {
      if (state === 'prompting') {
        this.token = ''
        this.failed = false
      }
      this.open = state === 'prompting'
    })
  }

  disconnectedCallback(): void {
    this.unsubscribe?.()
    this.unsubscribe = null
    super.disconnectedCallback()
  }

  private async submit(e: Event): Promise<void> {
    e.preventDefault()
    if (this.busy) return
    this.busy = true
    const token = this.token
    // Drop our copy before awaiting; only the request holds it now.
    this.token = ''
    const ok = await submitToken(token)
    this.busy = false
    this.failed = !ok
    if (ok) this.token = ''
  }

  private cancel(): void {
    this.token = ''
    cancelCredential()
  }

  private onInput(e: Event): void {
    this.token = (e.target as HTMLInputElement).value
    this.failed = false
  }

  render() {
    if (!this.open) return html``
    return html`
      <div class="scrim" role="dialog" aria-modal="true" aria-labelledby="ak-gate-title">
        <form class="panel" @submit=${(e: Event) => void this.submit(e)}>
          <h2 id="ak-gate-title">控制台需要凭据</h2>
          <p>
            该控制台配置了 <code>[webui] auth_token</code>。粘贴 token 后会换取一个会话
            cookie（HttpOnly），token 本身不会被浏览器保存。
          </p>
          <label for="ak-gate-token">token</label>
          <input
            id="ak-gate-token"
            type="password"
            autocomplete="off"
            spellcheck="false"
            .value=${this.token}
            @input=${(e: Event) => this.onInput(e)}
          />
          ${this.failed ? html`<div class="error">token 不被接受，请重试。</div>` : ''}
          <div class="row">
            <button type="button" @click=${() => this.cancel()}>取消</button>
            <button type="submit" class="primary" ?disabled=${this.busy || !this.token.trim()}>
              ${this.busy ? '验证中…' : '进入'}
            </button>
          </div>
        </form>
      </div>
    `
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-credential-gate': AkCredentialGate
  }
}
