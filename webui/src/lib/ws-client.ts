/**
 * Browser WebSocket client for the `/ws` push channel (webui-api). Owns:
 * - connect/reconnect with exponential backoff (server resends a full
 *   snapshot on every new connection — no cross-connection resume),
 * - ordered frame decoding (type byte, zlib inflate, msgpack / raw log),
 * - log subscription management (subscribe / unsubscribe actions),
 * - heartbeat-based liveness: silence beyond the server heartbeat period
 *   means the connection is zombie — drop and reconnect.
 *
 * The client is transport only: it reports events upward and holds no
 * domain state (the store merges snapshots/deltas).
 *
 * Credentials: the browser carries the console's session cookie on the
 * handshake automatically (same origin, no `Sec-WebSocket-Protocol` hack and
 * no token in the URL — a token in a URL lands in history and access logs).
 * A refused handshake is reported through `handshakeRefused` so the store can
 * route it into the same credential flow the HTTP calls use.
 */

import {
  decodeDoc,
  decodeFrame,
  decodeGapFrame,
  decodeLogFrame,
  encodeAction,
} from './frames.js'
import { MSG, type PendingDoc, type ProgramInfo, type StatusDoc } from './types.js'

export type WsState = 'connecting' | 'open' | 'closed'

export interface WsClientHandlers {
  snapshot(doc: StatusDoc): void
  /**
   * Incremental update. `removed` names programs that disappeared from the
   * projection and `appsGone` the app records that were removed
   * (guard-remove-until-stopped); the store MUST drop those entries rather
   * than keeping stale, clickable rows.
   */
  delta(programs: ProgramInfo[], pending?: PendingDoc, removed?: string[], appsGone?: string[]): void
  log(program: string, stream: 0 | 1, text: string): void
  gap(program: string, stream: 0 | 1, skipped: number): void
  error(message: string): void
  stateChange(state: WsState): void
  /**
   * The handshake was refused — the console asked for a credential (401 on
   * the upgrade) or nothing is serving `/ws`. The caller decides which: it
   * probes the REST surface, and only a 401 there raises the gate. That keeps
   * a plain network blip from being reported as an auth problem.
   */
  handshakeRefused(): void
}

const BACKOFF_BASE_MS = 500
const BACKOFF_MAX_MS = 10_000
/** Server heartbeats every 30s; silence past this means a dead link. */
const SILENCE_LIMIT_MS = 45_000

export class WsClient {
  private ws: WebSocket | null = null
  private handlers: WsClientHandlers
  private url: string
  private closedByUser = false
  private attempts = 0
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private silenceTimer: ReturnType<typeof setTimeout> | null = null
  /** Serializes async frame decoding so ordering survives zlib inflation. */
  private decodeChain: Promise<void> = Promise.resolve()
  /** Set once the socket has actually opened at least once. */
  private everOpened = false

  constructor(url: string, handlers: WsClientHandlers) {
    this.url = url
    this.handlers = handlers
  }

  connect(): void {
    this.closedByUser = false
    this.open()
  }

  close(): void {
    this.closedByUser = true
    this.clearTimers()
    this.ws?.close()
    this.ws = null
  }

  subscribe(program: string, stream: 0 | 1): void {
    this.sendAction('subscribe', program, stream)
  }

  unsubscribe(program: string, stream: 0 | 1): void {
    this.sendAction('unsubscribe', program, stream)
  }

  private sendAction(action: 'subscribe' | 'unsubscribe', program: string, stream: 0 | 1): void {
    if (this.ws?.readyState !== WebSocket.OPEN) return
    // encodeAction builds on plain ArrayBuffers, never SharedArrayBuffer.
    this.ws.send(encodeAction(action, program, stream) as Uint8Array<ArrayBuffer>)
  }

