#!/usr/bin/env node
// Run after scripts/build-sol-wasm.sh. This instantiates the actual linked
// module rather than treating a successful wasm32 cargo check as proof.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { initSync, execute_project, execute_lua, execute_lua_project, WasmDebugSession, WasmLuaDebugSession, WasmTypedDebugSession } from "../packages/sol-runtime/pkg/sol.js";

const bytes = readFileSync(new URL("../packages/sol-runtime/pkg/sol_bg.wasm", import.meta.url));
const module = new WebAssembly.Module(bytes);
assert.deepEqual(WebAssembly.Module.imports(module).filter((entry) => entry.module === "env"), [], "browser runtime must not import host libc");
initSync({ module });

// The typed corpus's native Tier-0 test uses these same expectations. Run
// the actual linked WASM too; successful cargo checks are not parity proof.
const typedTest = readFileSync(new URL("../crate/sol/tests/tier0_conformance.rs", import.meta.url), "utf8");
const typedCases = typedTest.slice(typedTest.indexOf("const CASES:"), typedTest.indexOf("fn run_on_tier0"));
let typedCount = 0;
for (const [, name, expected] of typedCases.matchAll(/\("([^"]+\.sol)", "([^"]+)"\)/g)) {
  if (name === "math.sol") continue; // Explicit native libm FFI, not a browser capability.
  const source = readFileSync(new URL(`../crate/sol/tests/fixtures/sol-conformance/${name}`, import.meta.url), "utf8");
  const result = execute_project("main.sol", ["main.sol"], [source]);
  try { assert.equal(result.error, undefined, name); assert.equal(result.result, expected, name); }
  finally { result.free(); }
  typedCount++;
}
assert.equal(typedCount, 33, "typed qualification must not silently skip corpus fixtures");

const typedDebug = WasmTypedDebugSession.launch_project("main.sol", ["main.sol", "math/base.sol"], [
  "import math.base\nfunction main(): i64\n local base: i64=40\n local result=math.base.add(base)\n return base+result\nend",
  "export function add(x: i64): i64\n local y=x+2\n return y\nend",
]);
try {
  const bp = typedDebug.set_breakpoint("math/base.sol", 2);
  try { assert.equal(bp.verified, true); } finally { bp.free(); }
  const stop = typedDebug.continue_burst(1000);
  try { assert.equal(stop.reason, "breakpoint"); assert.equal(stop.source, "math/base.sol"); } finally { stop.free(); }
  assert.equal(typedDebug.take_output(), "");
  const frames = typedDebug.get_stack_trace(0);
  try { assert.equal(frames.length, 2); assert.equal(frames[1].source, "main.sol"); assert.equal(frames[1].line, 4); }
  finally { frames.forEach((frame) => frame.free()); }
  for (const [frame, name, expression] of [[0, "x", "41"], [1, "base", "1"]]) {
    const result = typedDebug.set_variable(0, frame, name, expression);
    try { assert.equal(result.ok, true, result.display); } finally { result.free(); }
  }
  const failed = typedDebug.evaluate(0, "1//0", 0);
  try { assert.equal(failed.ok, false); } finally { failed.free(); }
  const done = typedDebug.continue_burst(1000);
  try { assert.equal(done.reason, "terminated"); } finally { done.free(); }
  assert.equal(typedDebug.take_output(), "44");
} finally { typedDebug.free(); }

// Force a real stack-independent typed collection, not the historical
// no-op. A discarded large array has its own chunk and must be reclaimed;
// paused arrays/maps and inspector handles must retain their graph.
const garbage = execute_project("main.sol", ["main.sol"], [
  "function main(): i64 local discarded=new_array_i64(200000) return discarded[0] end",
]);
try { assert.equal(garbage.error, undefined); } finally { garbage.free(); }
const typedGc = WasmTypedDebugSession.launch_project("main.sol", ["main.sol"], [
  "function main(): i64\n local xs: Array<i64> = {40,2}\n local m: Map<i64,i64> = {[1]=2}\n return xs[0]+m[1]\nend",
]);
// Compatibility API users may retain a historical trace concurrently with
// a live session. Its snapshots must participate in the same root registry.
const traceOwner = WasmDebugSession.launch("function main(): i64 local xs: Array<i64> = {9,10} return xs[0] end");
const traceRun = traceOwner.run(); traceRun.free();
try {
  const bp = typedGc.set_breakpoint("main.sol", 4); bp.free();
  const stop = typedGc.continue_burst(1000);
  try { assert.equal(stop.reason, "breakpoint"); } finally { stop.free(); }
  const before = typedGc.memory_stats();
  const beforeBytes = before.live_bytes; before.free();
  const locals = typedGc.get_locals(0, 0);
  const arrayReference = locals.find((local) => local.name === "xs").reference;
  locals.forEach((local) => local.free());
  typedGc.force_gc();
  const traceLocals = traceOwner.locals_at(traceOwner.trace_length() - 1);
  const traceArray = traceLocals.find((local) => {
    const value = local.value; try { return value.is_reference; } finally { value.free(); }
  });
  const traceValue = traceArray.value;
  const traceEntries = traceOwner.expand(traceArray.type_id, traceValue.reference);
  try {
    const value = traceEntries[0].value;
    try { assert.equal(value.scalar, "9"); } finally { value.free(); }
  } finally {
    traceValue.free(); traceEntries.forEach((entry) => entry.free()); traceLocals.forEach((local) => local.free());
  }
  const after = typedGc.memory_stats();
  try { assert.ok(beforeBytes - after.live_bytes > 1_000_000, "typed GC must reclaim discarded array storage"); }
  finally { after.free(); }
  const value = typedGc.evaluate(0, "xs[0]+m[1]", 0);
  try { assert.equal(value.ok, true, value.display); assert.equal(value.display, "42"); } finally { value.free(); }
  const entries = typedGc.get_table_entries(arrayReference, 0, 10);
  try { assert.equal(entries[0].display, "40"); } finally { entries.forEach((entry) => entry.free()); }
  const done = typedGc.continue_burst(1000);
  try { assert.equal(done.reason, "terminated"); } finally { done.free(); }
  typedGc.force_gc();
  const retained = typedGc.get_table_entries(arrayReference, 0, 10);
  try { assert.equal(retained[0].display, "40"); } finally { retained.forEach((entry) => entry.free()); }
} finally { typedGc.free(); traceOwner.free(); }

