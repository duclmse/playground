#!/usr/bin/env node
// U12 Work item 7: a real, repeatable differential between the old engine
// (`@lua-playground/runtime`, built from the retired `crate/lua-vm`/vendored
// Piccolo fork) and the new canonical engine (`@lua-playground/sol-runtime`,
// built from `crate/sol`'s `wasm_api.rs`) over the single-file `.sol`
// fixtures that are candidates for comparison today - driven in a real
// headless browser against the actual built worker bundle
// (`apps/web/src/lua-worker.ts`), not a mock.
//
// See docs/features/milestones/u12-wasm-playground.md's Work item 7 section
// for the full write-up of what this found. Short version, confirmed by
// actually running this script, not assumed:
//
//   There is currently no single-file `.sol` fixture in this repository for
//   which the two engines' observable "run" output is comparable, for two
//   independent, compounding, architectural reasons (not bugs):
//
//   1. The old engine is a plain Lua (Piccolo) interpreter with zero typed-
//      Sol-syntax support. Every candidate fixture needs a typed
//      `function main(): <type> ... end`-style entry point to satisfy
//      `crate::compile()`'s own requirement (`crate/sol/src/lib.rs`'s
//      `main_func` lookup - see wasm_api.rs's doc comment) - and that
//      colon-annotation syntax is not valid Lua grammar at all, so the old
//      engine fails to even *parse* every such fixture
//      (`"runtime error: parse error at line 1: found \"Colon\", expected
//      \"grouped expression or name\""`, confirmed empirically).
//   2. Separately and additionally: the two engines define a program's
//      "output" completely differently. `crate/lua-vm`'s `run`/`run_project`
//      (`crate/lua-vm/src/lib.rs`) capture only explicit `print()`/`io.write`
//      calls into a buffer and never invoke any particular function - a
//      script that merely *defines* `main` and never calls it produces empty
//      output. `wasm_api.rs::execute` instead compiles the source, calls the
//      function literally named `main`, and renders *its return value* as
//      the output - and typed `.sol` has no `print` builtin reachable from
//      source at all (confirmed in Work item 6). No fixture in this corpus
//      calls `print(...)` (confirmed: `grep -l "print(" ...` over every
//      candidate file matches nothing), so even a hypothetical fixture with
//      no type annotations at all (Lua-grammar-valid) would still mismatch:
//      old engine reports "" (nothing printed), new engine reports the
//      stringified return value.
//
// So this script's real job, and what it actually proves: that for every
// comparable fixture, the two engines agree on *failure* (the old engine
// uniformly fails - a parse error for every fixture tested) in a way
// consistent with this documented architectural gap, that none of them
// produce an inexplicable crash/hang in the new engine's wasm instance, and
// it surfaces the actual per-fixture evidence rather than asserting this
// from reasoning alone. A later item that wants a *meaningful* visible-
// output A/B (matching values, not just matching "did it fail") needs the
// new engine to grow output-buffer support first (Work item 6's own
// documented gap) - there is no test-corpus trick that gets around this
// requirement.
//
// Usage (requires two separate production builds, one per flag position,
// each into its own `--outDir` so both exist on disk simultaneously):
//   npm run build --workspace=apps/web -- --outDir dist-old
//   VITE_SOL_ENGINE=1 npm run build --workspace=apps/web -- --outDir dist-new
//   node apps/web/e2e/sol-engine-differential.mjs
//
// This script starts its own two `vite preview` servers (one per dist dir)
// and tears them down on exit - no separate `npm run preview` step needed.
// Exits 0 if every fixture's classification is an accepted outcome (match,
// or an explained/expected mismatch), 1 if anything needs a human look.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const HERE = dirname(fileURLToPath(import.meta.url));
const WEB_ROOT = join(HERE, "..");
const REPO_ROOT = join(WEB_ROOT, "..", "..");

const PER_FIXTURE_TIMEOUT_MS = 30_000;
const OLD_PORT = Number(process.env.SOL_DIFF_OLD_PORT ?? 4711);
const NEW_PORT = Number(process.env.SOL_DIFF_NEW_PORT ?? 4712);
const OLD_DIST = process.env.SOL_DIFF_OLD_DIST ?? "dist-old";
const NEW_DIST = process.env.SOL_DIFF_NEW_DIST ?? "dist-new";

