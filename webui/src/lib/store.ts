/**
 * Console store: merges WS snapshots/deltas into one StatusDoc, owns the
 * transport lifecycle (WS preferred, periodic polling as fallback), and
 * exposes log subscriptions to viewers. Framework-agnostic — Lit
 * components bind through `consoleController`.
 *
 * Transport rules (webui-ui: 状态实时刷新 + 后端不可达降级):
 * - WS open   → live (snapshot then deltas, server-side cadence),
 * - WS down   → poll `/api/overview` at the user's refresh interval and flag
 *               the doc as polled so the UI can show a degraded banner,
 * - WS back   → stop polling; the fresh snapshot clears staleness.
 */

import { apiFetch, apiJson, removeApp, type RemoveAppResult } from './api.js'
import { loadRefreshMs, saveRefreshMs, type RefreshMs } from './prefs.js'
import type { PendingDoc, ProgramInfo, StatusDoc, StreamId } from './types.js'
import { WsClient, type WsState } from './ws-client.js'
import type { ReactiveController, ReactiveControllerHost } from 'lit'

export type Transport = 'connecting' | 'live' | 'poll' | 'offline'

export interface ConsoleSnapshot {
  doc: StatusDoc | null
  transport: Transport
}

export type LogListener = (frame: { program: string; stream: StreamId; text: string }) => void

function wsUrl(): string {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${proto}//${location.host}/ws`
}

export class ConsoleStore {
  private doc: StatusDoc | null = null
  private transport: Transport = 'connecting'
  private listeners = new Set<() => void>()
  private client: WsClient | null = null
  private pollTimer: ReturnType<typeof setInterval> | null = null
  private logListeners = new Set<LogListener>()
  private gapListeners = new Set<(program: string, stream: StreamId, skipped: number) => void>()
  private refreshMs: RefreshMs = loadRefreshMs()
  private logFrames: { program: string; stream: StreamId; text: string }[] = []
  private flushScheduled = false
  private flashNotice: { ok: boolean; text: string } | null = null
  /** Apps removed since the last snapshot (STATUS `apps_gone`). */
  private removedApps = new Set<string>()

  /** Log subscriptions survive reconnects: WS resubscribes on snapshot. */
  private subscriptions = new Set<string>() // `${program}\u0000${stream}`

  start(): void {
    if (this.client) return
    this.client = new WsClient(wsUrl(), {
      snapshot: (doc) => {
        this.removedApps.clear()
        this.doc = {
          daemon: doc.daemon,
          programs: [...doc.programs].sort(byAppThenName),
          pending: doc.pending ?? emptyPending(),
        }
        this.setTransport('live')
        this.stopPolling()
        // A fresh connection resets log routing; re-subscribe viewers.
        for (const key of this.subscriptions) {
          const idx = key.indexOf('\u0000')
          this.client?.subscribe(key.slice(0, idx), Number(key[idx + 1]) as 0 | 1)
        }
        this.emit()
      },
      delta: (programs, pending, removed, appsGone) => {
        if (!this.doc) return
        this.mergeDelta(programs, removed)
        if (appsGone?.length) {
          // A removed app record is authoritative even when it declared no
          // programs (nothing to drop from the projection) — remember it so
          // the views can converge and navigate away.
          for (const a of appsGone) this.removedApps.add(a)
        }
        if (pending !== undefined) this.doc = { ...this.doc, pending }
        this.emit()
      },
      log: (program, stream, text) => {
        this.logFrames.push({ program, stream, text })
        this.flushLogFrames()
      },
      gap: (program, stream, skipped) => {
        for (const g of this.gapListeners) g(program, stream, skipped)
      },
      error: () => {
        /* frame-level errors surface as logs; nothing actionable */
      },
      handshakeRefused: () => {
        // Distinguish "needs a credential" from "backend down" with one
        // REST probe: only a 401 here raises the gate (apiFetch owns the
        // prompt + replay). Any other failure is the offline path the
        // transport banner already covers.
        void apiFetch('/api/overview')
          .then((res) => {
            if (res.status === 401 && this.client) this.client.connect()
          })
          .catch(() => {
            /* offline: stateChange already flipped the transport banner */
          })
      },
      stateChange: (state: WsState) => {
        if (state === 'open') return // snapshot handler flips to live
        if (state === 'closed') {
          this.setTransport(this.doc ? 'poll' : 'offline')
          this.startPolling()
        }
      },
    })
    this.client.connect()
  }

