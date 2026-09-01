// Shared request/response types for the debug worker protocol
// (docs/debug-protocol.md#worker-message-protocol, extended for Phases 4-7).
//
// Every debug request carries an `id`; the worker echoes it back on the
// matching response so `DebugSession` (debug-session.ts) can resolve the
// right pending Promise - `postMessage` itself has no request/response
// correlation built in.

export type StackFrameInfo = {
  index: number;
  name: string;
  source: string;
  line: number | null;
  functionType: string;
};

export type VariableInfo = {
  name: string;
  valueType: string;
  display: string;
  expandable: boolean;
  reference: number | null;
};

export type BreakpointInfo = {
  id: number;
  sourceId: string;
  line: number;
  verified: boolean;
};

export type StopInfo = {
  reason: string;
  line: number | null;
  message: string | null;
};

export type EvalResultInfo = {
  ok: boolean;
  display: string;
};

export type DebugRequest =
  | { type: "run"; files: Record<string, string>; entry: string }
  | { id: number; type: "debugLaunch"; files: Record<string, string>; entry: string }
  | { id: number; type: "debugContinue" }
  | { id: number; type: "debugStepOver" }
  | { id: number; type: "debugStepInto" }
  | { id: number; type: "debugStepOut" }
  | { id: number; type: "debugSetBreakpoint"; sourceId: string; line: number }
  | { id: number; type: "debugRemoveBreakpoint"; breakpointId: number }
  | {
      id: number;
      type: "debugSetBreakpointCondition";
      breakpointId: number;
      condition: string | null;
    }
  | {
      id: number;
      type: "debugSetBreakpointHitCondition";
      breakpointId: number;
      hitCondition: number | null;
    }
  | {
      id: number;
      type: "debugSetBreakpointLogMessage";
      breakpointId: number;
      logMessage: string | null;
    }
  | { id: number; type: "debugGetStackTrace" }
  | { id: number; type: "debugGetLocals"; frameIndex: number }
  | { id: number; type: "debugGetGlobals" }
  | { id: number; type: "debugGetTableEntries"; reference: number; start: number; count: number }
  | { id: number; type: "debugGetMetatable"; reference: number }
  | { id: number; type: "debugEvaluate"; expression: string; frameIndex: number }
  | {
      id: number;
      type: "debugSetVariable";
      frameIndex: number;
      name: string;
      valueExpr: string;
    }
  | { id: number; type: "debugTakeOutput" };

export type WorkerEvent =
  | { type: "ready" }
  | { type: "result"; output: string; error: string | null }
  | { type: "error"; id: number; message: string }
  | { type: "debugLaunched"; id: number }
  | { type: "debugStopped"; id: number; stop: StopInfo }
  | { type: "debugBreakpoint"; id: number; breakpoint: BreakpointInfo }
  | { type: "debugAck"; id: number }
  | { type: "debugStackTrace"; id: number; frames: StackFrameInfo[] }
  | { type: "debugVariables"; id: number; variables: VariableInfo[] }
  | { type: "debugMetatable"; id: number; reference: number | null }
  | { type: "debugEvalResult"; id: number; result: EvalResultInfo }
  | { type: "debugOutput"; id: number; text: string };
