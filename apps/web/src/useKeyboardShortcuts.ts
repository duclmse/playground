// F5 (run/continue/launch-if-breakpoints-set)/F9 (toggle breakpoint)/F10
// (step over) - split out of App.tsx, which was otherwise growing past
// ~850 lines.
//
// A mount-once window listener reading everything it needs through refs,
// rather than depending on `run`/`doDebugAction`/`toggleBreakpoint` (all
// three are redefined every App render, closing over that render's
// `project`/`debugSession`/etc.) in a dependency array: re-registering the
// listener on every relevant change would work too, but a stale-closure
// effect bug earlier in this project (DebugPanel's `onFrameSelected`) came
// from exactly this class of mistake - refs updated inline (matching
// App.tsx's own `activeFileRef` pattern) sidestep it entirely. Values that
// already have a ref owned by the caller (`editorRef`/`activeFileRef`, used
// elsewhere in App.tsx too) are taken directly; everything else gets its
// own ref internally.
import { useEffect, useRef, type RefObject } from "react";
import type * as monacoEditor from "monaco-editor";
import type { DebugSession, StopEvent } from "./debug-session";

export interface KeyboardShortcutsOptions {
  run: () => void;
  startDebugging: () => void | Promise<void>;
  doDebugAction: (action: (s: DebugSession) => Promise<StopEvent>) => void | Promise<void>;
  toggleBreakpoint: (file: string, line: number) => void | Promise<void>;
  debugSession: DebugSession | null;
  debugBusy: boolean;
  isTerminated: boolean;
  hasBreakpoints: boolean;
  editorRef: RefObject<monacoEditor.editor.IStandaloneCodeEditor | null>;
  activeFileRef: RefObject<string>;
}

export function useKeyboardShortcuts({
  run,
  startDebugging,
  doDebugAction,
  toggleBreakpoint,
  debugSession,
  debugBusy,
  isTerminated,
  hasBreakpoints,
  editorRef,
  activeFileRef,
}: KeyboardShortcutsOptions) {
  const runRef = useRef(run);
  runRef.current = run;
  const startDebuggingRef = useRef(startDebugging);
  startDebuggingRef.current = startDebugging;
  const doDebugActionRef = useRef(doDebugAction);
  doDebugActionRef.current = doDebugAction;
  const toggleBreakpointRef = useRef(toggleBreakpoint);
  toggleBreakpointRef.current = toggleBreakpoint;
  const debugSessionRef = useRef(debugSession);
  debugSessionRef.current = debugSession;
  const debugBusyRef = useRef(debugBusy);
  debugBusyRef.current = debugBusy;
  const isTerminatedRef = useRef(isTerminated);
  isTerminatedRef.current = isTerminated;
  const hasBreakpointsRef = useRef(hasBreakpoints);
  hasBreakpointsRef.current = hasBreakpoints;

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      // Don't hijack F-keys while the user is typing in the Watch/REPL
      // inputs (or any other text field).
      if (target?.tagName === "INPUT" || target?.tagName === "TEXTAREA") return;

      if (e.key === "F5") {
        e.preventDefault();
        if (debugSessionRef.current) {
          if (!debugBusyRef.current && !isTerminatedRef.current) {
            void doDebugActionRef.current(s => s.continue());
          }
        } else if (hasBreakpointsRef.current) {
          // Breakpoints only ever fire under a debug session (a plain Run
          // ignores them entirely) - so if any are set, F5 should launch one
          // instead of silently running straight past them.
          void startDebuggingRef.current();
        } else {
          runRef.current();
        }
        return;
      }
      if (e.key === "F9") {
        e.preventDefault();
        const line = editorRef.current?.getPosition()?.lineNumber;
        if (line != null) void toggleBreakpointRef.current(activeFileRef.current, line);
        return;
      }
      if (e.key === "F10" && debugSessionRef.current && !debugBusyRef.current && !isTerminatedRef.current) {
        e.preventDefault();
        void doDebugActionRef.current(s => s.stepOver());
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
