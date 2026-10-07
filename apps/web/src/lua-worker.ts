/// <reference lib="webworker" />
import init, { execute_project, DebugSession, profile_project, record_timeline_project } from "@lua-playground/runtime";
import type { DebugRequest, WorkerEvent } from "./debug-protocol";
import type * as SolRuntime from "@lua-playground/sol-runtime";

export type { WorkerEvent } from "./debug-protocol";

// U12 opt-in canonical runtime. All-Lua and specialized all-Sol projects
// have live debugger adapters; imports use the worker's in-memory file map.
// Mixed projects still use the compatibility adapter pending qualification.
// The production switch remains OFF unless VITE_SOL_ENGINE=1 is supplied.
const SOL_ENGINE_ENABLED = import.meta.env.VITE_SOL_ENGINE === "1";

let ready: Promise<unknown> | null = null;
let session: DebugSession | null = null;
let solRuntime: typeof SolRuntime | null = null;
let luaDebugSession: SolRuntime.WasmLuaDebugSession | null = null;
let typedDebugSession: SolRuntime.WasmTypedDebugSession | null = null;

async function ensureReady() {
  if (!ready) {
    ready = SOL_ENGINE_ENABLED
      ? (async () => {
          const runtime = await import("@lua-playground/sol-runtime");
          await Promise.all([init(), runtime.default()]);
          solRuntime = runtime;
        })()
      : init();
  }
  await ready;
}

function requireSolRuntime(): typeof SolRuntime {
  if (!solRuntime) {
    throw new Error("canonical Sol runtime was not initialized");
  }
  return solRuntime;
}

function post(event: WorkerEvent) {
  self.postMessage(event);
}

function requireSession(): DebugSession {
  if (!session) {
    throw new Error("no debug session: call debugLaunch first");
  }
  return session;
}

function isCanonicalSolProject(files: Record<string, string>, entry: string): boolean {
  const names = Object.keys(files);
  return SOL_ENGINE_ENABLED && entry.endsWith(".sol") && names.every((name) => name.endsWith(".sol"));
}

