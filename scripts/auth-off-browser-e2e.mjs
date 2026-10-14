// Auth-OFF regression: a console without [webui] auth_token must behave
// exactly as before this change — no gate, no extra request, data renders.
import { chromium } from '/home/cupen/.npm/_npx/e41f203b7505f1fb/node_modules/playwright/index.mjs'

const BASE = process.argv[2]
if (!BASE) {
  console.error('usage: node auth-off-e2e.mjs <base-url>')
  process.exit(2)
}
const browser = await chromium.launch()
const page = await browser.newPage()
await page.goto(BASE)
await page.waitForLoadState('domcontentloaded')
await page.waitForTimeout(2500)
const gate = await page.$('[role="dialog"]')
const overview = await page.getByText('版本').first().waitFor({ state: 'visible', timeout: 8000 }).then(() => true).catch(() => false)
await browser.close()
if (gate !== null) {
  console.error('FAIL: credential gate appeared on a console without auth_token')
  process.exit(1)
}
if (!overview) {
  console.error('FAIL: overview did not render without a credential')
  process.exit(1)
}
console.log('ok: no gate on auth-off console')
console.log('ok: overview renders anonymously')
console.log('AUTH-OFF BROWSER E2E PASSED')