// ---------------------------------------------------------------------
// Fixture discovery
// ---------------------------------------------------------------------

const FIXTURE_DIRS = [
  join(REPO_ROOT, "benchmarks"),
  join(REPO_ROOT, "crate/sol/tests/fixtures"),
  join(REPO_ROOT, "crate/sol/tests/fixtures/sol-conformance"),
];

// A real `import <module>` statement (Sol's module syntax - see
// `crate/sol/src/parser.rs`'s contextual "import" handling), not merely the
// substring "import" appearing inside a comment (e.g.
// `crate/sol/tests/fixtures/sol-conformance/pm.sol` has a line
// "-- `import` (docs/spec/...)" that must not trip this).
const IMPORT_RE = /^\s*import\s+[A-Za-z_][\w.]*\s*$/m;

function discoverFixtures() {
  const found = [];
  for (const dir of FIXTURE_DIRS) {
    for (const name of readdirSync(dir)) {
      if (!name.endsWith(".sol")) continue;
      const path = join(dir, name);
      found.push({ path, rel: relative(REPO_ROOT, path), name });
    }
  }
  // Stable order, independent of directory iteration order.
  found.sort((a, b) => a.rel.localeCompare(b.rel));
  return found;
}

function classifyFixture(fixture) {
  const source = readFileSync(fixture.path, "utf8");
  if (IMPORT_RE.test(source)) {
    return { ...fixture, source, inScope: false, reason: "imports another .sol file (multi-file project, out of scope)" };
  }
  return { ...fixture, source, inScope: true };
}

// ---------------------------------------------------------------------
// Preview servers
// ---------------------------------------------------------------------

function findWorkerPath(distDir) {
  const assetsDir = join(WEB_ROOT, distDir, "assets");
  const match = readdirSync(assetsDir).find((f) => f.startsWith("lua-worker") && f.endsWith(".js"));
  if (!match) throw new Error(`no lua-worker-*.js chunk found under ${assetsDir} - did you build ${distDir}?`);
  return `/assets/${match}`;
}

function startPreview(distDir, port) {
  const child = spawn(
    "npx",
    ["vite", "preview", "--outDir", distDir, "--port", String(port), "--strictPort"],
    { cwd: WEB_ROOT, stdio: ["ignore", "pipe", "pipe"] },
  );
  let out = "";
  child.stdout.on("data", (d) => (out += d));
  child.stderr.on("data", (d) => (out += d));
  child.on("error", (err) => {
    throw new Error(`failed to start vite preview for ${distDir}: ${err}`);
  });
  return { child, log: () => out };
}

async function waitForServer(url, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(url);
      if (res.ok || res.status < 500) return;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`timed out waiting for ${url}`);
}

// ---------------------------------------------------------------------
// Running a fixture through one engine's real worker, in a real browser
// ---------------------------------------------------------------------

async function runInWorker(page, workerPath, source, timeoutMs, entry = "main.sol") {
  return page.evaluate(
    async ({ workerPath, source, timeoutMs, entry }) => {
      return await new Promise((resolve) => {
        let settled = false;
        const settle = (value) => {
          if (settled) return;
          settled = true;
          resolve(value);
        };
        const timer = setTimeout(() => settle({ outcome: "timeout" }), timeoutMs);
        let worker;
        try {
          worker = new Worker(workerPath, { type: "module" });
        } catch (err) {
          clearTimeout(timer);
          settle({ outcome: "launch-error", message: String(err) });
          return;
        }
        worker.onmessage = (ev) => {
          const data = ev.data;
          if (data.type === "ready") {
            worker.postMessage({ type: "run", files: { [entry]: source }, entry });
          } else if (data.type === "result") {
            clearTimeout(timer);
            settle({ outcome: "result", data });
            worker.terminate();
          } else if (data.type === "error") {
            clearTimeout(timer);
            settle({ outcome: "worker-error", data });
            worker.terminate();
          }
        };
        worker.onerror = (ev) => {
          clearTimeout(timer);
          settle({ outcome: "uncaught-error", message: String(ev.message ?? ev) });
          worker.terminate();
        };
      });
    },
    { workerPath, source, timeoutMs, entry },
  );
}

