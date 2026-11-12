// @vitest-environment happy-dom
// Credential gate audit (add-webui-launch-command 6.3): the expiry hint is
// shown ONLY when a stored token was rejected, never for a first-time prompt
// (fixed-token mode / manual entry).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
// Side-effect import registers the custom element (type-only usage of the
// class would be elided by the TS transform).
import './credential-gate.js'
import type { AkCredentialGate } from './credential-gate.js'
import {
  STORED_TOKEN_KEY,
  cancelCredential,
  resetAuth,
  shouldPrompt,
  withSession,
} from '../lib/credentials.js'
import { resetBootstrap } from '../lib/bootstrap.js'

function jsonResponse(status: number, body: unknown = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

async function until(cond: () => boolean): Promise<void> {
  for (let i = 0; i < 100 && !cond(); i++) {
    await new Promise((r) => setTimeout(r, 0))
  }
  expect(cond(), 'condition never became true').toBe(true)
}

function mount(): AkCredentialGate {
  const el = document.createElement('ak-credential-gate') as AkCredentialGate
  document.body.appendChild(el)
  return el
}

function textOf(el: AkCredentialGate): string {
  return (el.shadowRoot?.textContent ?? '').replace(/\s+/g, ' ')
}

beforeEach(() => {
  resetAuth()
  resetBootstrap()
  localStorage.clear()
  document.body.innerHTML = ''
})

afterEach(() => {
  resetAuth()
  resetBootstrap()
  localStorage.clear()
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('ak-credential-gate expiry hint (6.3)', () => {
  it('a first-time 401 prompts without the expiry hint', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(500)))
    const gate = mount()
    const pending = withSession(async () => jsonResponse(401))

    await until(() => shouldPrompt())
    await gate.updateComplete

    const text = textOf(gate)
    expect(text).toContain('控制台需要凭据')
    expect(text).not.toContain('已失效')
    expect(text).toContain('手工输入的 token 不会被浏览器保存')

    cancelCredential()
    expect((await pending).status).toBe(401)
  })

  it('a rejected stored token shows the run-ak-webui hint', async () => {
    localStorage.setItem(STORED_TOKEN_KEY, 'stale-tok')
    // withSession calls the injected send first, then /api/auth (rejected).
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(401)))
    const gate = mount()
    const pending = withSession(async () => jsonResponse(401))

    await until(() => shouldPrompt())
    await gate.updateComplete

    const text = textOf(gate)
    expect(text).toContain('token 已失效，请重新执行 ak webui')
    // The stored token was cleared before the gate went up.
    expect(localStorage.getItem(STORED_TOKEN_KEY)).toBeNull()

    cancelCredential()
    expect((await pending).status).toBe(401)
  })
})