  stop(): void {
    this.client?.close()
    this.client = null
    this.stopPolling()
  }

  // --ak- reactive glue --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak-

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  getSnapshot(): ConsoleSnapshot {
    return { doc: this.doc, transport: this.transport }
  }

  get refreshInterval(): RefreshMs {
    return this.refreshMs
  }

  setRefreshInterval(ms: RefreshMs): void {
    this.refreshMs = ms
    saveRefreshMs(ms)
    // Polling cadence follows the preference; WS cadence is server-side.
    if (this.pollTimer) {
      this.stopPolling()
      this.startPolling()
    }
    this.emit()
  }

  // --ak- apply --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak--

  /** The current pending projection (never null; empty when in sync). */
  get pending(): PendingDoc {
    return this.doc?.pending ?? emptyPending()
  }

  /**
   * A one-shot console notice that survives navigation (e.g. "app removed; no
   * apply needed" after the overview falls back to the dashboard). Rendered by
   * the overview bar until dismissed.
   */
  get flash(): { ok: boolean; text: string } | null {
    return this.flashNotice
  }

  setFlash(text: string, ok = true): void {
    this.flashNotice = { ok, text }
    this.emit()
  }

  clearFlash(): void {
    if (!this.flashNotice) return
    this.flashNotice = null
    this.emit()
  }

  /**
   * Whether [`app`] was removed since the last snapshot (STATUS `apps_gone`).
   * The shell uses it to leave a page whose app no longer exists, including
   * one that declared no programs at all.
   */
  wasAppRemoved(app: string): boolean {
    return this.removedApps.has(app)
  }