function handleCanonicalLiveDebug(message: DebugRequest): boolean {
  const canonical = luaDebugSession ?? typedDebugSession;
  if (!canonical || !("id" in message) || message.type === "debugLaunch") return false;
  const id = message.id;
  const variables = (values: SolRuntime.LuaDebugVariable[]) => {
    const mapped = values.map((v) => {
      const result = { name: v.name, valueType: v.value_type, display: v.display,
        expandable: v.expandable, reference: v.reference ?? null };
      v.free();
      return result;
    });
    post({ type: "debugVariables", id, variables: mapped });
  };
  const stopped = (value: SolRuntime.LuaDebugStop, burst = false) => {
    const stop = { reason: value.reason, line: value.line ?? null, message: value.message ?? null };
    if (burst) post({ type: "debugBurst", id, stopped: stop.reason !== "running",
      stop: stop.reason === "running" ? null : stop, source: value.source ?? null, line: stop.line });
    else post({ type: "debugStopped", id, stop });
    value.free();
  };
  const evaluation = (result: SolRuntime.WasmEvalResult) => {
    post({ type: "debugEvalResult", id, result: { ok: result.ok, display: result.display } });
    result.free();
  };
  switch (message.type) {
    case "debugContinue": stopped(canonical.continue_()); break;
    case "debugContinueBurst": stopped(canonical.continue_burst(message.maxInstructions), true); break;
    case "debugStepInto": stopped(canonical.step_into()); break;
    case "debugStepOver": stopped(canonical.step_over()); break;
    case "debugStepOut": stopped(canonical.step_out()); break;
    case "debugSetBreakpoint": {
      const bp = canonical.set_breakpoint(message.sourceId, message.line);
      post({ type: "debugBreakpoint", id, breakpoint: { id: bp.id, sourceId: message.sourceId,
        line: bp.line, verified: bp.verified } });
      bp.free(); break;
    }
    case "debugRemoveBreakpoint": canonical.remove_breakpoint(message.breakpointId); post({ type: "debugAck", id }); break;
    case "debugSetBreakpointCondition": canonical.set_breakpoint_condition(message.breakpointId, message.condition ?? undefined); post({ type: "debugAck", id }); break;
    case "debugSetBreakpointHitCondition": canonical.set_breakpoint_hit_condition(message.breakpointId, message.hitCondition ?? undefined); post({ type: "debugAck", id }); break;
    case "debugSetBreakpointLogMessage": canonical.set_breakpoint_log_message(message.breakpointId, message.logMessage ?? undefined); post({ type: "debugAck", id }); break;
    case "debugGetThreads": {
      const threads = canonical.get_threads().map((thread) => {
        const result = { id: thread.id, status: thread.status }; thread.free(); return result;
      });
      post({ type: "debugThreads", id, threads }); break;
    }
    case "debugGetStackTrace": {
      const frames = canonical.get_stack_trace(message.threadId).map((frame) => {
        const result = { index: frame.index, name: frame.name, source: frame.source,
          line: frame.line ?? null, functionType: frame.function_type };
        frame.free(); return result;
      });
      post({ type: "debugStackTrace", id, frames }); break;
    }
    case "debugGetLocals": variables(canonical.get_locals(message.threadId, message.frameIndex)); break;
    case "debugGetUpvalues": variables(canonical.get_upvalues(message.threadId, message.frameIndex)); break;
    case "debugGetGlobals": variables(canonical.get_globals()); break;
    case "debugGetTableEntries": variables(canonical.get_table_entries(message.reference, message.start, message.count)); break;
    case "debugGetMetatable": post({ type: "debugMetatable", id, reference: canonical.get_metatable(message.reference) ?? null }); break;
    case "debugEvaluate": evaluation(canonical.evaluate(message.threadId, message.expression, message.frameIndex)); break;
    case "debugSetVariable": evaluation(canonical.set_variable(message.threadId, message.frameIndex, message.name, message.valueExpr)); break;
    case "debugTakeOutput": post({ type: "debugOutput", id, text: canonical.take_output() }); break;
    case "debugForceGc":
    case "debugGetMemoryStats": {
      if (message.type === "debugForceGc") canonical.force_gc();
      const stats = canonical.memory_stats();
      post({ type: "debugMemoryStats", id, stats: { totalAllocation: stats.live_bytes,
        gcAllocation: stats.live_bytes, externalAllocation: 0, allocationDebt: 0 } });
      stats.free(); break;
    }
    default: return false;
  }
  return true;
}

