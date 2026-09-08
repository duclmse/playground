// TypeScript-side `DebugSession` (docs/debug-protocol.md#debugsession-interface),
// the `LuaDebugger` layer in the documented call chain
// `React → DebugSession → LuaDebugger → LuaRuntime → piccolo (WASM)`.
// This class *is* both boxes: it owns request/response correlation over the
// worker and exposes exactly the promise-based methods the spec defines -
// all real debugging logic (stepping, breakpoints, evaluation) lives in
// `crates/lua-vm/src/session.rs`'s `DebugSession` on the Rust side; this
// file only translates between that and the worker message protocol.
//
// Wired into React by DebugPanel.tsx/VariablesTree.tsx/App.tsx - see
// docs/phase-4-8-implementation.md for the browser-verified feature set
// and what's still unbuilt (profiler/timeline UI, a thread selector for
// coroutines, pause()).

import {
  allocateRequestId,
  type BreakpointInfo,
  type DebugRequest,
  type EvalResultInfo,
  type MemoryStatsInfo,
  type StackFrameInfo,
  type ThreadInfo,
  type VariableInfo,
  type WorkerEvent,
} from "./debug-protocol";

export type {
  BreakpointInfo,
  StackFrameInfo,
  ThreadInfo,
  VariableInfo,
  EvalResultInfo,
  MemoryStatsInfo,
};

// Plain `Omit<DebugRequest, "id">` doesn't distribute over the union (it
// collapses to the shape's *common* keys minus "id", which is why every
// variant-specific field below was rejected as an "unknown property") -
// `DistributiveOmit` maps `Omit` over each member individually first
// (distribution requires the checked type to be a bare type parameter, not
// a union referenced directly - hence the indirection through `T`), keeping
// each variant's own fields intact. `Extract<.., { id: number }>` drops the
// one variant ("run") that has no `id` at all, since this class never sends it.
type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;
type DebugRequestWithoutId = DistributiveOmit<Extract<DebugRequest, { id: number }>, "id">;

export interface StopEvent {
  reason: "breakpoint" | "step" | "exception" | "terminated" | "paused" | "running";
  line: number | null;
  message: string | null;
}

/**
 * Placeholder `StopEvent` for the moment a session is launched but its
 * first `continue()` hasn't resolved yet (see App.tsx's `startDebugging`).
 * Without this, `DebugPanel` - gated on `debugSession && stopEvent` - can't
 * render at all during that first call, so its Pause button doesn't exist
 * yet either: the *only* time a user is guaranteed to be looking at a
 * possibly-long-running `continue()` (right after clicking Debug) would be
 * exactly the one time they have no way to pause it. `reason: "running"` is
 * never actually shown - DebugPanel's status view checks `busy` before it
 * ever reads `stop.reason` - it exists so `isTerminated` (neither
 * "terminated" nor "exception") and the rest of DebugPanel's `stop`-reading
 * effects see a well-formed, harmless value instead of a fake "step"/etc.
 */
export const LAUNCHING_STOP_EVENT: StopEvent = { reason: "running", line: null, message: null };

/**
 * Instructions per `continue()` burst (see `continue()`'s doc comment).
 * Small enough that `pause()` takes effect within roughly one worker
 * round-trip of being called; large enough that a normal (non-paused) run
 * isn't dominated by round-trip overhead - most programs finish or hit a
 * breakpoint in far fewer than this many opcodes, so they complete in a
 * single burst anyway.
 */
const CONTINUE_BURST_SIZE = 200_000;

/**
 * Matches `crates/lua-vm`'s `MAX_INSTRUCTIONS` runaway-loop cap. A single
 * `continue_burst` call has no notion of this - each one just runs its own
 * `CONTINUE_BURST_SIZE` budget and reports back - so without a client-side
 * running total, `continue()`'s burst loop would keep requesting bursts
 * forever for a genuine `while true do end`, instead of eventually failing
 * the same way a single unbounded `continue_()` call always has.
 */
const MAX_CONTINUE_INSTRUCTIONS = 10_000_000;

/** Scope kinds per docs/debug-protocol.md#scopes--locals. */
export type ScopeType = "local" | "global" | "register";

export interface Scope {
  name: string;
  type: ScopeType;
  frameIndex: number;
}

/**
 * Promise-based wrapper around one worker's debug session, matching
 * docs/debug-protocol.md's `DebugSession` interface. One instance per
 * `launch()` - call `launch()` again (or construct a new instance) to debug
 * a different program.
 */
export class DebugSession {
  private worker: Worker;
  private pending = new Map<
    number,
    { resolve: (event: WorkerEvent) => void; reject: (err: Error) => void }
  >();
  private pauseRequested = false;

  constructor(worker: Worker) {
    this.worker = worker;
    worker.addEventListener("message", this.handleMessage);
  }

  dispose() {
    this.worker.removeEventListener("message", this.handleMessage);
    for (const { reject } of this.pending.values()) {
      reject(new Error("DebugSession disposed"));
    }
    this.pending.clear();
  }