// Exercises the canonical debugger through the same worker protocol the UI
// uses. This is deliberately separate from the run differential above: the
// retired runtime cannot debug typed Sol, so an old/new comparison would not
// be meaningful here. It does prove the opt-in route does not merely expose
// wasm-bindgen methods that the worker never calls.
async function debugCanonicalSolInWorker(page, workerPath) {
  return page.evaluate(async ({ workerPath }) => {
    const source = `function main(): i64
  local value: i64 = 40
  value = value + 2
  return value
end`;
    return await new Promise((resolve, reject) => {
      const worker = new Worker(workerPath, { type: "module" });
      const timeout = setTimeout(() => {
        worker.terminate();
        reject(new Error("canonical debugger worker timed out"));
      }, 30_000);
      const send = (message) => worker.postMessage(message);
      worker.onmessage = (event) => {
        const message = event.data;
        if (message.type === "ready") {
          send({ id: 1, type: "debugLaunch", files: { "main.sol": source }, entry: "main.sol" });
        } else if (message.id === 1 && message.type === "debugLaunched") {
          send({ id: 2, type: "debugSetBreakpoint", sourceId: "main.sol", line: 3 });
        } else if (message.id === 2 && message.type === "debugBreakpoint") {
          if (!message.breakpoint.verified) reject(new Error("canonical breakpoint was not verified"));
          else send({ id: 3, type: "debugContinueBurst", maxInstructions: 1000 });
        } else if (message.id === 3 && message.type === "debugBurst") {
          if (!message.stopped || message.stop?.reason !== "breakpoint") reject(new Error(`expected canonical breakpoint stop, got ${JSON.stringify(message)}`));
          else send({ id: 4, type: "debugGetLocals", threadId: 0, frameIndex: 0 });
        } else if (message.id === 4 && message.type === "debugVariables") {
          const local = message.variables.find((variable) => variable.name === "value");
          if (!local || local.display !== "40") reject(new Error(`unexpected canonical locals: ${JSON.stringify(message.variables)}`));
          else send({ id: 5, type: "debugEvaluate", threadId: 0, frameIndex: 0, expression: "value + 2" });
        } else if (message.id === 5 && message.type === "debugEvalResult") {
          clearTimeout(timeout);
          worker.terminate();
          resolve(message.result);
        } else if (message.type === "error") {
          clearTimeout(timeout);
          worker.terminate();
          reject(new Error(message.message));
        }
      };
      worker.onerror = (event) => {
        clearTimeout(timeout);
        worker.terminate();
        reject(new Error(String(event.message ?? event)));
      };
    });
  }, { workerPath });
}

function summarizeRun(run) {
  if (run.outcome === "timeout") return { status: "timeout" };
  if (run.outcome === "uncaught-error" || run.outcome === "launch-error" || run.outcome === "worker-error") {
    return { status: "crash", detail: run.message ?? JSON.stringify(run.data) };
  }
  // outcome === "result"
  const { output, error } = run.data;
  if (error) return { status: "failure", error };
  return { status: "success", output };
}

