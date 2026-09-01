// TypeScript-side `DebugSession` (docs/debug-protocol.md#debugsession-interface),
// the `LuaDebugger` layer in the documented call chain
// `React → DebugSession → LuaDebugger → LuaRuntime → piccolo (WASM)`.
// This class *is* both boxes: it owns request/response correlation over the
// worker and exposes exactly the promise-based methods the spec defines -
// all real debugging logic (stepping, breakpoints, evaluation) lives in
// `crates/lua-vm/src/session.rs`'s `DebugSession` on the Rust side; this
// file only translates between that and the worker message protocol.
//
// Not yet wired into any React component - see
// docs/phase-4-8-implementation.md for what's built vs. what a UI still
// needs to add (breakpoint gutter, call stack panel, variables tree,
// step/continue controls, watch/REPL panel).

import type {
  BreakpointInfo,
  DebugRequest,
  EvalResultInfo,
  StackFrameInfo,
  VariableInfo,
  WorkerEvent,
} from "./debug-protocol";

export type { BreakpointInfo, StackFrameInfo, VariableInfo, EvalResultInfo };

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
  reason: "breakpoint" | "step" | "exception" | "terminated";
  line: number | null;
  message: string | null;
}

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
  private nextId = 0;
  private pending = new Map<
    number,
    { resolve: (event: WorkerEvent) => void; reject: (err: Error) => void }
  >();

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
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (e: WorkerEvent) => void, reject });
      this.worker.postMessage({ ...request, id } as DebugRequest);
    });
  }

  async launch(files: Record<string, string>, entry: string): Promise<void> {
    await this.send({ type: "debugLaunch", files, entry });
  }

  async continue(): Promise<StopEvent> {
    const { stop } = await this.send<Extract<WorkerEvent, { type: "debugStopped" }>>({
      type: "debugContinue",
    });
    return stop as StopEvent;
  }

  // `pause()` has no separate worker message: piccolo's fuel-stepped model
  // means "pause" is just "the driver stops calling step()" (per
  // docs/debug-protocol.md#worker-message-protocol) - there is no
  // long-running `continue()` call to interrupt mid-flight here, since
  // `DebugSession::continue_()` on the Rust side runs to completion inside
  // one synchronous call. A responsive pause button needs the Rust side to
  // run in bounded bursts instead of one unbounded call - see
  // docs/phase-4-8-implementation.md's "pause()" section for the design
  // that would enable it; not implemented in this pass.
  async pause(): Promise<void> {
    throw new Error(
      "pause() is not implemented - continue() currently runs to completion in one call; see docs/phase-4-8-implementation.md",
    );
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

  async setBreakpoint(line: number): Promise<BreakpointInfo> {
    const { breakpoint } = await this.send<Extract<WorkerEvent, { type: "debugBreakpoint" }>>({
      type: "debugSetBreakpoint",
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

  async getStackTrace(): Promise<StackFrameInfo[]> {
    const { frames } = await this.send<Extract<WorkerEvent, { type: "debugStackTrace" }>>({
      type: "debugGetStackTrace",
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

  async getLocals(frameIndex: number): Promise<VariableInfo[]> {
    const { variables } = await this.send<Extract<WorkerEvent, { type: "debugVariables" }>>({
      type: "debugGetLocals",
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

  async evaluate(expression: string, frameIndex: number): Promise<EvalResultInfo> {
    const { result } = await this.send<Extract<WorkerEvent, { type: "debugEvalResult" }>>({
      type: "debugEvaluate",
      expression,
      frameIndex,
    });
    return result;
  }

  async setVariable(frameIndex: number, name: string, valueExpr: string): Promise<EvalResultInfo> {
    const { result } = await this.send<Extract<WorkerEvent, { type: "debugEvalResult" }>>({
      type: "debugSetVariable",
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
}