const fixtures = readFileSync(new URL("../crate/sol/tests/wasm-portable.tsv", import.meta.url), "utf8").trimEnd().split("\n");
for (const row of fixtures) {
  const [name, source, encoded] = row.split("\t");
  const expected = encoded.replaceAll("\\n", "\n").replaceAll("\\t", "\t");
  const result = execute_lua(source);
  try { assert.equal(result.error, undefined, name); assert.equal(result.result, expected, name); }
  finally { result.free(); }
}

for (const [source, expected] of [
  ["print(42); collectgarbage(); print(43)", "42\n43\n"],
  ["local t=setmetatable({}, {__index=function(_,k) return k..'!' end}); print(t.answer)", "answer!\n"],
]) {
  const result = execute_lua(source);
  try { assert.equal(result.error, undefined); assert.equal(result.result, expected); }
  finally { result.free(); }
}
const failed = execute_lua("print(42); error('expected failure')");
try { assert.equal(failed.result, "42\n"); assert.match(failed.error, /expected failure/); }
finally { failed.free(); }

const names = ["main.lua", "math/base.lua"];
const contents = ["local base = require('math.base'); print(base.answer)", "local answer = 40\nanswer = answer + 2\nreturn {answer = answer}"];
const result = execute_lua_project("main.lua", names, contents);
try { assert.equal(result.error, undefined); assert.equal(result.result, "42\n"); }
finally { result.free(); }
const session = WasmLuaDebugSession.launch_project("main.lua", names, contents);
try {
  const bp = session.set_breakpoint("math/base.lua", 3); bp.free();
  const stop = session.continue_burst(1000);
  try { assert.equal(stop.reason, "breakpoint"); assert.equal(stop.source, "math/base.lua"); }
  finally { stop.free(); }
  assert.equal(session.take_output(), "");
  const evaluated = session.evaluate(0, "answer", 0);
  try { assert.equal(evaluated.ok, true); assert.equal(evaluated.display, "42"); }
  finally { evaluated.free(); }
  session.force_gc();
  const done = session.continue_burst(1000);
  try { assert.equal(done.reason, "terminated"); } finally { done.free(); }
  assert.equal(session.take_output(), "42\n");
} finally { session.free(); }
const analysis = WasmLuaDebugSession.launch_project("main.lua", ["main.lua"], ["local function f(n)\n return n+1\nend\nprint(f(40), f(41))"]);
try {
  const stats = analysis.profile();
  try { assert.ok(stats.some((stat) => stat.function_name.endsWith(":f") && stat.calls === 2)); }
  finally { stats.forEach((stat) => stat.free()); }
} finally { analysis.free(); }
const logpoint = WasmLuaDebugSession.launch_project("main.lua", ["main.lua"], ["local x=40; print('before')\nprint(x)\nprint('after')"]);
try {
  const bp = logpoint.set_breakpoint("main.lua", 2);
  try { logpoint.set_breakpoint_log_message(bp.id, "value={x}, next={x+2}"); } finally { bp.free(); }
  const stop = logpoint.continue_burst(1000);
  try { assert.equal(stop.reason, "terminated"); } finally { stop.free(); }
  assert.equal(logpoint.take_output(), "before\nvalue=40, next=42\n40\nafter\n");
} finally { logpoint.free(); }
const coroutine = WasmLuaDebugSession.launch_project("main.lua", ["main.lua"], [
  "local outer=40\nlocal co=coroutine.create(function()\n local value=outer+2\n coroutine.yield(value)\n print(value)\nend)\nlocal ok,value=coroutine.resume(co)\nprint(ok,value)\nprint(coroutine.resume(co))",
]);
try {
  const bp = coroutine.set_breakpoint("main.lua", 4); bp.free();
  const stop = coroutine.continue_burst(1000);
  try { assert.equal(stop.reason, "breakpoint"); } finally { stop.free(); }
  const threads = coroutine.get_threads();
  try { assert.deepEqual(threads.map((thread) => thread.status), ["normal", "running"]); }
  finally { threads.forEach((thread) => thread.free()); }
  const value = coroutine.evaluate(1, "value", 0);
  try { assert.equal(value.display, "42"); } finally { value.free(); }
  const edit = coroutine.set_variable(1, 0, "value", "44");
  try { assert.equal(edit.ok, true); } finally { edit.free(); }
  coroutine.force_gc();
  const done = coroutine.continue_burst(1000);
  try { assert.equal(done.reason, "terminated"); } finally { done.free(); }
  assert.equal(coroutine.take_output(), "true\t44\n44\ntrue\n");
} finally { coroutine.free(); }
console.log(`PASS: canonical WASM imports, ${typedCount} typed fixtures, Lua execution, error output, modules, live debugging and GC`);
