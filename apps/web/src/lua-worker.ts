/// <reference lib="webworker" />
import init, { execute_project, DebugSession } from "@lua-playground/runtime";
import type { DebugRequest, WorkerEvent } from "./debug-protocol";

export type { WorkerEvent } from "./debug-protocol";

let ready: Promise<unknown> | null = null;
let session: DebugSession | null = null;

async function ensureReady() {
  if (!ready) {
    ready = init();
  }
  await ready;
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

self.onmessage = async (event: MessageEvent<DebugRequest>) => {
  const message = event.data;
  await ensureReady();

  try {
    switch (message.type) {
      case "run": {
        const names = Object.keys(message.files);
        const contents = names.map((name) => message.files[name]);
        const result = execute_project(names, contents, message.entry);
        post({ type: "result", output: result.output, error: result.error ?? null });
        return;
      }

      case "debugLaunch": {
        const names = Object.keys(message.files);
        const contents = names.map((name) => message.files[name]);
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
        const bp = requireSession().set_breakpoint(message.line);
        post({
          type: "debugBreakpoint",
          id: message.id,
          breakpoint: { id: bp.id, line: bp.line, verified: bp.verified },
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
        requireSession().set_breakpoint_hit_condition(
          message.breakpointId,
          message.hitCondition ?? undefined,
        );
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

      case "debugGetStackTrace": {
        const frames = requireSession()
          .get_stack_trace()
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
          .get_locals(message.frameIndex)
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
        const result = requireSession().evaluate(message.expression, message.frameIndex);
        post({
          type: "debugEvalResult",
          id: message.id,
          result: { ok: result.ok, display: result.display },
        });
        return;
      }
      case "debugSetVariable": {
        const result = requireSession().set_variable(
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
    }
  } catch (err) {
    if ("id" in message) {
      post({ type: "error", id: message.id, message: String(err) });
    }
  }
};

ensureReady().then(() => {
  post({ type: "ready" });
});
