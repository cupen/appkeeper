// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest'
import { nothing, render, type TemplateResult } from 'lit'
import { STATUS_ICON_PX, statusIcon } from './icons.js'

const SLUGS = ['running', 'starting', 'stopping', 'backoff', 'exited', 'stopped', 'fatal']

function draw(slug: string): HTMLElement {
  const host = document.createElement('div')
  render(statusIcon(slug), host)
  return host
}

describe('statusIcon', () => {
  it('renders a 12px template for every lifecycle slug', () => {
    for (const slug of SLUGS) {
      const host = draw(slug)
      const svg = host.querySelector('svg')
      expect(svg, slug).toBeTruthy()
      expect(svg!.getAttribute('width')).toBe(String(STATUS_ICON_PX))
      expect(svg!.getAttribute('height')).toBe(String(STATUS_ICON_PX))
      // Decorative: the word (inside the badge) carries the meaning.
      expect(svg!.getAttribute('aria-hidden')).toBe('true')
    }
  })

  it('draws a distinct silhouette per slug (greyscale survives)', () => {
    // happy-dom does not populate SVG children from an innerHTML binding, so
    // compare the bound path data — a distinct shape per lifecycle state.
    const shapes = new Set(
      SLUGS.map((slug) => {
        const tpl = statusIcon(slug) as TemplateResult
        return JSON.stringify(tpl.values)
      }),
    )
    expect(shapes.size).toBe(SLUGS.length)
  })

  it('returns nothing for an unknown slug', () => {
    expect(statusIcon('queued')).toBe(nothing)
    expect(statusIcon('mystery-state')).toBe(nothing)
    expect(draw('mystery-state').querySelector('svg')).toBeNull()
  })
})
