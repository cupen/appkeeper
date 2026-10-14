// Browser e2e for the console credential flow (webui-api 控制台访问控制).
// Drives the auth-ON console with real Chromium (gate appears on 401, wrong
// token keeps the gate, right token exchanges a session cookie, hygiene).
//
// Usage:
//   1. Start a daemon with `[webui] auth_token = "e2e-secret-token-42"` on
//      127.0.0.1:19877 (adjust BASE/TOKEN below to match).
//   2. Point the playwright import below at a resolved playwright install.
//   3. `node scripts/auth-browser-e2e.mjs`
// Exits 0 on pass, 1 on failure; logs one line per assertion.
import { chromium } from '/home/cupen/.npm/_npx/e41f203b7505f1fb/node_modules/playwright/index.mjs'

const BASE = 'http://127.0.0.1:19877/'
const TOKEN = 'e2e-secret-token-42'
let failures = 0
const ok = (name, cond, extra = '') => {
  console.log(`${cond ? 'ok' : 'FAIL'}: ${name}${extra ? ` — ${extra}` : ''}`)
  if (!cond) failures++
}

const browser = await chromium.launch()
const page = await browser.newPage()
const consoleLogs = []
page.on('console', (m) => consoleLogs.push(m.text()))
page.on('pageerror', (e) => consoleLogs.push(`pageerror: ${e.message}`))

// -- 1. The gate appears on a 401 console --------------------------------
await page.goto(BASE)
await page.waitForSelector('[role="dialog"]', { timeout: 8000 })
const heading = await page.textContent('[role="dialog"] h2')
ok('gate appears on 401', heading?.includes('控制台需要凭据'), heading ?? 'no heading')

// -- 2. Wrong token: exchange fails, error shows, gate stays -------------
await page.fill('[role="dialog"] input', 'wrong-token')
await page.click('[role="dialog"] button.primary')
await page.waitForSelector('[role="dialog"] .error', { timeout: 8000 })
ok('wrong token keeps the gate up with an error', true)

// -- 3. Right token: exchange, replay, console goes live -----------------
await page.fill('[role="dialog"] input', TOKEN)
await page.waitForTimeout(300) // Lit batched render enables the button
await page.click('[role="dialog"] button.primary')
await page.waitForSelector('[role="dialog"]', { state: 'detached', timeout: 8000 })
ok('gate closes after a successful exchange', true)
// Playwright text locators pierce open shadow roots (body.innerText does not).
await page.getByText('版本').first().waitFor({ state: 'visible', timeout: 8000 })
const bannerGone = await page
  .getByText('后端不可达')
  .first()
  .waitFor({ state: 'hidden', timeout: 8000 })
  .then(() => true)
  .catch(() => false)
ok('degraded banner cleared — console is live', bannerGone)
ok('overview data rendered', (await page.getByText('版本').count()) > 0)

// -- 4. Session hygiene: token in neither cookie, storage, URL, console --
const url = page.url()
ok('token not in the URL', !url.includes(TOKEN), url)
const storage = await page.evaluate(() => ({
  cookie: document.cookie,
  local: Object.keys(localStorage),
  session: Object.keys(sessionStorage),
}))
ok(
  'token not in document.cookie / localStorage / sessionStorage',
  !storage.cookie.includes(TOKEN) &&
    !storage.local.some((k) => localStorage.getItem(k)?.includes(TOKEN)) &&
    !storage.session.some((k) => sessionStorage.getItem(k)?.includes(TOKEN)),
)
ok(
  'token never hit the console log',
  !consoleLogs.some((l) => l.includes(TOKEN)),
)

// -- 5. Session persists across reload (cookie-based), no new gate -------
await page.reload()
await page.waitForLoadState('domcontentloaded')
await page.waitForTimeout(1500)
const gateAfterReload = await page.$('[role="dialog"]')
ok('no gate after reload — session cookie persists', gateAfterReload === null)
const liveAfterReload = await page
  .getByText('版本')
  .first()
  .waitFor({ state: 'visible', timeout: 8000 })
  .then(() => true)
  .catch(() => false)
ok('console live right after reload', liveAfterReload)

await browser.close()
if (failures > 0) {
  console.error(`\n${failures} assertion(s) failed`)
  process.exit(1)
}
console.log('\nAUTH BROWSER E2E PASSED')
