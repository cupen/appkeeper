// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest'
import { AkStatusBadge } from './status-badge.js'

const SLUGS = ['starting', 'queued', 'working', 'done', 'failed', 'dormant']

function mount(slug: string, label = '', glyph = ''): AkStatusBadge {
  const el = document.createElement('ak-status-badge') as AkStatusBadge
  el.slug = slug
  el.label = label
  el.glyph = glyph
  document.body.appendChild(el)
  return el
}

describe('ak-status-badge', () => {
  it('renders the word, a status icon and the data-status hook', async () => {
    const el = mount('running', '运行中')
    await el.updateComplete
    expect(el.shadowRoot?.querySelector('.label')?.textContent).toBe('运行中')
    // The word is the accessible channel — never colour-only.
    expect(el.shadowRoot?.textContent).toContain('运行中')
    expect(el.shadowRoot?.querySelector('[data-status="running"]')).toBeTruthy()
    // Shape is an inline SVG inside the fixed slot, not a font glyph.
    expect(el.shadowRoot?.querySelector('.glyph svg')).toBeTruthy()
    el.remove()
  })

  it('keeps the 12px icon slot at a fixed width', () => {
    const cssText = (AkStatusBadge.styles as unknown as { cssText: string }).cssText
    expect(cssText).toContain('flex: 0 0 12px')
    expect(cssText).toContain('width: 12px')
  })

  it('resolves a distinct CSS variable per legacy slug', async () => {
    const colors = new Set<string>()
    for (const slug of SLUGS) {
      const el = mount(slug, slug)
      await el.updateComplete
      const dot = el.shadowRoot?.querySelector<HTMLElement>('.dot')
      expect(dot?.getAttribute('style')).toContain(`--ak-status-${slug}`)
      colors.add(dot?.getAttribute('style') ?? '')
      el.remove()
    }
    // Distinct variables per slug — the greyscale/colour-blindness channel.
    expect(colors.size).toBe(SLUGS.length)
  })

  it('falls back to the slug as label when none provided', async () => {
    const el = mount('done')
    await el.updateComplete
    expect(el.shadowRoot?.textContent).toContain('done')
    el.remove()
  })

  it('renders a caller glyph for a slug outside the icon set, with the raw slug', async () => {
    const el = mount('mystery', '', '◌')
    await el.updateComplete
    expect(el.shadowRoot?.querySelector('.glyph')?.textContent).toBe('◌')
    expect(el.shadowRoot?.querySelector('.label')?.textContent).toBe('mystery')
    el.remove()
  })

  it('renders nothing for an empty slug and empty label (no empty badge)', async () => {
    const el = mount('')
    await el.updateComplete
    expect(el.shadowRoot?.querySelector('.badge')).toBeNull()
    el.remove()
  })
})
