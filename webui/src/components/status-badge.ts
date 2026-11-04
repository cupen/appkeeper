/**
 * Operator-facing status badge. Renders the backend-owned status projection
 * as a tinted pill: a 6px dot, a fixed 12px icon slot and the word — so the
 * state survives greyscale and colour-blindness (shape + word + colour) and
 * the glyph's font baseline can no longer push the row height around.
 *
 * `label` and `glyph` stay caller-provided (the backend/UI vocabulary is not
 * duplicated here): `label` is the word, `glyph` is only a fallback for slugs
 * outside the built-in icon set. An unknown slug renders its raw text —
 * never an empty badge.
 *
 * Pill background/border/text come from the per-slug status tokens via the
 * data-status attribute; the dot keeps an inline `--ak-status-*` reference
 * (stylesheet hook, mirrors the SSR projection).
 */

import { LitElement, css, html, nothing } from 'lit'
import { customElement, property } from 'lit/decorators.js'
import { statusIcon } from './icons.js'

/**
 * The status vocabulary is owned by the backend (single source); the badge
 * renders whatever slug the API sends. Known slugs get per-slug tokens from
 * the stylesheet; unknown slugs fall back to the neutral default.
 */
export type StatusSlug = string

@customElement('ak-status-badge')
export class AkStatusBadge extends LitElement {
  @property() slug: StatusSlug = ''
  @property() label = ''
  /** Caller glyph, used only when the slug has no built-in 12px icon. */
  @property() glyph = ''

  static styles = css`
    .badge {
      display: inline-flex;
      align-items: center;
      gap: var(--ak-space-2, 8px);
      padding: var(--ak-space-1, 4px) var(--ak-space-2, 8px);
      border-radius: var(--ak-radius-full, 999px);
      border: 1px solid var(--ak-status-dormant-border, rgba(148, 160, 180, 0.28));
      background: var(--ak-status-dormant-bg, rgba(148, 160, 180, 0.1));
      color: var(--ak-status-dormant, #94a0b4);
      font-size: var(--ak-fs-xs, 0.72rem);
      font-weight: 550;
      letter-spacing: 0.01em;
      line-height: 1.4;
      font-variant-numeric: tabular-nums;
      white-space: nowrap;
      transition:
        background var(--ak-dur, 150ms) var(--ak-ease, ease),
        border-color var(--ak-dur, 150ms) var(--ak-ease, ease);
    }
    .badge[data-status='starting'] {
      background: var(--ak-status-starting-bg);
      border-color: var(--ak-status-starting-border);
      color: var(--ak-status-starting);
    }
    .badge[data-status='queued'] {
      background: var(--ak-status-queued-bg);
      border-color: var(--ak-status-queued-border);
      color: var(--ak-status-queued);
    }
    .badge[data-status='working'] {
      background: var(--ak-status-working-bg);
      border-color: var(--ak-status-working-border);
      color: var(--ak-status-working);
    }
    .badge[data-status='done'] {
      background: var(--ak-status-done-bg);
      border-color: var(--ak-status-done-border);
      color: var(--ak-status-done);
    }
    .badge[data-status='failed'] {
      background: var(--ak-status-failed-bg);
      border-color: var(--ak-status-failed-border);
      color: var(--ak-status-failed);
    }
    .badge[data-status='dormant'] {
      background: var(--ak-status-dormant-bg);
      border-color: var(--ak-status-dormant-border);
      color: var(--ak-status-dormant);
    }
    /* Daemon lifecycle slugs (process-management capability) map onto the
       same status token families — one visual language for all states. */
    .badge[data-status='running'] {
      background: var(--ak-status-done-bg);
      border-color: var(--ak-status-done-border);
      color: var(--ak-status-done);
    }
    .badge[data-status='starting'],
    .badge[data-status='stopping'] {
      background: var(--ak-status-starting-bg);
      border-color: var(--ak-status-starting-border);
      color: var(--ak-status-starting);
    }
    .badge[data-status='backoff'] {
      background: var(--ak-status-working-bg);
      border-color: var(--ak-status-working-border);
      color: var(--ak-status-working);
    }
    .badge[data-status='exited'],
    .badge[data-status='stopped'] {
      background: var(--ak-status-dormant-bg);
      border-color: var(--ak-status-dormant-border);
      color: var(--ak-status-dormant);
    }
    .badge[data-status='fatal'] {
      background: var(--ak-status-failed-bg);
      border-color: var(--ak-status-failed-border);
      color: var(--ak-status-failed);
    }
    /* Fixed icon slot: 12px wide regardless of glyph or font, so rows never
       jitter when the status word changes. */
    .glyph {
      flex: 0 0 12px;
      width: 12px;
      height: 12px;
      display: inline-grid;
      place-items: center;
      line-height: 1;
    }
    .dot {
      position: relative;
      width: 6px;
      height: 6px;
      border-radius: 50%;
      background: var(--ak-status-dormant, #94a0b4);
      flex: 0 0 auto;
    }
    /* Live pulse on the working status — CSS only, honours reduced motion. */
    .badge[data-status='working'] .dot::before {
      content: '';
      position: absolute;
      inset: -3px;
      border-radius: 50%;
      border: 1px solid var(--ak-status-working, currentColor);
      animation: appkeeper-ping 1.6s var(--ak-ease, ease) infinite;
    }
    @keyframes appkeeper-ping {
      0% {
        transform: scale(0.55);
        opacity: 0.9;
      }
      80%,
      100% {
        transform: scale(1.7);
        opacity: 0;
      }
    }
    @media (prefers-reduced-motion: reduce) {
      .badge[data-status='working'] .dot::before {
        animation: none;
        opacity: 0;
      }
    }
    .label {
      font-size: var(--ak-fs-xs, 0.72rem);
    }
  `

  private get statusColor(): string {
    const slug = this.slug.replace(/[^a-z]/g, '')
    return `var(--ak-status-${slug}, var(--ak-status-dormant, #94a0b4))`
  }

  render() {
    // The word is the accessible text; an unknown slug reads back verbatim
    // (webui-ui: 不认识的 slug 原样显示，不得渲染为空).
    const label = this.label || this.slug
    if (!label) return nothing
    // Built-in 12px SVG wins; a caller glyph is the fallback for other slugs.
    const icon = statusIcon(this.slug)
    const mark = icon !== nothing ? icon : this.glyph || nothing
    return html`<span class="badge" data-status=${this.slug}>
      <span class="dot" style=${`background: ${this.statusColor}`} aria-hidden="true"></span>
      ${mark === nothing
        ? nothing
        : html`<span class="glyph" aria-hidden="true">${mark}</span>`}
      <span class="label">${label}</span>
    </span>`
  }
}

declare global {
  interface HTMLElementTagNameMap {
    'ak-status-badge': AkStatusBadge
  }
}
