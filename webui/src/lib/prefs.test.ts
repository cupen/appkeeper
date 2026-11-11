import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  loadLogStyle,
  loadRateWindowS,
  loadRefreshMs,
  saveLogStyle,
  saveRateWindowS,
  saveRefreshMs,
  LOG_STYLE_CHOICES,
  REFRESH_CHOICES_MS,
  RATE_WINDOW_CHOICES_S,
} from './prefs.js'

describe('prefs', () => {
  beforeEach(() => {
    localStorage.clear()
  })

  it('defaults to 3s refresh, 10s rate window and the black log palette', () => {
    expect(loadRefreshMs()).toBe(3000)
    expect(loadRateWindowS()).toBe(10)
    expect(loadLogStyle()).toBe('black')
  })

  it('persists and reloads a chosen value', () => {
    saveRefreshMs(10000)
    saveRateWindowS(60)
    expect(loadRefreshMs()).toBe(10000)
    expect(loadRateWindowS()).toBe(60)
    // The value is really in localStorage (survives a page reload).
    expect(localStorage.getItem('ak.refresh_ms')).toBe('10000')
  })

  it('falls back to defaults on out-of-vocabulary values', () => {
    localStorage.setItem('ak.refresh_ms', '7000')
    localStorage.setItem('ak.rate_window_s', '42')
    expect(loadRefreshMs()).toBe(3000)
    expect(loadRateWindowS()).toBe(10)
  })

  it('offers exactly the spec choices (webui-ui: 刷新频率控制)', () => {
    expect([...REFRESH_CHOICES_MS]).toEqual([1000, 3000, 5000, 10000])
    expect([...RATE_WINDOW_CHOICES_S]).toEqual([1, 10, 60, 300])
    // webui-visual-system: 日志窗口配色档位 — terminal black / paper white / cream.
    expect([...LOG_STYLE_CHOICES]).toEqual(['black', 'white', 'cream'])
  })

  it('persists and reloads the log palette (webui-ui: 日志查看)', () => {
    saveLogStyle('cream')
    expect(loadLogStyle()).toBe('cream')
    expect(localStorage.getItem('ak.log_style')).toBe('cream')
  })

  it('falls back to black on an out-of-vocabulary log palette', () => {
    localStorage.setItem('ak.log_style', 'neon')
    expect(loadLogStyle()).toBe('black')
  })

  it('degrades session-only when localStorage throws', () => {
    const getItem = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('disabled')
    })
    const setItem = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('disabled')
    })
    expect(loadLogStyle()).toBe('black')
    expect(() => saveLogStyle('white')).not.toThrow()
    getItem.mockRestore()
    setItem.mockRestore()
  })
})
