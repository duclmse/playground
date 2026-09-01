// The Phase 4-7 debugger UI: call stack, locals/globals inspector, watch
// expressions, and a frame-scoped REPL - the "React" end of the
// `React → DebugSession → LuaDebugger → LuaRuntime → piccolo (WASM)` chain
// from docs/debug-protocol.md. Everything here just calls methods on the
// already-complete `DebugSession` client (debug-session.ts); no debugging
// logic lives in this file.
import { useEffect, useState } from "react";
import type { DebugSession, EvalResultInfo, StackFrameInfo, StopEvent, VariableInfo } from "./debug-session";
import { VariablesTree } from "./VariablesTree";

export interface DebugPanelProps {
  session: DebugSession;
  stop: StopEvent;
  isTerminated: boolean;
  onContinue: () => void;
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
  onContinue,
  onStepOver,
  onStepInto,
  onStepOut,
  onStop,
  onFrameSelected,
}: DebugPanelProps) {
  const [frames, setFrames] = useState<StackFrameInfo[]>([]);
  const [selectedFrame, setSelectedFrame] = useState(0);
  const [locals, setLocals] = useState<VariableInfo[]>([]);
  const [globals, setGlobals] = useState<VariableInfo[]>([]);
  const [watches, setWatches] = useState<{ expression: string; result: EvalResultInfo | null }[]>([]);
  const [watchInput, setWatchInput] = useState("");
  const [replInput, setReplInput] = useState("");
  const [replHistory, setReplHistory] = useState<{ expression: string; result: EvalResultInfo }[]>([]);

  // Every new stop invalidates the previous frame/variable snapshot -
  // matches how `DebugSession`'s Rust side resets its object registry on
  // every stop (see session.rs's `ObjectRegistry::reset` doc comment).
  useEffect(() => {
    setSelectedFrame(0);
    onFrameSelected(0);
    if (isTerminated) {
      setFrames([]);
      setLocals([]);
      return;
    }
    void session.getStackTrace().then(setFrames);
    void session.getLocals(0).then(setLocals);
    void session.getGlobals().then(setGlobals);
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
    void Promise.all(watches.map((w) => session.evaluate(w.expression, selectedFrame))).then(
      (results) => {
        if (cancelled) return;
        setWatches((current) => current.map((w, i) => ({ ...w, result: results[i] })));
      },
    );
    return () => {
      cancelled = true;
    };
    // Only re-run when the *set* of watch expressions or the selected
    // frame/stop changes - not every render (`watches` itself changes on
    // every result update above, which would otherwise loop).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stop, selectedFrame, isTerminated, session, watches.map((w) => w.expression).join("|")]);

  const selectFrame = async (index: number) => {
    setSelectedFrame(index);
    onFrameSelected(index);
    setLocals(await session.getLocals(index));
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
    const result = await session.evaluate(expression, selectedFrame);
    setReplHistory((h) => [...h, { expression, result }]);
    setReplInput("");
  };

  return (
    <section className="debug-panel">
      <div className="debug-toolbar">
        <button type="button" onClick={onContinue} disabled={isTerminated} title="Continue">
          ▶ Continue
        </button>
        <button type="button" onClick={onStepOver} disabled={isTerminated} title="Step Over">
          ⤵ Over
        </button>
        <button type="button" onClick={onStepInto} disabled={isTerminated} title="Step Into">
          ⤷ Into
        </button>
        <button type="button" onClick={onStepOut} disabled={isTerminated} title="Step Out">
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
        ) : (
          <span className="debug-status-paused">
            Paused ({stop.reason}) at line {stop.line}
          </span>
        )}
      </div>

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
        <VariablesTree session={session} variables={locals} />
      </div>

      <div className="debug-section">
        <h3>Globals</h3>
        <VariablesTree session={session} variables={globals} />
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