self.onmessage = async (event: MessageEvent<DebugRequest>) => {
  const message = event.data;
  await ensureReady();

  try {
    if (handleCanonicalLiveDebug(message)) return;
    switch (message.type) {
      case "run": {
        const names = Object.keys(message.files);
        const contents = names.map((name) => message.files[name]);
        if (isCanonicalSolProject(message.files, message.entry)) {
          // The canonical engine accepts exactly the typed `.sol` profile.
          // Keep any `.lua`/mixed project on the existing runtime, whose
          // dynamic standard library and mixed-module adapter are required.
          const runtime = requireSolRuntime();
          const solResult = runtime.execute_project(message.entry, names, contents);
          post({
            type: "result",
            output: solResult.result ?? "",
            error: solResult.error ?? null,
            errorSource: solResult.error ? names[0] : null,
            errorLine: null,
          });
          solResult.free();
          return;
        }
        if (SOL_ENGINE_ENABLED && message.entry.endsWith(".lua") && names.every((name) => name.endsWith(".lua"))) {
          const solResult = requireSolRuntime().execute_lua_project(message.entry, names, contents);
          post({
            type: "result",
            output: solResult.result ?? "",
            error: solResult.error ?? null,
            errorSource: solResult.error ? message.entry : null,
            errorLine: null,
          });
          solResult.free();
          return;
        }
        const result = execute_project(names, contents, message.entry);
        post({
          type: "result",
          output: result.output,
          error: result.error ?? null,
          errorSource: result.error_source ?? null,
          errorLine: result.error_line ?? null,
        });
        return;
      }

      case "debugLaunch": {
        const names = Object.keys(message.files);
        const contents = names.map((name) => message.files[name]);
        luaDebugSession?.free();
        luaDebugSession = null;
        typedDebugSession?.free();
        typedDebugSession = null;
        session?.free();
        session = null;
        if (SOL_ENGINE_ENABLED && message.entry.endsWith(".lua") && names.every((name) => name.endsWith(".lua"))) {
          luaDebugSession = requireSolRuntime().WasmLuaDebugSession.launch_project(message.entry, names, contents);
          post({ type: "debugLaunched", id: message.id });
          return;
        }
        if (isCanonicalSolProject(message.files, message.entry)) {
          const runtime = requireSolRuntime();
          typedDebugSession = runtime.WasmTypedDebugSession.launch_project(message.entry, names, contents);
          session = null;
          post({ type: "debugLaunched", id: message.id });
          return;
        }
        session = DebugSession.launch_project(names, contents, message.entry);
        post({ type: "debugLaunched", id: message.id });
        return;
      }

      case "debugContinue": {
        const stop = requireSession().continue_();
        post({
          type: "debugStopped",
          id: message.id,
          stop: { reason: stop.reason, line: stop.line ?? null, message: stop.message ?? null },
        });
        return;
      }
      case "debugContinueBurst": {
        const burst = requireSession().continue_burst(message.maxInstructions);
        const stop = burst.stop;
        post({
          type: "debugBurst",
          id: message.id,
          stopped: burst.stopped,
          stop: stop ? { reason: stop.reason, line: stop.line ?? null, message: stop.message ?? null } : null,
          source: burst.source ?? null,
          line: burst.line ?? null,
        });
        return;
      }
      case "debugStepOver": {
        const stop = requireSession().step_over();
        post({
          type: "debugStopped",
          id: message.id,
          stop: { reason: stop.reason, line: stop.line ?? null, message: stop.message ?? null },
        });
        return;
      }
      case "debugStepInto": {
        const stop = requireSession().step_into();
        post({
          type: "debugStopped",
          id: message.id,
          stop: { reason: stop.reason, line: stop.line ?? null, message: stop.message ?? null },
        });
        return;
      }
      case "debugStepOut": {
        const stop = requireSession().step_out();
        post({
          type: "debugStopped",
          id: message.id,
          stop: { reason: stop.reason, line: stop.line ?? null, message: stop.message ?? null },
        });
        return;
      }

      case "debugSetBreakpoint": {
        const bp = requireSession().set_breakpoint(message.sourceId, message.line);
        post({
          type: "debugBreakpoint",
          id: message.id,
          breakpoint: { id: bp.id, sourceId: bp.source_id, line: bp.line, verified: bp.verified },
        });
        return;
      }
      case "debugRemoveBreakpoint": {
        requireSession().remove_breakpoint(message.breakpointId);
        post({ type: "debugAck", id: message.id });
        return;
      }
      case "debugSetBreakpointCondition": {
        requireSession().set_breakpoint_condition(message.breakpointId, message.condition ?? undefined);
        post({ type: "debugAck", id: message.id });
        return;
      }
      case "debugSetBreakpointHitCondition": {
        requireSession().set_breakpoint_hit_condition(message.breakpointId, message.hitCondition ?? undefined);
        post({ type: "debugAck", id: message.id });
        return;
      }
      case "debugSetBreakpointLogMessage": {
        requireSession().set_breakpoint_log_message(
          message.breakpointId,
          message.logMessage ?? undefined,
        );
        post({ type: "debugAck", id: message.id });
        return;
      }

      case "debugGetThreads": {
        const threads = requireSession()
          .get_threads()
          .map((t) => ({ id: t.id, status: t.status }));
        post({ type: "debugThreads", id: message.id, threads });
        return;
      }
      case "debugGetStackTrace": {
        const frames = requireSession()
          .get_stack_trace(message.threadId)
          .map((f) => ({
            index: f.index,
            name: f.name,
            source: f.source,
            line: f.line ?? null,
            functionType: f.function_type,
          }));
        post({ type: "debugStackTrace", id: message.id, frames });
        return;
      }
      case "debugGetLocals": {
        const variables = requireSession()
          .get_locals(message.threadId, message.frameIndex)
          .map((v) => ({
            name: v.name,
            valueType: v.value_type,
            display: v.display,
            expandable: v.expandable,
            reference: v.reference ?? null,
          }));
        post({ type: "debugVariables", id: message.id, variables });
        return;
      }
      case "debugGetUpvalues": {
        const variables = requireSession()
          .get_upvalues(message.threadId, message.frameIndex)
          .map((v) => ({
            name: v.name,
            valueType: v.value_type,
            display: v.display,
            expandable: v.expandable,
            reference: v.reference ?? null,
          }));
        post({ type: "debugVariables", id: message.id, variables });
        return;
      }
      case "debugGetGlobals": {
        const variables = requireSession()
          .get_globals()
          .map((v) => ({
            name: v.name,
            valueType: v.value_type,
            display: v.display,
            expandable: v.expandable,
            reference: v.reference ?? null,
          }));
        post({ type: "debugVariables", id: message.id, variables });
        return;
      }
      case "debugGetTableEntries": {
        const variables = requireSession()
          .get_table_entries(message.reference, message.start, message.count)
          .map((v) => ({
            name: v.name,
            valueType: v.value_type,
            display: v.display,
            expandable: v.expandable,
            reference: v.reference ?? null,
          }));
        post({ type: "debugVariables", id: message.id, variables });
        return;
      }
      case "debugGetMetatable": {
        const reference = requireSession().get_metatable(message.reference);
        post({ type: "debugMetatable", id: message.id, reference: reference ?? null });
        return;
      }

      case "debugEvaluate": {
        const result = requireSession().evaluate(
          message.threadId,
          message.expression,
          message.frameIndex,
        );
        post({
          type: "debugEvalResult",
          id: message.id,
          result: { ok: result.ok, display: result.display },
        });
        return;
      }
      case "debugSetVariable": {
        const result = requireSession().set_variable(
          message.threadId,
          message.frameIndex,
          message.name,
          message.valueExpr,
        );
        post({
          type: "debugEvalResult",
          id: message.id,
          result: { ok: result.ok, display: result.display },
        });
        return;
      }
      case "debugTakeOutput": {
        const text = requireSession().take_output();
        post({ type: "debugOutput", id: message.id, text });
        return;
      }
      case "debugGetMemoryStats": {
        const stats = requireSession().get_memory_stats();
        post({
          type: "debugMemoryStats",
          id: message.id,
          stats: {
            totalAllocation: stats.total_allocation,
            gcAllocation: stats.gc_allocation,
            externalAllocation: stats.external_allocation,
            allocationDebt: stats.allocation_debt,
          },
        });
        return;
      }
      case "debugForceGc": {
        requireSession().force_gc();
        const stats = requireSession().get_memory_stats();
        post({
          type: "debugMemoryStats",
          id: message.id,
          stats: {
            totalAllocation: stats.total_allocation,
            gcAllocation: stats.gc_allocation,
            externalAllocation: stats.external_allocation,
            allocationDebt: stats.allocation_debt,
          },
        });
        return;
      }

      case "profile": {
        const names = Object.keys(message.files);
        const contents = names.map((name) => message.files[name]);
        if (SOL_ENGINE_ENABLED && message.entry.endsWith(".lua") && names.every((name) => name.endsWith(".lua"))) {
          const canonical = requireSolRuntime().WasmLuaDebugSession.launch_project(message.entry, names, contents);
          try {
            const stats = canonical.profile().map((stat) => {
              const result = { functionId: stat.function_name, calls: stat.calls,
                totalInstructions: stat.total_instructions, selfInstructions: stat.self_instructions };
              stat.free(); return result;
            });
            post({ type: "profileResult", id: message.id, stats });
          } finally { canonical.free(); }
          return;
        }
        if (isCanonicalSolProject(message.files, message.entry)) {
          const canonical = requireSolRuntime().WasmTypedDebugSession.launch_project(message.entry, names, contents);
          try {
            const stats = canonical.profile().map((stat) => {
              const value = { functionId: stat.function_name, calls: stat.calls,
                totalInstructions: stat.total_instructions, selfInstructions: stat.self_instructions };
              stat.free(); return value;
            });
            post({ type: "profileResult", id: message.id, stats });
          } finally { canonical.free(); }
          return;
        }
        const stats = profile_project(names, contents, message.entry).map((s) => ({
          functionId: s.function_id,
          calls: s.calls,
          totalInstructions: s.total_instructions,
          selfInstructions: s.self_instructions,
        }));
        post({ type: "profileResult", id: message.id, stats });
        return;
      }
      case "recordTimeline": {
        const names = Object.keys(message.files);
        const contents = names.map((name) => message.files[name]);
        if (SOL_ENGINE_ENABLED && message.entry.endsWith(".lua") && names.every((name) => name.endsWith(".lua"))) {
          const canonical = requireSolRuntime().WasmLuaDebugSession.launch_project(message.entry, names, contents);
          try {
            const result = canonical.record_timeline(message.maxEvents);
            try {
              const events = result.events.map((event) => {
                const mapped = { eventType: event.event_type, source: event.source,
                  line: event.line ?? null, local0: event.local0 ?? null, duration: event.duration };
                event.free(); return mapped;
              });
              post({ type: "timelineResult", id: message.id,
                timeline: { events, truncated: result.truncated, error: result.error ?? null } });
            } finally { result.free(); }
          } finally { canonical.free(); }
          return;
        }
        if (isCanonicalSolProject(message.files, message.entry)) {
          const canonical = requireSolRuntime().WasmTypedDebugSession.launch_project(message.entry, names, contents);
          try {
            const result = canonical.record_timeline(message.maxEvents);
            try {
              const events = result.events.map((event) => {
                const mapped = { eventType: event.event_type, source: event.source,
                  line: event.line ?? null, local0: event.local0 ?? null, duration: event.duration };
                event.free(); return mapped;
              });
              post({ type: "timelineResult", id: message.id,
                timeline: { events, truncated: result.truncated, error: result.error ?? null } });
            } finally { result.free(); }
          } finally { canonical.free(); }
          return;
        }
        const result = record_timeline_project(names, contents, message.entry, message.maxEvents);
        const events = result.events.map((e) => ({
          eventType: e.event_type,
          source: e.source ?? null,
          line: e.line ?? null,
          local0: e.local0 ?? null,
          duration: e.duration,
        }));
        post({
          type: "timelineResult",
          id: message.id,
          timeline: { events, truncated: result.truncated, error: result.error ?? null },
        });
        return;
      }
    }
  } catch (err) {
    if ("id" in message) {
      post({ type: "error", id: message.id, message: String(err) });
    } else if (message.type === "run") {
      // U12 item 7 finding: a "run" message carries no `id` (unlike every
      // debug* message), so without this branch a thrown exception here -
      // e.g. the new `executeSol` engine's Tier-0 interpreter hitting a trap
      // (`crate/sol/src/interp.rs:50`'s `trap()`/`std::process::abort()`,
      // which lowers to a wasm `unreachable` trap and surfaces as a thrown
      // `WebAssembly.RuntimeError` from the synchronous `executeSol` call,
      // not a value `executeSol` itself returns) - silently vanished with no
      // reply ever posted, hanging the caller forever instead of surfacing
      // an error. See
      // docs/features/milestones/u12-wasm-playground.md's Work item 7
      // section for the differential run that found this via real trap
      // fixtures (e.g. `division_by_zero.sol`).
      post({
        type: "result",
        output: "",
        error: String(err),
        errorSource: null,
        errorLine: null,
      });
    }
  }
};

ensureReady().then(() => {
  post({ type: "ready" });
});
