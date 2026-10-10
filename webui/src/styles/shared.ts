/**
 * Shared view style kit. Every routed view composes `viewStyles` with its
 * own `css` fragment so panels, tables, empty states, skeletons, callouts
 * and key/value rows look identical everywhere. These live inside each
 * component's shadow root, so the focus-visible rules here cover anchors
 * and focusable elements that the document-level rule cannot reach.
 */

import { css } from 'lit'

export const viewStyles = css`
  :host {
    display: block;
  }

  /* --ak---ak- Page header --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak-- */
  .page-head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: var(--ak-space-4);
    flex-wrap: wrap;
    margin-bottom: var(--ak-space-5);
  }
  .page-title {
    margin: 0;
    font-size: 1.25rem;
    font-weight: 650;
    letter-spacing: -0.015em;
    color: var(--ak-text-bright);
  }
  .page-sub {
    margin: var(--ak-space-1) 0 0;
    color: var(--ak-text-dim);
    font-size: 0.875rem;
  }

  /* --ak---ak- Panels --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak-- */
  .panel {
    background: var(--ak-surface);
    border: 1px solid var(--ak-border);
    border-radius: var(--ak-radius-lg);
    box-shadow: var(--ak-shadow-1);
    overflow: hidden;
  }
  /* 8.1: at narrow widths a wide child (e.g. a data table) must stay
     reachable — clip only vertically, let the user swipe horizontally. */
  @media (max-width: 640px) {
    .panel {
      overflow-x: auto;
    }
  }
  .panel-pad {
    padding: var(--ak-space-4);
  }
  .panel-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--ak-space-3);
    flex-wrap: wrap;
    padding: var(--ak-space-3) var(--ak-space-4);
    border-bottom: 1px solid var(--ak-border);
  }
  .panel-title {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--ak-text-bright);
  }
  .panel-caption {
    color: var(--ak-text-faint);
    font-size: 0.8rem;
  }

  /* --ak---ak- Tables --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak-- */
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: var(--ak-space-3) var(--ak-space-4);
    border-bottom: 1px solid var(--ak-border);
    vertical-align: middle;
  }
  th {
    color: var(--ak-text-faint);
    font-weight: 550;
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    background: var(--ak-surface-2);
    white-space: nowrap;
  }
  tbody tr {
    transition: background var(--ak-dur) var(--ak-ease);
  }
  tbody tr:hover {
    background: var(--ak-surface-2);
  }
  tbody tr:last-child td {
    border-bottom: none;
  }
  /* Status accent on the leading edge of a row (set data-status on the tr). */
  tbody tr[data-status] td:first-child {
    box-shadow: inset 2px 0 0 0 var(--ak-status-dormant);
  }
  tbody tr[data-status='starting'] td:first-child {
    box-shadow: inset 2px 0 0 0 var(--ak-status-starting);
  }
  tbody tr[data-status='queued'] td:first-child {
    box-shadow: inset 2px 0 0 0 var(--ak-status-queued);
  }
  tbody tr[data-status='working'] td:first-child {
    box-shadow: inset 2px 0 0 0 var(--ak-status-working);
  }
  tbody tr[data-status='done'] td:first-child {
    box-shadow: inset 2px 0 0 0 var(--ak-status-done);
  }
  tbody tr[data-status='failed'] td:first-child {
    box-shadow: inset 2px 0 0 0 var(--ak-status-failed);
  }
  tbody tr[data-status='dormant'] td:first-child {
    box-shadow: inset 2px 0 0 0 var(--ak-status-dormant);
  }

  /* --ak---ak- Typography helpers --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak- */
  .mono {
    font-family: var(--ak-font-mono);
    font-size: 0.82rem;
  }
  .tnum {
    font-variant-numeric: tabular-nums;
  }
  .dim {
    color: var(--ak-text-dim);
  }

  /* --ak---ak- Links --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak- */
  a {
    color: var(--ak-accent);
    text-decoration: none;
    transition: color var(--ak-dur) var(--ak-ease);
  }
  a:hover {
    color: var(--ak-accent-hover);
    text-decoration: underline;
    text-underline-offset: 3px;
  }
  a:focus-visible {
    outline: var(--ak-focus-ring);
    outline-offset: 2px;
    border-radius: var(--ak-radius-sm);
  }
  button:focus-visible,
  textarea:focus-visible,
  input:focus-visible,
  select:focus-visible,
  [tabindex]:focus-visible {
    outline: var(--ak-focus-ring);
    outline-offset: 2px;
  }

  /* --ak---ak- Empty states --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak- */
  .empty {
    padding: var(--ak-space-10) var(--ak-space-6);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--ak-space-2);
    text-align: center;
    color: var(--ak-text-dim);
  }
  .empty .glyph {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border-radius: var(--ak-radius-full);
    background: var(--ak-surface-2);
    border: 1px solid var(--ak-border);
    color: var(--ak-text-faint);
    margin-bottom: var(--ak-space-2);
  }
  .empty .title {
    color: var(--ak-text-bright);
    font-weight: 600;
    font-size: 0.95rem;
  }
  .empty .hint {
    margin: 0;
    font-size: 0.85rem;
    max-width: 42ch;
  }
  .empty .cta {
    margin-top: var(--ak-space-3);
  }

  /* --ak---ak- Loading skeletons --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak-- */
  .skel {
    border-radius: var(--ak-radius-sm);
    background: linear-gradient(
      90deg,
      var(--ak-surface-2) 25%,
      var(--ak-surface-3) 45%,
      var(--ak-surface-2) 65%
    );
    background-size: 200% 100%;
    animation: appkeeper-shimmer 1.4s ease-in-out infinite;
  }
  .skel-row {
    display: flex;
    gap: var(--ak-space-4);
    padding: var(--ak-space-4);
    border-bottom: 1px solid var(--ak-border);
  }
  .skel-row:last-child {
    border-bottom: none;
  }
  .skel-line {
    height: 12px;
  }
  @keyframes appkeeper-shimmer {
    from {
      background-position: 180% 0;
    }
    to {
      background-position: -80% 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .skel {
      animation: none;
    }
  }

  /* --ak---ak- Callouts --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak- */
  .callout {
    display: flex;
    align-items: flex-start;
    gap: var(--ak-space-2);
    padding: var(--ak-space-3) var(--ak-space-4);
    border-radius: var(--ak-radius-md);
    border: 1px solid;
    font-size: 0.875rem;
    margin: 0 0 var(--ak-space-4);
  }
  .callout svg {
    flex: 0 0 auto;
    margin-top: 2px;
  }
  .callout-error {
    color: var(--ak-status-failed);
    background: var(--ak-status-failed-bg);
    border-color: var(--ak-status-failed-border);
  }
  .callout-warn,
  .callout-warning {
    color: var(--ak-status-working);
    background: var(--ak-status-working-bg);
    border-color: var(--ak-status-working-border);
  }
  .callout-info {
    color: var(--ak-status-done);
    background: var(--ak-status-done-bg);
    border-color: var(--ak-status-done-border);
  }

  /* --ak---ak- Chips --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak- */
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px;
    border-radius: var(--ak-radius-full);
    border: 1px solid var(--ak-border);
    background: var(--ak-surface-2);
    color: var(--ak-text-dim);
    font-size: 0.78rem;
    white-space: nowrap;
  }
  .chip .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--ak-text-faint);
  }
  .chip b {
    color: var(--ak-text-bright);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  /* --ak---ak- Key/value rows --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak- */
  .kv {
    display: grid;
    grid-template-columns: minmax(140px, 220px) 1fr;
    gap: var(--ak-space-2) var(--ak-space-6);
    padding: var(--ak-space-3) 0;
    border-bottom: 1px solid var(--ak-border);
    margin: 0;
  }
  .kv:last-child {
    border-bottom: none;
  }
  .kv dt {
    color: var(--ak-text-dim);
    font-size: 0.875rem;
  }
  .kv dd {
    margin: 0;
    color: var(--ak-text);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
`
