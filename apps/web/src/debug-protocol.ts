// Shared request/response types for the debug worker protocol
// (docs/debug-protocol.md#worker-message-protocol, extended for Phases 4-7).
//
// Every debug request carries an `id`; the worker echoes it back on the
// matching response so `DebugSession` (debug-session.ts) can resolve the
// right pending Promise - `postMessage` itself has no request/response
// correlation built in.

/**
 * One id sequence shared by every id-correlated caller of the single Lua
 * worker (`DebugSession` in debug-session.ts, the one-shot profiler/timeline
 * calls in analysis.ts). All of them listen for "message" on the *same*
 * `Worker` object and filter by `id`, so two callers minting ids from their
 * own counters (both naturally starting at 0) could otherwise collide and
 * resolve each other's pending promise with the wrong response - confirmed
 * live: firing a Profile request immediately followed by starting a debug
 * session produced an uncaught `TypeError` and left the debugger UI
 * permanently stuck. A single shared counter makes every id on the wire
 * unique regardless of which caller sent it.
 */
let nextRequestId = 0;
export function allocateRequestId(): number {
  return nextRequestId++;
}

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

/** Phase 8 (docs/debug-protocol.md#advanced-coroutines-phase-8). */
export type ThreadInfo = {
  id: number;
  status: string;
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

/** Live allocation/GC stats for a paused session - see `DebugSession::get_memory_stats`. */
export type MemoryStatsInfo = {
  totalAllocation: number;
  gcAllocation: number;
  externalAllocation: number;
  allocationDebt: number;
};

/** Phase 8 (docs/debug-protocol.md#advanced-profiler-phase-8): one entry of `profile()`. */
export type FunctionStatsInfo = {
  functionId: string;
  calls: number;
  totalInstructions: number;
  selfInstructions: number;
};

/** Phase 8 (docs/debug-protocol.md#advanced-execution-timeline-phase-8): one entry of `recordTimeline()`. */
export type TimelineEventInfo = {
  eventType: string;
  source: string | null;
  line: number | null;
  local0: string | null;
  /** Opcode steps since the previously recorded event (see `DebugEvent::duration`). */
  duration: number;
};

export type TimelineInfo = {
  events: TimelineEventInfo[];
  truncated: boolean;
  error: string | null;
};

export type DebugRequest =
  | { type: "run"; files: Record<string, string>; entry: string }
  | { id: number; type: "debugLaunch"; files: Record<string, string>; entry: string }
  | { id: number; type: "debugContinue" }
  /** pause() support: run at most `maxInstructions` opcodes, see debug-session.ts's `continue()`. */
  | { id: number; type: "debugContinueBurst"; maxInstructions: number }
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
  | { id: number; type: "debugGetThreads" }
  | { id: number; type: "debugGetStackTrace"; threadId: number }
  | { id: number; type: "debugGetLocals"; threadId: number; frameIndex: number }
  | { id: number; type: "debugGetUpvalues"; threadId: number; frameIndex: number }
  | { id: number; type: "debugGetGlobals" }
  | { id: number; type: "debugGetTableEntries"; reference: number; start: number; count: number }
  | { id: number; type: "debugGetMetatable"; reference: number }
  | {
      id: number;
      type: "debugEvaluate";
      threadId: number;
      expression: string;
      frameIndex: number;
    }
  | {
      id: number;
      type: "debugSetVariable";
      threadId: number;
      frameIndex: number;
      name: string;
      valueExpr: string;
    }
  | { id: number; type: "debugTakeOutput" }
  | { id: number; type: "debugGetMemoryStats" }
  | { id: number; type: "debugForceGc" }
  | { id: number; type: "profile"; files: Record<string, string>; entry: string }
  | {
      id: number;
      type: "recordTimeline";
      files: Record<string, string>;
      entry: string;
      maxEvents: number;
    };

export type WorkerEvent =
  | { type: "ready" }
  | {
      type: "result";
      output: string;
      error: string | null;
      /** Inline-diagnostics support: where `error` happened, if recoverable - see `ExecuteResult::error_source`. */
      errorSource: string | null;
      errorLine: number | null;
    }
  | { type: "error"; id: number; message: string }
  | { type: "debugLaunched"; id: number }
  | { type: "debugStopped"; id: number; stop: StopInfo }
  | {
      type: "debugBurst";
      id: number;
      stopped: boolean;
      stop: StopInfo | null;
      source: string | null;
      line: number | null;
    }
  | { type: "debugBreakpoint"; id: number; breakpoint: BreakpointInfo }
  | { type: "debugAck"; id: number }
  | { type: "debugThreads"; id: number; threads: ThreadInfo[] }
  | { type: "debugStackTrace"; id: number; frames: StackFrameInfo[] }
  | { type: "debugVariables"; id: number; variables: VariableInfo[] }
  | { type: "debugMetatable"; id: number; reference: number | null }
  | { type: "debugEvalResult"; id: number; result: EvalResultInfo }
  | { type: "debugOutput"; id: number; text: string }
  | { type: "debugMemoryStats"; id: number; stats: MemoryStatsInfo }
  | { type: "profileResult"; id: number; stats: FunctionStatsInfo[] }
  | { type: "timelineResult"; id: number; timeline: TimelineInfo };