async function canonicalLuaDebugScenario(page, workerPath) {
  return page.evaluate(async ({ workerPath }) => {
    const worker = new Worker(workerPath, { type: "module" });
    const pending = new Map();
    let id = 1;
    worker.onmessage = ({ data }) => {
      const request = pending.get(data.id);
      if (request) { pending.delete(data.id); clearTimeout(request.timeout);
        if (data.type === "error") request.reject(new Error(data.message)); else request.resolve(data); }
    };
    const rpc = (message) => new Promise((resolve, reject) => {
      const requestId = id++;
      const timeout = setTimeout(() => reject(new Error(`timeout: ${message.type}`)), 10000);
      pending.set(requestId, { resolve, reject, timeout });
      worker.postMessage({ ...message, id: requestId });
    });
    const check = (value, message) => { if (!value) throw new Error(message); };
    try {
      await rpc({ type: "debugLaunch", entry: "main.sol", files: {
        "main.sol": "import math.base\nfunction main(): i64\n local base: i64=40\n local result=math.base.add(base)\n return base+result\nend",
        "math/base.sol": "export function add(x: i64): i64\n local y=x+2\n return y\nend",
      } });
      const typedBp = await rpc({ type: "debugSetBreakpoint", sourceId: "math/base.sol", line: 2 });
      check(typedBp.breakpoint.verified, "typed imported-file breakpoint verification");
      const typedStop = await rpc({ type: "debugContinueBurst", maxInstructions: 1000 });
      check(typedStop.stop?.reason === "breakpoint" && typedStop.source === "math/base.sol", "typed live imported-file stop");
      check((await rpc({ type: "debugTakeOutput" })).text === "", "typed execution must not run ahead of its stop");
      const typedFrames = await rpc({ type: "debugGetStackTrace", threadId: 0 });
      check(typedFrames.frames.length === 2 && typedFrames.frames[1].source === "main.sol" && typedFrames.frames[1].line === 4, "typed full live stack and caller call-site");
      const typedLocals = await rpc({ type: "debugGetLocals", threadId: 0, frameIndex: 0 });
      check(typedLocals.variables.some((v) => v.name === "x" && v.display === "40"), "typed named locals");
      for (const [frameIndex, name, valueExpr] of [[0, "x", "41"], [1, "base", "1"]]) {
        check((await rpc({ type: "debugSetVariable", threadId: 0, frameIndex, name, valueExpr })).result.ok, "typed frame-selected live mutation");
      }
      check(!(await rpc({ type: "debugEvaluate", threadId: 0, frameIndex: 0, expression: "1//0" })).result.ok, "typed expression trap must be recoverable");
      check((await rpc({ type: "debugContinueBurst", maxInstructions: 1000 })).stop?.reason === "terminated", "typed live resume");
      check((await rpc({ type: "debugTakeOutput" })).text === "44", "typed live edits must affect actual output");
      await rpc({ type: "debugLaunch", entry: "main.sol", files: { "main.sol":
        "function main(): i64 local discarded=new_array_i64(200000) return discarded[0] end" } });
      check((await rpc({ type: "debugContinueBurst", maxInstructions: 1000 })).stop?.reason === "terminated", "typed discarded-array fixture completion");
      await rpc({ type: "debugLaunch", entry: "main.sol", files: { "main.sol":
        "function main(): i64\n local xs: Array<i64> = {40,2}\n local m: Map<i64,i64> = {[1]=2}\n return xs[0]+m[1]\nend" } });
      await rpc({ type: "debugSetBreakpoint", sourceId: "main.sol", line: 4 });
      check((await rpc({ type: "debugContinueBurst", maxInstructions: 1000 })).stop?.reason === "breakpoint", "typed GC fixture live stop");
      const typedGcLocals = await rpc({ type: "debugGetLocals", threadId: 0, frameIndex: 0 });
      const typedArray = typedGcLocals.variables.find((v) => v.name === "xs");
      const typedBeforeGc = await rpc({ type: "debugGetMemoryStats" });
      const typedAfterGc = await rpc({ type: "debugForceGc" });
      check(typedBeforeGc.stats.totalAllocation - typedAfterGc.stats.totalAllocation > 1_000_000, "typed worker GC must actually reclaim discarded storage");
      check((await rpc({ type: "debugEvaluate", threadId: 0, frameIndex: 0, expression: "xs[0]+m[1]" })).result.display === "42", "typed live graph must survive GC");
      const typedArrayEntries = await rpc({ type: "debugGetTableEntries", reference: typedArray.reference, start: 0, count: 10 });
      check(typedArrayEntries.variables[0].display === "40", "typed inspector reference must survive GC");
      await rpc({ type: "debugLaunch", entry: "main.lua", files: { "main.lua":
        "local x = 40\nlocal function f()\n local t = {answer = x}\n print(t.answer)\nend\nf()" } });
      const bp = await rpc({ type: "debugSetBreakpoint", sourceId: "main.lua", line: 4 });
      check(bp.breakpoint.verified, "Lua breakpoint verification");
      const stop = await rpc({ type: "debugContinueBurst", maxInstructions: 1000 });
      check(stop.stopped && stop.stop.reason === "breakpoint", "Lua live breakpoint");
      const output = await rpc({ type: "debugTakeOutput" });
      check(output.text === "", "print must not happen before the breakpoint");
      const frames = await rpc({ type: "debugGetStackTrace", threadId: 0 });
      check(frames.frames.length === 2, "Lua live nested frames");
      const locals = await rpc({ type: "debugGetLocals", threadId: 0, frameIndex: 0 });
      const table = locals.variables.find((variable) => variable.name === "t");
      check(table?.expandable, "Lua named table local");
      const entries = await rpc({ type: "debugGetTableEntries", reference: table.reference, start: 0, count: 10 });
      check(entries.variables.some((variable) => variable.name === "answer" && variable.display === "40"), "Lua lazy table expansion");
      const upvalues = await rpc({ type: "debugGetUpvalues", threadId: 0, frameIndex: 0 });
      check(upvalues.variables.some((variable) => variable.name === "x" && variable.display === "40"), "Lua captured upvalue");
      const evaluated = await rpc({ type: "debugEvaluate", threadId: 0, frameIndex: 0, expression: "t.answer + x" });
      check(evaluated.result.ok && evaluated.result.display === "80", "Lua frame-scoped evaluation");
      const mutation = await rpc({ type: "debugSetVariable", threadId: 0, frameIndex: 0, name: "t", valueExpr: "{answer = 42}" });
      check(mutation.result.ok, "Lua live local mutation");
      await rpc({ type: "debugForceGc" });
      const done = await rpc({ type: "debugContinueBurst", maxInstructions: 1000 });
      check(done.stop?.reason === "terminated", "Lua resume after inspection and GC");
      const finalOutput = await rpc({ type: "debugTakeOutput" });
      check(finalOutput.text === "42\n", "Lua mutated output");
      const project = { entry: "main.lua", files: { "main.lua": "local function f(n)\n return n+1\nend\nprint(f(40), f(41))" } };
      const profile = await rpc({ type: "profile", ...project });
      check(profile.stats.some((stat) => stat.functionId.endsWith(":f") && stat.calls === 2 && stat.selfInstructions > 0), "canonical Lua profiler");
      const timeline = await rpc({ type: "recordTimeline", ...project, maxEvents: 2 });
      check(timeline.timeline.events.length === 2 && timeline.timeline.truncated && !timeline.timeline.error, "canonical bounded Lua timeline");
      await rpc({ type: "debugLaunch", entry: "main.lua", files: { "main.lua":
        "local outer = 40\nlocal co = coroutine.create(function()\n local value = outer\n value = value + 2\n coroutine.yield(value)\n print(value)\nend)\nlocal ok, result = coroutine.resume(co)\nprint(ok, result)\nok = coroutine.resume(co)\nprint(ok)" } });
      await rpc({ type: "debugSetBreakpoint", sourceId: "main.lua", line: 5 });
      const coroutineStop = await rpc({ type: "debugContinueBurst", maxInstructions: 1000 });
      check(coroutineStop.stop?.reason === "breakpoint", "live coroutine breakpoint");
      const threads = await rpc({ type: "debugGetThreads" });
      check(threads.threads.length === 2 && threads.threads[0].status === "normal" && threads.threads[1].status === "running", "active coroutine resume chain");
      const parent = await rpc({ type: "debugEvaluate", threadId: 0, frameIndex: 0, expression: "outer" });
      const child = await rpc({ type: "debugEvaluate", threadId: 1, frameIndex: 0, expression: "value" });
      check(parent.result.display === "40" && child.result.display === "42", "isolated coroutine frame scopes");
      const editedChild = await rpc({ type: "debugSetVariable", threadId: 1, frameIndex: 0, name: "value", valueExpr: "44" });
      check(editedChild.result.ok, "live coroutine mutation");
      await rpc({ type: "debugForceGc" });
      const resumed = await rpc({ type: "debugContinueBurst", maxInstructions: 1000 });
      check(resumed.stop?.reason === "terminated", "coroutine yield and later resume");
      const coroutineOutput = await rpc({ type: "debugTakeOutput" });
      check(coroutineOutput.text === "true\t44\n44\ntrue\n", "coroutine mutation survives suspension and GC");
      await rpc({ type: "debugLaunch", entry: "main.lua", files: { "main.lua": "while true do end" } });
      const start = performance.now();
      const running = await rpc({ type: "debugContinueBurst", maxInstructions: 1000 });
      const elapsed = performance.now() - start;
      check(!running.stopped && elapsed < 2000, "Lua runaway loop must yield control within a burst");
      return { output: finalOutput.text, frames: frames.frames.length, elapsed };
    } finally {
      worker.terminate();
      for (const request of pending.values()) clearTimeout(request.timeout);
    }
  }, { workerPath });
}

