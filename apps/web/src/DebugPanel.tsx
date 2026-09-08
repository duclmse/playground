// The Phase 4-7 debugger UI: call stack, locals/globals inspector, watch
// expressions, and a frame-scoped REPL - the "React" end of the
// `React → DebugSession → LuaDebugger → LuaRuntime → piccolo (WASM)` chain
// from docs/debug-protocol.md. Everything here just calls methods on the
// already-complete `DebugSession` client (debug-session.ts); no debugging
// logic lives in this file.
import { useEffect, useState } from "react";
import type {
  DebugSession,
  EvalResultInfo,
  MemoryStatsInfo,
  StackFrameInfo,
  StopEvent,
  ThreadInfo,
  VariableInfo,
} from "./debug-session";
import { VariablesTree } from "./VariablesTree";

export interface DebugPanelProps {
  session: DebugSession;
  stop: StopEvent;
  isTerminated: boolean;
  /** True while a debug action (continue/step) is in flight - see App.tsx's `debugBusy`. */
  busy: boolean;
  onContinue: () => void;
  onPause: () => void;
  onStepOver: () => void;
  onStepInto: () => void;
  onStepOut: () => void;
  onStop: () => void;
  onFrameSelected: (frameIndex: number) => void;
}

export function DebugPanel({
  session,
  stop,
  isTerminated,
  busy,
  onContinue,
  onPause,
  onStepOver,
  onStepInto,
  onStepOut,
  onStop,
  onFrameSelected,
}: DebugPanelProps) {
  const [threads, setThreads] = useState<ThreadInfo[]>([]);
  const [selectedThread, setSelectedThread] = useState(0);
  const [frames, setFrames] = useState<StackFrameInfo[]>([]);
  const [selectedFrame, setSelectedFrame] = useState(0);
  const [locals, setLocals] = useState<VariableInfo[]>([]);
  const [upvalues, setUpvalues] = useState<VariableInfo[]>([]);
  const [globals, setGlobals] = useState<VariableInfo[]>([]);
  const [watches, setWatches] = useState<{ expression: string; result: EvalResultInfo | null }[]>([]);
  const [watchInput, setWatchInput] = useState("");
  const [memoryStats, setMemoryStats] = useState<MemoryStatsInfo | null>(null);
  const [replInput, setReplInput] = useState("");
  const [replHistory, setReplHistory] = useState<{ expression: string; result: EvalResultInfo }[]>([]);

  // Every new stop invalidates the previous thread/frame/variable snapshot -
  // matches how `DebugSession`'s Rust side resets its object registry on
  // every stop (see session.rs's `ObjectRegistry::reset` doc comment).
  // Phase 8 (docs/debug-protocol.md#advanced-coroutines-phase-8): defaults
  // to whichever thread `getThreads()` marks "running" - the thread that
  // actually hit the breakpoint/step, which is a coroutine whenever the
  // stop happened inside one, not always the main thread.
  useEffect(() => {
    setSelectedFrame(0);
    onFrameSelected(0);
    if (isTerminated) {
      setThreads([]);
      setFrames([]);
      setLocals([]);
      setUpvalues([]);
      setMemoryStats(null);
      return;
    }
    void session.getThreads().then((ts) => {
      setThreads(ts);
      const running = ts.find((t) => t.status === "running")?.id ?? 0;
      setSelectedThread(running);
      void session.getStackTrace(running).then(setFrames);
      void session.getLocals(running, 0).then(setLocals);
      void session.getUpvalues(running, 0).then(setUpvalues);
    });
    void session.getGlobals().then(setGlobals);
    void session.getMemoryStats().then(setMemoryStats);
    // Re-run every watch against the newly paused frame, per
    // docs/debug-protocol.md#evaluation ("Watch expressions - re-evaluated
    // on every stop").
    setWatches((current) =>
      current.map((w) => ({ ...w, result: null })),
    );
  }, [stop, isTerminated, session, onFrameSelected]);

  useEffect(() => {
    if (watches.length === 0 || isTerminated) return;
    let cancelled = false;
    void Promise.all(
      watches.map((w) => session.evaluate(selectedThread, w.expression, selectedFrame)),
    ).then((results) => {
      if (cancelled) return;
      setWatches((current) => current.map((w, i) => ({ ...w, result: results[i] })));
    });
    return () => {
      cancelled = true;
    };
    // Only re-run when the *set* of watch expressions or the selected
    // thread/frame/stop changes - not every render (`watches` itself
    // changes on every result update above, which would otherwise loop).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    stop,
    selectedThread,
    selectedFrame,
    isTerminated,
    session,
    watches.map((w) => w.expression).join("|"),
  ]);

  const selectThread = async (threadId: number) => {
    setSelectedThread(threadId);
    setSelectedFrame(0);
    onFrameSelected(0);
    setFrames(await session.getStackTrace(threadId));
    setLocals(await session.getLocals(threadId, 0));
    setUpvalues(await session.getUpvalues(threadId, 0));
  };

  const selectFrame = async (index: number) => {
    setSelectedFrame(index);
    onFrameSelected(index);
    setLocals(await session.getLocals(selectedThread, index));
    setUpvalues(await session.getUpvalues(selectedThread, index));
  };

  const addWatch = () => {
    const expression = watchInput.trim();
    if (!expression) return;
    setWatches((w) => [...w, { expression, result: null }]);
    setWatchInput("");
  };

  const removeWatch = (index: number) => {
    setWatches((w) => w.filter((_, i) => i !== index));
  };

  const runRepl = async () => {
    const expression = replInput.trim();
    if (!expression || isTerminated) return;
    const result = await session.evaluate(selectedThread, expression, selectedFrame);
    setReplHistory((h) => [...h, { expression, result }]);
    setReplInput("");
  };

  /**
   * After a successful `setVariable` from a Locals/Upvalues row: refresh
   * both lists (the edit could have been to either, and either can
   * reference the other) and re-run every watch, since a watch expression
   * may reference the variable that just changed.
   */
  const refreshAfterEdit = async () => {
    const [newLocals, newUpvalues] = await Promise.all([
      session.getLocals(selectedThread, selectedFrame),
      session.getUpvalues(selectedThread, selectedFrame),
    ]);
    setLocals(newLocals);
    setUpvalues(newUpvalues);
    if (watches.length > 0) {
      const results = await Promise.all(
        watches.map((w) => session.evaluate(selectedThread, w.expression, selectedFrame)),
      );
      setWatches((current) => current.map((w, i) => ({ ...w, result: results[i] })));
    }
  };

  const forceGc = async () => {
    setMemoryStats(await session.forceGc());
  };

  return (
    <section className="debug-panel">
      <div className="debug-toolbar">
        <button type="button" onClick={onContinue} disabled={isTerminated || busy} title="Continue (F5)">
          ▶ Continue
        </button>
        <button type="button" onClick={onPause} disabled={isTerminated || !busy} title="Pause">
          ⏸ Pause
        </button>
        <button type="button" onClick={onStepOver} disabled={isTerminated || busy} title="Step Over (F10)">
          ⤵ Over
        </button>
        <button type="button" onClick={onStepInto} disabled={isTerminated || busy} title="Step Into">
          ⤷ Into
        </button>
        <button type="button" onClick={onStepOut} disabled={isTerminated || busy} title="Step Out">
          ⤴ Out
        </button>
        <button type="button" onClick={onStop} className="debug-stop" title="Stop debugging">
          ■ Stop
        </button>
      </div>

      <div className="debug-status">
        {isTerminated ? (
          <span className="debug-status-terminated">
            {stop.reason === "exception" ? `Error: ${stop.message}` : "Terminated"}
          </span>
        ) : busy ? (
          <span className="debug-status-running">Running…</span>
        ) : (
          <span className="debug-status-paused">
            {stop.reason === "paused" ? "Paused by user" : `Paused (${stop.reason})`} at line {stop.line}
          </span>
        )}
      </div>

      {threads.length > 1 && (
        <div className="debug-section">
          <h3>Threads</h3>
          <ul className="thread-list">
            {threads.map((t) => (
              <li key={t.id}>
                <button
                  type="button"
                  className={t.id === selectedThread ? "active" : ""}
                  onClick={() => selectThread(t.id)}
                >
                  {t.id === 0 ? "main" : `coroutine #${t.id}`} ({t.status})
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="debug-section">
        <h3>Call Stack</h3>
        <ul className="call-stack">
          {frames.map((f) => (
            <li key={f.index === -1 || f.functionType === "c" ? Math.random() : f.index}>
              <button
                type="button"
                className={f.index === selectedFrame ? "active" : ""}
                disabled={f.functionType === "c"}
                onClick={() => selectFrame(f.index)}
              >
                {f.name} {f.source ? `${f.source}:${f.line ?? "?"}` : ""}
              </button>
            </li>
          ))}
        </ul>
      </div>

      <div className="debug-section">
        <h3>Locals</h3>
        <VariablesTree
          session={session}
          variables={locals}
          editable
          threadId={selectedThread}
          frameIndex={selectedFrame}
          onEdited={refreshAfterEdit}
        />
      </div>

      <div className="debug-section">
        <h3>Upvalues</h3>
        <VariablesTree
          session={session}
          variables={upvalues}
          editable
          threadId={selectedThread}
          frameIndex={selectedFrame}
          onEdited={refreshAfterEdit}
        />
      </div>

      <div className="debug-section">
        <h3>Globals</h3>
        <VariablesTree session={session} variables={globals} />
      </div>

      <div className="debug-section">
        <h3>Memory</h3>
        {memoryStats ? (
          <dl className="memory-stats">
            <div className="memory-stat-row">
              <dt>Total</dt>
              <dd>{(memoryStats.totalAllocation / 1024).toFixed(1)} KB</dd>
            </div>
            <div className="memory-stat-row">
              <dt>GC-managed</dt>
              <dd>{(memoryStats.gcAllocation / 1024).toFixed(1)} KB</dd>
            </div>
            <div className="memory-stat-row">
              <dt>External</dt>
              <dd>{(memoryStats.externalAllocation / 1024).toFixed(1)} KB</dd>
            </div>
            <div className="memory-stat-row">
              <dt>Allocation debt</dt>
              <dd>{memoryStats.allocationDebt.toFixed(1)}</dd>
            </div>
          </dl>
        ) : (
          <div className="variables-empty">(none)</div>
        )}
        <button type="button" onClick={forceGc} disabled={isTerminated}>
          Force GC
        </button>
      </div>

      <div className="debug-section">
        <h3>Watch</h3>
        <div className="watch-input-row">
          <input
            type="text"
            value={watchInput}
            onChange={(e) => setWatchInput(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && addWatch()}
            placeholder="expression"
          />
          <button type="button" onClick={addWatch}>
            +
          </button>
        </div>
        <ul className="watch-list">
          {watches.map((w, i) => (
            <li key={i}>
              <span className="watch-expr">{w.expression}</span>
              <span className={w.result && !w.result.ok ? "watch-error" : "watch-value"}>
                {w.result ? w.result.display : "…"}
              </span>
              <button type="button" className="icon-button" onClick={() => removeWatch(i)}>
                ×
              </button>
            </li>
          ))}
        </ul>
      </div>

      <div className="debug-section repl-section">
        <h3>REPL (frame {selectedFrame})</h3>
        <div className="repl-history">
          {replHistory.map((h, i) => (
            <div key={i} className="repl-entry">
              <div className="repl-input-line">&gt; {h.expression}</div>
              <div className={h.result.ok ? "repl-result" : "repl-error"}>{h.result.display}</div>
            </div>
          ))}
        </div>
        <input
          type="text"
          value={replInput}
          onChange={(e) => setReplInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && runRepl()}
          placeholder="evaluate an expression…"
          disabled={isTerminated}
        />
      </div>
    </section>
  );
}
