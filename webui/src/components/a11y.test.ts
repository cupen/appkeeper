// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import './status-badge.js'
import type { AkStatusBadge } from './status-badge.js'

describe('accessibility baseline', () => {
  it('status badge text remains available in greyscale (label + shape nodes)', async () => {
    const el = document.createElement('ak-status-badge') as AkStatusBadge
    el.slug = 'fatal'
    el.label = '故障'
    el.glyph = '✕'
    document.body.appendChild(el)
    await el.updateComplete
    const label = el.shadowRoot?.querySelector('.label')
    const shape = el.shadowRoot?.querySelector('.glyph svg')
    // Word and shape nodes both exist: greyscale cannot erase the state.
    expect(label?.textContent).toBe('故障')
    expect(shape).toBeTruthy()
    el.remove()
  })

  it('reduced motion + theme class hooks ship in tokens.css, focus ring in app.css', async () => {
    const fs = await import('node:fs')
    const path = await import('node:path')
    const { fileURLToPath } = await import('node:url')
    const here = path.dirname(fileURLToPath(import.meta.url))
    const tokens = fs.readFileSync(path.join(here, '../../src/styles/tokens.css'), 'utf8')
    expect(tokens).toContain('prefers-reduced-motion')
    // Light re-map is keyed on the single wa-dark switch (see src/theme.ts),
    // so an explicit appearance override can beat the OS preference.
    expect(tokens).toContain(':root:not(.wa-dark)')
    const app = fs.readFileSync(path.join(here, '../../src/styles/app.css'), 'utf8')
    expect(app).toContain(':focus-visible')
  })
})