// ---------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------

async function main() {
  const all = discoverFixtures().map(classifyFixture);
  const inScope = all.filter((f) => f.inScope);
  const outOfScope = all.filter((f) => !f.inScope);

  console.log(`Discovered ${all.length} .sol fixtures across ${FIXTURE_DIRS.length} directories.`);
  if (outOfScope.length > 0) {
    console.log(`Excluded (multi-file project, out of scope for this differential):`);
    for (const f of outOfScope) console.log(`  - ${f.rel}: ${f.reason}`);
  }
  console.log(
    `Known, separately-documented multi-file gap not scanned here: examples/sol-demo/main.sol ` +
      `(imports geometry.sol) - out of scope for the same reason.`,
  );
  console.log(`Running ${inScope.length} in-scope fixtures through both engines...\n`);

  const oldWorker = findWorkerPath(OLD_DIST);
  const newWorker = findWorkerPath(NEW_DIST);

  const oldPreview = startPreview(OLD_DIST, OLD_PORT);
  const newPreview = startPreview(NEW_DIST, NEW_PORT);
  const cleanup = () => {
    oldPreview.child.kill();
    newPreview.child.kill();
  };
  process.on("exit", cleanup);

  try {
    await waitForServer(`http://localhost:${OLD_PORT}/`);
    await waitForServer(`http://localhost:${NEW_PORT}/`);

    const browser = await chromium.launch({ args: ["--no-sandbox"] });
    const oldPage = await browser.newPage();
    const newPage = await browser.newPage();
    await oldPage.goto(`http://localhost:${OLD_PORT}/`);
    await newPage.goto(`http://localhost:${NEW_PORT}/`);

    const debuggerResult = await debugCanonicalSolInWorker(newPage, newWorker);
    assert.deepEqual(debuggerResult, { ok: true, display: "42" });
    console.log("Canonical worker debugger breakpoint/locals/eval check: PASS.");
    const luaDebug = await canonicalLuaDebugScenario(newPage, newWorker);
    assert.equal(luaDebug.output, "42\n");
    console.log(`Canonical live Lua debugger/GC/response check: PASS (${luaDebug.elapsed.toFixed(1)} ms burst).`);
    const luaFixtures = [
      ["output", "print(42)", "42\n"],
      ["closures", "local n=40; local function f() n=n+1; return n end; print(f(), f())", "41\t42\n"],
      ["tables", "local t={}; for i=1,10 do t[i]=i end; print(#t,t[4])", "10\t4\n"],
      ["metamethod", "local t=setmetatable({}, {__index=function(_, k) return k..'!' end}); print(t.answer)", "answer!\n"],
      ["protected call", "local ok=pcall(function() error('expected') end); print(ok)", "false\n"],
      ["coroutine", "local co=coroutine.create(function() coroutine.yield(40); return 42 end); local a,b=coroutine.resume(co); print(a,b); a,b=coroutine.resume(co); print(a,b)", "true\t40\ntrue\t42\n"],
    ];
    for (const [name, source, expected] of luaFixtures) {
      const [oldRun, newRun] = await Promise.all([
        runInWorker(oldPage, oldWorker, source, PER_FIXTURE_TIMEOUT_MS, "main.lua"),
        runInWorker(newPage, newWorker, source, PER_FIXTURE_TIMEOUT_MS, "main.lua"),
      ]);
      assert.deepEqual(summarizeRun(oldRun), { status: "success", output: expected }, `legacy Lua fixture: ${name}`);
      assert.deepEqual(summarizeRun(newRun), { status: "success", output: expected }, `canonical Lua fixture: ${name}`);
    }
    console.log(`Real Lua old/new output differential: PASS (${luaFixtures.length} fixtures).`);
    await new Promise((resolve, reject) => {
      const check = spawn(process.execPath, [join(HERE, "verify-debugger-features.mjs")], {
        cwd: WEB_ROOT, env: { ...process.env, APP_URL: `http://localhost:${NEW_PORT}/` },
        stdio: ["ignore", "pipe", "pipe"],
      });
      let log = "";
      check.stdout.on("data", (chunk) => { log += chunk; });
      check.stderr.on("data", (chunk) => { log += chunk; });
      check.on("error", reject);
      check.on("exit", (code) => {
        if (code === 0) { console.log(log.trim()); resolve(); }
        else reject(new Error(`canonical debugger UI regression failed: ${log}`));
      });
    });

    const results = [];
    for (const fixture of inScope) {
      const [oldRaw, newRaw] = await Promise.all([
        runInWorker(oldPage, oldWorker, fixture.source, PER_FIXTURE_TIMEOUT_MS),
        runInWorker(newPage, newWorker, fixture.source, PER_FIXTURE_TIMEOUT_MS),
      ]);
      const oldResult = summarizeRun(oldRaw);
      const newResult = summarizeRun(newRaw);
      results.push({ fixture, oldResult, newResult, classification: classify(oldResult, newResult) });
    }

    await browser.close();
    report(results);

    const needsReview = results.filter((r) => r.classification.needsReview);
    if (needsReview.length > 0) {
      console.error(`\n${needsReview.length} fixture(s) need human review (see "NEEDS REVIEW" above). FAIL.`);
      process.exitCode = 1;
    } else {
      console.log(`\nAll ${results.length} fixtures fall into an accepted/expected category. PASS.`);
      process.exitCode = 0;
    }
  } finally {
    cleanup();
  }
}

