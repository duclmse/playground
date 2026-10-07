/// <reference lib="webworker" />
import init, * as runtime from "@lua-playground/sol-runtime";
import type { DebugRequest, WorkerEvent } from "./debug-protocol";
import type * as SolRuntime from "@lua-playground/sol-runtime";
export type { WorkerEvent } from "./debug-protocol";
type CanonicalSession = SolRuntime.WasmLuaDebugSession | SolRuntime.WasmTypedDebugSession | SolRuntime.WasmMixedDebugSession;
let ready: Promise<unknown> | null = null;
let debugSession: CanonicalSession | null = null;
function ensureReady() { return ready ??= init(); }
function post(event: WorkerEvent) { self.postMessage(event); }
function launch(files: Record<string,string>, entry: string): CanonicalSession {
  const names = Object.keys(files);
  const contents = names.map((name) => files[name]);
  if (!Object.prototype.hasOwnProperty.call(files, entry)) throw new Error("entry is missing from project");
  names.forEach((name) => {
    if (!name.endsWith(".lua") && !name.endsWith(".sol")) throw new Error(`unsupported project path '${name}'`);
  });
  const generic = !runtime.source_requires_specialization(files[entry],entry.endsWith(".sol"));
  if (generic) return runtime.WasmLuaDebugSession.launch_generic_project(entry,names,contents);
  try {
    return runtime.WasmTypedDebugSession.launch_project(entry,names,contents);
  } catch (typedError) {
    // The mixed compiler validates the same source contracts and canonical
    // import graph. Genuine static/FFI errors are not silently boxed away.
    try { return runtime.WasmMixedDebugSession.launch_project(entry,names,contents); }
    catch { throw typedError; }
  }
}
function handleCanonicalLiveDebug(message: DebugRequest): boolean {
  const canonical = debugSession;
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


self.onmessage = async ({ data: message }: MessageEvent<DebugRequest>) => {
  try {
    await ensureReady();
    if (handleCanonicalLiveDebug(message)) return;
    switch (message.type) {
      case "debugLaunch": {
        const next = launch(message.files,message.entry);
        debugSession?.free(); debugSession=next;
        post({type:"debugLaunched",id:message.id}); return;
      }
      case "run": {
        const execution=launch(message.files,message.entry);
        try {
          let stop=execution.continue_();
          while (stop.reason==="running") { stop.free(); stop=execution.continue_(); }
          try { post({type:"result",output:execution.take_output(),error:stop.reason==="exception" ? stop.message ?? "execution failed" : null,
            errorSource:stop.reason==="exception" ? stop.source ?? message.entry : null,errorLine:stop.line ?? null}); }
          finally {stop.free();}
        } finally {execution.free();}
        return;
      }
      case "profile": {
        const execution=launch(message.files,message.entry);
        try {
          const stats=execution.profile().map((stat)=>{
            const result={functionId:stat.function_name,calls:stat.calls,totalInstructions:stat.total_instructions,selfInstructions:stat.self_instructions};
            stat.free(); return result;
          });
          post({type:"profileResult",id:message.id,stats});
        } finally {execution.free();}
        return;
      }
      case "recordTimeline": {
        const execution=launch(message.files,message.entry);
        try {
          const timeline=execution.record_timeline(message.maxEvents);
          try {
            const events=timeline.events.map((event)=>{
              const result={eventType:event.event_type,source:event.source,line:event.line ?? null,local0:event.local0 ?? null,duration:event.duration};
              event.free(); return result;
            });
            post({type:"timelineResult",id:message.id,timeline:{events,truncated:timeline.truncated,error:timeline.error ?? null}});
          } finally {timeline.free();}
        } finally {execution.free();}
        return;
      }
      default: throw new Error("no debug session: call debugLaunch first");
    }
  } catch (error) {
    if (message.type==="run") post({type:"result",output:"",error:String(error),errorSource:message.entry,errorLine:null});
    else if ("id" in message) post({type:"error",id:message.id,message:String(error)});
  }
};
ensureReady().then(()=>post({type:"ready"})).catch(()=>{/* Requests report initialization failure. */});