  private handleMessage = (event: MessageEvent<WorkerEvent>) => {
    const message = event.data;
    if (!("id" in message)) return; // "ready"/"result" belong to the plain run() path, not us
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    if (message.type === "error") {
      pending.reject(new Error(message.message));
    } else {
      pending.resolve(message);
    }
  };

  private send<T extends WorkerEvent>(request: DebugRequestWithoutId): Promise<T> {
    const id = allocateRequestId();
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (e: WorkerEvent) => void, reject });
      this.worker.postMessage({ ...request, id } as DebugRequest);
    });
  }

  async launch(files: Record<string, string>, entry: string): Promise<void> {
    await this.send({ type: "debugLaunch", files, entry });
  }

  /**
   * Drives the program in `CONTINUE_BURST_SIZE`-instruction bursts (each its
   * own worker round-trip) instead of one unbounded `debugContinue` call, so
   * `pause()` - which just flips a flag this loop checks between bursts -
   * has something to interrupt. Per docs/phase-4-8-implementation.md's
   * `pause()` design: "the driver stops calling step()"; here the driver is
   * this loop, and "stops calling" means "stops requesting another burst."
   * Resolves with a synthetic `{reason: "paused"}` StopEvent when that
   * happens, or with the real stop once a breakpoint/step-target/exception/
   * termination is actually reached, exactly as a single `debugContinue`
   * call used to.
   */
  async continue(): Promise<StopEvent> {
    this.pauseRequested = false;
    let totalInstructions = 0;
    for (;;) {
      const burst = await this.send<Extract<WorkerEvent, { type: "debugBurst" }>>({
        type: "debugContinueBurst",
        maxInstructions: CONTINUE_BURST_SIZE,
      });
      if (burst.stopped) {
        return burst.stop as StopEvent;
      }
      // Not stopped means this burst ran its full requested budget without
      // hitting a real stop condition.
      totalInstructions += CONTINUE_BURST_SIZE;
      if (totalInstructions >= MAX_CONTINUE_INSTRUCTIONS) {
        return {
          reason: "exception",
          line: burst.line,
          message: "Execution exceeded instruction limit",
        };
      }
      if (this.pauseRequested) {
        this.pauseRequested = false;
        return { reason: "paused", line: burst.line, message: null };
      }
    }
  }

  /**
   * Requests that the in-flight `continue()` burst loop stop after its
   * current burst instead of requesting another - see `continue()`'s doc
   * comment. A no-op if nothing is running: the flag is reset at the start
   * of every `continue()` call, so a stray `pause()` called outside one
   * (e.g. while a step is in flight) has no lasting effect.
   */
  async pause(): Promise<void> {
    this.pauseRequested = true;
  }

  async stepOver(): Promise<StopEvent> {
    const { stop } = await this.send<Extract<WorkerEvent, { type: "debugStopped" }>>({
      type: "debugStepOver",
    });
    return stop as StopEvent;
  }

  async stepInto(): Promise<StopEvent> {
    const { stop } = await this.send<Extract<WorkerEvent, { type: "debugStopped" }>>({
      type: "debugStepInto",
    });
    return stop as StopEvent;
  }

  async stepOut(): Promise<StopEvent> {
    const { stop } = await this.send<Extract<WorkerEvent, { type: "debugStopped" }>>({
      type: "debugStepOut",
    });
    return stop as StopEvent;
  }

  /**
   * `sourceId` must match how this session names the file internally: the
   * entry file's own name (as passed to `launch`), or - for a `require()`d
   * file - the *require argument*, not the virtual-FS filename (e.g.
   * `require("lib")` against `lib.lua` uses source id `"lib"`, not
   * `"lib.lua"` - see `install_require` in crates/lua-vm/src/lib.rs).
   */
  async setBreakpoint(sourceId: string, line: number): Promise<BreakpointInfo> {
    const { breakpoint } = await this.send<Extract<WorkerEvent, { type: "debugBreakpoint" }>>({
      type: "debugSetBreakpoint",
      sourceId,
      line,
    });
    return breakpoint;
  }

  async removeBreakpoint(breakpointId: number): Promise<void> {
    await this.send({ type: "debugRemoveBreakpoint", breakpointId });
  }

  /** Phase 8: condition evaluated in frame 0 on every hit; `null` clears it. */
  async setBreakpointCondition(breakpointId: number, condition: string | null): Promise<void> {
    await this.send({ type: "debugSetBreakpointCondition", breakpointId, condition });
  }

  /** Phase 8: only stop from the Nth hit onward; `null` clears it. */
  async setBreakpointHitCondition(breakpointId: number, hitCondition: number | null): Promise<void> {
    await this.send({ type: "debugSetBreakpointHitCondition", breakpointId, hitCondition });
  }

  /** Phase 8: log instead of stopping; `null` turns the breakpoint back into a normal one. */
  async setBreakpointLogMessage(breakpointId: number, logMessage: string | null): Promise<void> {
    await this.send({ type: "debugSetBreakpointLogMessage", breakpointId, logMessage });
  }

  /**
   * Phase 8 (docs/debug-protocol.md#advanced-coroutines-phase-8): the
   * active thread nesting at this pause point - `id: 0` is always the main
   * thread; a coroutine currently on the resume chain gets a higher id.
   * See `crates/lua-vm/src/session.rs`'s `Executor::debug_thread_stack` doc
   * comment for exactly what this does and doesn't cover (only threads on
   * the *active* resume chain, not every coroutine the program has ever
   * created). `threadId` here is what `getStackTrace`/`getLocals`/
   * `evaluate`/`setVariable`'s own `threadId` parameter expects.
   */
  async getThreads(): Promise<ThreadInfo[]> {
    const { threads } = await this.send<Extract<WorkerEvent, { type: "debugThreads" }>>({
      type: "debugGetThreads",
    });
    return threads;
  }

  async getStackTrace(threadId = 0): Promise<StackFrameInfo[]> {
    const { frames } = await this.send<Extract<WorkerEvent, { type: "debugStackTrace" }>>({
      type: "debugGetStackTrace",
      threadId,
    });
    return frames;
  }

  /**
   * docs/debug-protocol.md's `getScopes(frameId)` - unlike the spec's
   * separate `getVariables(reference)` call per scope, this returns the
   * scope *descriptors* only; fetch each scope's variables with
   * `getLocals`/`getGlobals`. Upvalues are not exposed (see
   * docs/phase-4-8-implementation.md).
   */
  getScopes(frameIndex: number): Scope[] {
    return [
      { name: "Locals", type: "local", frameIndex },
      { name: "Globals", type: "global", frameIndex },
    ];
  }

  async getLocals(threadId: number, frameIndex: number): Promise<VariableInfo[]> {
    const { variables } = await this.send<Extract<WorkerEvent, { type: "debugVariables" }>>({
      type: "debugGetLocals",
      threadId,
      frameIndex,
    });
    return variables;
  }

  /** Upvalues captured by the closure running at `frameIndex` - see `session.rs`'s `get_upvalues` doc comment. */
  async getUpvalues(threadId: number, frameIndex: number): Promise<VariableInfo[]> {
    const { variables } = await this.send<Extract<WorkerEvent, { type: "debugVariables" }>>({
      type: "debugGetUpvalues",
      threadId,
      frameIndex,
    });
    return variables;
  }

  async getGlobals(): Promise<VariableInfo[]> {
    const { variables } = await this.send<Extract<WorkerEvent, { type: "debugVariables" }>>({
      type: "debugGetGlobals",
    });
    return variables;
  }

  /** docs/debug-protocol.md's `getVariables(reference, { start, count })` for a table reference. */
  async getVariables(reference: number, start = 0, count = 100): Promise<VariableInfo[]> {
    const { variables } = await this.send<Extract<WorkerEvent, { type: "debugVariables" }>>({
      type: "debugGetTableEntries",
      reference,
      start,
      count,
    });
    return variables;
  }

  async getMetatable(reference: number): Promise<number | null> {
    const { reference: metaRef } = await this.send<Extract<WorkerEvent, { type: "debugMetatable" }>>({
      type: "debugGetMetatable",
      reference,
    });
    return metaRef;
  }

  async evaluate(threadId: number, expression: string, frameIndex: number): Promise<EvalResultInfo> {
    const { result } = await this.send<Extract<WorkerEvent, { type: "debugEvalResult" }>>({
      type: "debugEvaluate",
      threadId,
      expression,
      frameIndex,
    });
    return result;
  }

  async setVariable(
    threadId: number,
    frameIndex: number,
    name: string,
    valueExpr: string,
  ): Promise<EvalResultInfo> {
    const { result } = await this.send<Extract<WorkerEvent, { type: "debugEvalResult" }>>({
      type: "debugSetVariable",
      threadId,
      frameIndex,
      name,
      valueExpr,
    });
    return result;
  }

  /** Buffered `print()` output since the last call - poll after every stop. */
  async takeOutput(): Promise<string> {
    const { text } = await this.send<Extract<WorkerEvent, { type: "debugOutput" }>>({
      type: "debugTakeOutput",
    });
    return text;
  }

  /** Live allocation/GC stats for the paused program - see `DebugSession::get_memory_stats`. */
  async getMemoryStats(): Promise<MemoryStatsInfo> {
    const { stats } = await this.send<Extract<WorkerEvent, { type: "debugMemoryStats" }>>({
      type: "debugGetMemoryStats",
    });
    return stats;
  }

  /** Forces a full GC cycle, then returns the resulting stats (saves a round trip). */
  async forceGc(): Promise<MemoryStatsInfo> {
    const { stats } = await this.send<Extract<WorkerEvent, { type: "debugMemoryStats" }>>({
      type: "debugForceGc",
    });
    return stats;
  }
}