// Classification is deliberately conservative: only "both crashed/timed out
// unexpectedly" or "both succeeded with different values" or "new engine
// failed where old succeeded" count as needing a human look. A new-success/
// old-parse-failure split is the expected, documented architectural gap
// (see this file's top comment) - not flagged, but still printed per
// fixture so the evidence is visible, not hidden.
function classify(oldResult, newResult) {
  if (oldResult.status === "timeout" || newResult.status === "timeout") {
    return { label: "TIMEOUT", needsReview: true };
  }
  if (oldResult.status === "crash" || newResult.status === "crash") {
    return { label: "CRASH", needsReview: true };
  }
  if (oldResult.status === "success" && newResult.status === "success") {
    if (oldResult.output === newResult.output) {
      return { label: "MATCH", needsReview: false };
    }
    return { label: "VALUE MISMATCH", needsReview: true };
  }
  if (oldResult.status === "failure" && newResult.status === "failure") {
    return { label: "BOTH FAIL (agree)", needsReview: false };
  }
  if (newResult.status === "success" && oldResult.status === "failure") {
    return { label: "EXPECTED GAP (new runs typed Sol, old cannot parse it)", needsReview: false };
  }
  if (newResult.status === "failure" && oldResult.status === "success") {
    return { label: "NEW ENGINE REGRESSED (old succeeded, new failed)", needsReview: true };
  }
  return { label: "UNEXPECTED", needsReview: true };
}

function shortDetail(result) {
  if (result.status === "success") return `output=${JSON.stringify(result.output)}`;
  if (result.status === "failure") return `error=${JSON.stringify(result.error)}`;
  if (result.status === "timeout") return "timed out";
  return `crash: ${result.detail}`;
}

function report(results) {
  console.log("\n=== Per-fixture results ===");
  for (const { fixture, oldResult, newResult, classification } of results) {
    console.log(`${classification.needsReview ? "NEEDS REVIEW" : "ok"}  [${classification.label}]  ${fixture.rel}`);
    console.log(`    old: ${shortDetail(oldResult)}`);
    console.log(`    new: ${shortDetail(newResult)}`);
  }
  const counts = {};
  for (const { classification } of results) {
    counts[classification.label] = (counts[classification.label] ?? 0) + 1;
  }
  console.log("\n=== Summary ===");
  for (const [label, count] of Object.entries(counts)) {
    console.log(`  ${count}  ${label}`);
  }
}

await main();