  private open(): void {
    this.setState('connecting')
    let ws: WebSocket
    try {
      ws = new WebSocket(this.url)
    } catch {
      this.scheduleReconnect()
      return
    }
    ws.binaryType = 'arraybuffer'
    this.ws = ws

    ws.onopen = () => {
      this.attempts = 0
      this.everOpened = true
      this.armSilenceTimer()
      this.setState('open')
    }
    ws.onmessage = (ev) => {
      this.armSilenceTimer()
      const data =
        ev.data instanceof ArrayBuffer
          ? new Uint8Array(ev.data)
          : new TextEncoder().encode(String(ev.data))
      // Chain decodes: DecompressionStream is async and could reorder
      // frames if run concurrently — snapshot-before-delta must hold.
      this.decodeChain = this.decodeChain.then(() => this.handleFrame(data)).catch(() => {
        /* a bad frame is logged and skipped; the stream continues */
      })
    }
    ws.onerror = () => {
      /* onclose follows; nothing to do here */
    }
    ws.onclose = () => {
      this.ws = null
      if (this.closedByUser) {
        this.setState('closed')
        return
      }
      // Refused before it ever opened: either the console wants a
      // credential, or `/ws` is simply not reachable. The store probes the
      // REST surface to tell those apart.
      if (!this.everOpened) this.handlers.handshakeRefused()
      this.scheduleReconnect()
    }
  }

  private async handleFrame(data: Uint8Array): Promise<void> {
    let type: number
    let payload: Uint8Array
    try {
      const decoded = await decodeFrame(data)
      type = decoded.type
      payload = decoded.payload
    } catch (e) {
      this.handlers.error(`bad frame: ${String(e)}`)
      return
    }
    // Log/gap decoders expect the full frame (type byte header included);
    // reassemble now that decompression, if any, is done.
    const full = new Uint8Array(payload.length + 1)
    full[0] = type
    full.set(payload, 1)
    switch (type) {
      case MSG.SNAPSHOT:
        this.handlers.snapshot(decodeDoc<StatusDoc>(payload))
        return
      case MSG.STATUS: {
        // The payload is a JSON object {programs, pending?, removed?} since the
        // apply-workflow / guard-remove changes; decode defensively (old
        // servers sent a bare program array).
        const text = new TextDecoder().decode(payload)
        try {
          const parsed = JSON.parse(text) as {
            programs?: ProgramInfo[]
            pending?: PendingDoc
            removed?: string[]
            apps_gone?: string[]
          }
          this.handlers.delta(parsed.programs ?? [], parsed.pending, parsed.removed, parsed.apps_gone)
        } catch {
          const programs = decodeDoc<ProgramInfo[]>(payload)
          this.handlers.delta(programs)
        }
        return
      }
      case MSG.LOG: {
        try {
          const { program, stream, text } = decodeLogFrame(full)
          this.handlers.log(program, stream, text)
        } catch (e) {
          this.handlers.error(`bad log frame: ${String(e)}`)
        }
        return
      }
      case MSG.LOG_GAP: {
        try {
          const { program, stream, skipped } = decodeGapFrame(full)
          this.handlers.gap(program, stream, skipped)
        } catch (e) {
          this.handlers.error(`bad gap frame: ${String(e)}`)
        }
        return
      }
      case MSG.HEARTBEAT:
        return // silence timer already re-armed
      case MSG.ERROR: {
        const text = new TextDecoder().decode(payload)
        this.handlers.error(text)
        return
      }
      default:
        return // unknown frame types are skipped, never fatal
    }
  }

  private armSilenceTimer(): void {
    if (this.silenceTimer) clearTimeout(this.silenceTimer)
    this.silenceTimer = setTimeout(() => {
      // Zombie link: force-close; onclose drives the reconnect.
      try {
        this.ws?.close()
      } catch {
        /* already gone */
      }
    }, SILENCE_LIMIT_MS)
  }

  private scheduleReconnect(): void {
    this.setState('closed')
    const delay = Math.min(BACKOFF_BASE_MS * 2 ** this.attempts, BACKOFF_MAX_MS)
    this.attempts++
    this.reconnectTimer = setTimeout(() => this.open(), delay)
  }

  private clearTimers(): void {
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer)
    if (this.silenceTimer) clearTimeout(this.silenceTimer)
    this.reconnectTimer = null
    this.silenceTimer = null
  }

  private setState(state: WsState): void {
    this.handlers.stateChange(state)
  }
}