  /**
   * Apply pending changes via the control-plane API. Scope mirrors the CLI:
   * app + optional program restrict the blast radius; restart also restarts
   * unchanged programs (user-stopped ones stay stopped — server semantics).
   * Returns the rendered result text (or throws with the API error).
   */
  async apply(opts: { app?: string; program?: string; restart?: boolean } = {}): Promise<string> {
    const body: Record<string, unknown> = { restart: opts.restart ?? false }
    if (opts.app) body['app'] = opts.app
    if (opts.program) body['program'] = opts.program
    const v = await apiJson<{ result?: string; error?: string }>('/api/apply', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })
    // The WS delta / next poll refreshes state; nudge immediately for UI
    // snappiness by re-polling the overview once.
    void this.pollOnceIfNotLive()
    return v?.result ?? 'applied'
  }

  private async pollOnceIfNotLive(): Promise<void> {
    if (this.transport === 'live' && this.client) return
    await this.pollOnce()
  }

  /**
   * Remove one whole app through the Web endpoint
   * (guard-remove-until-stopped). The daemon owns the guard and the result;
   * this never stops anything and never applies afterwards. A 409/404 throws
   * an `ApiError` carrying the daemon's message, and the caller keeps the
   * page as-is.
   *
   * Live WS sessions receive the removal as a STATUS delta with `removed`;
   * degraded (polling) sessions are refreshed here so the removed app cannot
   * linger.
   */
  async removeApp(app: string): Promise<RemoveAppResult> {
    const result = await removeApp(app)
    // The server already dropped the record and these programs; apply the
    // same removal locally so the sidebar/overview converge immediately even
    // before the next WS delta (a 2xx answer is authoritative).
    this.removedApps.add(app)
    if (result.programs?.length) {
      this.mergeDelta([], result.programs)
    }
    this.emit()
    await this.pollOnceIfNotLive()
    return result
  }

  // --ak- log subscriptions --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak-

  subscribeLogs(program: string, stream: StreamId): void {
    const key = `${program}\u0000${stream}`
    this.subscriptions.add(key)
    this.client?.subscribe(program, stream)
  }

  unsubscribeLogs(program: string, stream: StreamId): void {
    const key = `${program}\u0000${stream}`
    this.subscriptions.delete(key)
    this.client?.unsubscribe(program, stream)
  }

  onLogFrame(l: LogListener): () => void {
    this.logListeners.add(l)
    return () => this.logListeners.delete(l)
  }

  onGap(l: (program: string, stream: StreamId, skipped: number) => void): () => void {
    this.gapListeners.add(l)
    return () => this.gapListeners.delete(l)
  }

  /** Batch frames per microtask so viewers append in render-sized lumps. */
  private flushLogFrames(): void {
    if (this.flushScheduled) return
    this.flushScheduled = true
    queueMicrotask(() => {
      this.flushScheduled = false
      for (const f of this.logFrames) {
        for (const l of this.logListeners) l(f)
      }
      this.logFrames = []
    })
  }

  // --ak- internals --ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak---ak--

  /**
   * Merge one STATUS delta. `removed` carries program names the server dropped
   * (a whole app was removed); they must be deleted from the projection — an
   * additive merge would leave stale, still-clickable rows behind
   * (guard-remove-until-stopped 4.3).
   */
  private mergeDelta(programs: ProgramInfo[], removed?: string[]): void {
    if (!this.doc) return
    const byName = new Map(this.doc.programs.map((p) => [p.name, p]))
    for (const name of removed ?? []) byName.delete(name)
    for (const p of programs) byName.set(p.name, p)
    this.doc = {
      daemon: this.doc.daemon,
      programs: [...byName.values()].sort(byAppThenName),
      pending: this.doc.pending,
    }
  }

  private async pollOnce(): Promise<void> {
    try {
      const res = await apiFetch('/api/overview')
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      const doc = (await res.json()) as StatusDoc
      // A poll returns a whole document, so it supersedes the incremental
      // removal bookkeeping (absent apps are simply absent).
      this.removedApps.clear()
      this.doc = {
        daemon: doc.daemon,
        programs: [...doc.programs].sort(byAppThenName),
        pending: doc.pending ?? emptyPending(),
      }
      // Polling succeeded → the backend is reachable (degraded but alive).
      if (this.transport !== 'live') this.setTransport('poll')
      this.emit()
    } catch {
      this.setTransport('offline')
      this.emit()
    }
  }

  private startPolling(): void {
    if (this.pollTimer) return
    void this.pollOnce()
    this.pollTimer = setInterval(() => void this.pollOnce(), this.refreshMs)
  }

  private stopPolling(): void {
    if (this.pollTimer) clearInterval(this.pollTimer)
    this.pollTimer = null
  }

  private setTransport(t: Transport): void {
    if (this.transport === t) return
    this.transport = t
    this.emit()
  }

  private emit(): void {
    for (const l of this.listeners) l()
  }
}

function byAppThenName(a: ProgramInfo, b: ProgramInfo): number {
  return (a.app + '\u0000' + a.name).localeCompare(b.app + '\u0000' + b.name)
}

function emptyPending(): PendingDoc {
  return { programs: [], apps_added: [], apps_removed: [], daemon_hints: [], errors: [] }
}

/** Lit reactive controller: bind a component to the console store. */
export class ConsoleController implements ReactiveController {
  private unsubscribe: (() => void) | null = null

  constructor(
    private host: ReactiveControllerHost,
    private store: ConsoleStore,
  ) {
    host.addController(this)
  }

  get snapshot(): ConsoleSnapshot {
    return this.store.getSnapshot()
  }

  hostConnected(): void {
    this.store.start()
    this.unsubscribe = this.store.subscribe(() => this.host.requestUpdate())
    this.host.requestUpdate()
  }

  hostDisconnected(): void {
    this.unsubscribe?.()
    this.unsubscribe = null
  }
}

/** Shared singleton for the app shell + views. */
export const consoleStore = new ConsoleStore()
