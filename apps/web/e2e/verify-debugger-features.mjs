#!/usr/bin/env node
// Browser end-to-end check for the debugger features added alongside this
// script (set-variable editing for locals/upvalues, the Memory/Force GC
// panel): drives the real app in headless Chromium and asserts on actual
// DOM/output state, not just that the dev server responds.
//
// This project has no existing e2e harness/CI wiring - this is a
// standalone, manually-run script for exercising a real browser during
// development, not part of `npm test`/CI. There's nothing here that a unit
// test could cover instead: the thing under test is the click-to-edit UI
// interaction and the resulting round trip through the worker/wasm/VM back
// into what's rendered on screen.
//
// Usage:
//   npx playwright install chromium   # once, downloads the browser binary
//   npm run dev                       # in one terminal
//   node e2e/verify-debugger-features.mjs
//
// Screenshots are written to e2e/screenshots/ (gitignored) for visual
// spot-checks; the actual pass/fail signal is the assertions below.

import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { chromium } from "playwright";

const APP_URL = process.env.APP_URL ?? "http://localhost:5173";
const HERE = dirname(fileURLToPath(import.meta.url));
const SHOT_DIR = join(HERE, "screenshots");
await mkdir(SHOT_DIR, { recursive: true });

// A local, a closure capturing an upvalue, and enough table allocation to
// give the Memory panel something to show.
const SCRIPT = `local function makeCounter()
  local count = 0
  local function increment()
    count = count + 1
    return count
  end
  return increment
end

local big = {}
for i = 1, 500 do
  big[i] = { value = i }
end

local inc = makeCounter()
local total = inc()
print(total)
`;

const browser = await chromium.launch({ args: ["--no-sandbox", "--disable-gpu"] });
const page = await browser.newPage({ viewport: { width: 1400, height: 1000 } });
const consoleErrors = [];
page.on("console", (msg) => {
  if (msg.type() === "error") consoleErrors.push(msg.text());
});
page.on("pageerror", (err) => consoleErrors.push(String(err)));

// Forces a couple of animation frames plus a synthetic resize before each
// screenshot - headless Chromium occasionally leaves a just-mounted DOM
// node unpainted (confirmed via computed style/outerHTML being correct
// while the screenshot showed nothing) right after a focused `<input>` is
// replaced by adjacent text in the same tick; this only affects the
// screenshot capture, not the app, but is cheap enough to always do.
async function forceRepaint() {
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  await page.evaluate(() => window.dispatchEvent(new Event("resize")));
}

async function screenshot(name) {
  await forceRepaint();
  await page.screenshot({ path: join(SHOT_DIR, name) });
}

async function clickGutter(lineNumber) {
  const lineNumEl = page.locator(".line-numbers", { hasText: new RegExp(`^${lineNumber}$`) });
  const box = await lineNumEl.boundingBox();
  if (!box) throw new Error(`line number ${lineNumber} not found/visible`);
  await page.mouse.click(box.x - 10, box.y + box.height / 2);
}

try {
  await page.goto(APP_URL);
  await page.waitForSelector(".monaco-editor");

  // Seed the project via localStorage (avoids fighting Monaco's editor
  // widget for text entry) and reload so App.tsx picks it up on mount.
  await page.evaluate((script) => {
    localStorage.setItem("lua-playground:project", JSON.stringify({ files: { "main.lua": script }, entry: "main.lua" }));
  }, SCRIPT);
  await page.reload();
  await page.waitForSelector(".monaco-editor");
  await page.waitForTimeout(500);

  // Breakpoints: line 5 ("return count", inside increment - tests upvalue
  // editing) and line 17 ("print(total)", top-level frame - tests local
  // editing).
  await clickGutter(5);
  await clickGutter(17);
  await screenshot("01-breakpoints-set.png");

  await page.click("button.debug-button");
  await page.waitForSelector(".debug-status-paused", { timeout: 15000 });
  await page.waitForTimeout(300);
  await screenshot("02-paused-at-upvalue.png");

  // --- Edit the upvalue `count` inside increment() ---
  const upvaluesSection = page.locator(".debug-section", { has: page.locator("h3", { hasText: "Upvalues" }) });
  const countRow = upvaluesSection.locator(".variable-row", { has: page.locator(".variable-name", { hasText: /^count$/ }) });
  assert.equal(await countRow.locator(".variable-display").innerText(), "1", "count should be 1 before editing");
  await countRow.locator(".variable-display").click();
  await countRow.locator(".variable-edit-input").fill("777");
  await countRow.locator(".variable-edit-input").press("Enter");
  await page.waitForTimeout(300);
  assert.equal(await countRow.locator(".variable-display").innerText(), "777", "upvalue edit did not take effect");
  await screenshot("03-upvalue-edited.png");

  // --- Continue to the second breakpoint (top-level `total`) ---
  await page.click('button:has-text("Continue")');
  await page.waitForTimeout(500);
  await screenshot("04-paused-at-local.png");

  const localsSection = page.locator(".debug-section", { has: page.locator("h3", { hasText: "Locals" }) });
  const totalRow = localsSection.locator(".variable-row", { has: page.locator(".variable-name", { hasText: /^total$/ }) });
  assert.equal(
    await totalRow.locator(".variable-display").innerText(),
    "777",
    "total should equal the edited upvalue's return value",
  );
  await totalRow.locator(".variable-display").click();
  await totalRow.locator(".variable-edit-input").fill("999");
  await totalRow.locator(".variable-edit-input").press("Enter");
  await page.waitForTimeout(300);
  assert.equal(await totalRow.locator(".variable-display").innerText(), "999", "local edit did not take effect");
  await screenshot("05-local-edited.png");

  // --- Memory panel + Force GC ---
  const memorySection = page.locator(".debug-section", { has: page.locator("h3", { hasText: "Memory" }) });
  const memBefore = await memorySection.locator(".memory-stats").innerText();
  assert.match(memBefore, /Total\s+[\d.]+ KB/, `Memory panel did not render stats:\n${memBefore}`);
  await memorySection.locator('button:has-text("Force GC")').click();
  await page.waitForTimeout(300);
  const memAfter = await memorySection.locator(".memory-stats").innerText();
  assert.match(memAfter, /Allocation debt\s+0\.0/, `expected Force GC to zero allocation debt:\n${memAfter}`);
  await screenshot("06-memory-panel.png");

  // --- Finish execution: the edited local must reach the actual program output ---
  await page.click('button:has-text("Continue")');
  await page.waitForTimeout(500);
  const consoleText = await page.locator("pre.console-output").innerText();
  assert.equal(consoleText.trim(), "999", "the edited local's value should be what print() actually output");
  await screenshot("07-terminated.png");

  assert.deepEqual(consoleErrors, [], `unexpected browser console errors: ${consoleErrors.join("; ")}`);

  console.log("PASS - all debugger feature checks succeeded");
} finally {
  await browser.close();
}
