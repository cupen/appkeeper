/**
 * User preferences (localStorage-backed). The console has exactly three:
 * - refresh interval for list/metric display updates (log following is
 *   never throttled — webui-ui: 刷新频率控制),
 * - the log-rate window the rate columns show,
 * - the log-viewer colour palette (terminal black / paper white / cream —
 *   webui-visual-system: 日志窗口配色档位).
 */

export const REFRESH_CHOICES_MS = [1000, 3000, 5000, 10000] as const
export type RefreshMs = (typeof REFRESH_CHOICES_MS)[number]

export const RATE_WINDOW_CHOICES_S = [1, 10, 60, 300] as const
export type RateWindowS = (typeof RATE_WINDOW_CHOICES_S)[number]

export const LOG_STYLE_CHOICES = ['black', 'white', 'cream'] as const
export type LogStyle = (typeof LOG_STYLE_CHOICES)[number]

const REFRESH_KEY = 'ak.refresh_ms'
const WINDOW_KEY = 'ak.rate_window_s'
const LOG_STYLE_KEY = 'ak.log_style'

function loadNumber(key: string, allowed: readonly number[], fallback: number): number {
  try {
    const raw = localStorage.getItem(key)
    if (raw === null) return fallback
    const v = Number(raw)
    return allowed.includes(v) ? v : fallback
  } catch {
    return fallback
  }
}

function loadChoice<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  try {
    const raw = localStorage.getItem(key)
    if (raw === null) return fallback
    return (allowed as readonly string[]).includes(raw) ? (raw as T) : fallback
  } catch {
    return fallback
  }
}

export function loadRefreshMs(): RefreshMs {
  return loadNumber(REFRESH_KEY, REFRESH_CHOICES_MS, 3000) as RefreshMs
}

export function saveRefreshMs(ms: RefreshMs): void {
  try {
    localStorage.setItem(REFRESH_KEY, String(ms))
  } catch {
    /* private mode etc: preferences become session-only */
  }
}

export function loadRateWindowS(): RateWindowS {
  return loadNumber(WINDOW_KEY, RATE_WINDOW_CHOICES_S, 10) as RateWindowS
}

export function saveRateWindowS(s: RateWindowS): void {
  try {
    localStorage.setItem(WINDOW_KEY, String(s))
  } catch {
    /* session-only */
  }
}

export function loadLogStyle(): LogStyle {
  return loadChoice(LOG_STYLE_KEY, LOG_STYLE_CHOICES, 'black')
}

export function saveLogStyle(style: LogStyle): void {
  try {
    localStorage.setItem(LOG_STYLE_KEY, style)
  } catch {
    /* session-only */
  }
}
