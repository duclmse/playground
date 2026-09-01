import Editor, {type OnMount} from "@monaco-editor/react";
import * as monacoEditor from "monaco-editor";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import "./monaco-setup";
import type {WorkerEvent} from "./lua-worker";
import type {FunctionStatsInfo, TimelineEventInfo} from "./debug-protocol";
import {runProfile, runTimeline} from "./analysis";
import {DebugSession, LAUNCHING_STOP_EVENT, type StopEvent} from "./debug-session";
import {DebugPanel} from "./DebugPanel";
import {ProfilerPanel} from "./ProfilerPanel";
import {TimelinePanel} from "./TimelinePanel";
import {
  downloadProject,
  isValidFileName,
  loadProject,
  parseProjectFile,
  saveProject,
  type Project,
} from "./project";
import "./App.css";

const TIMELINE_MAX_EVENTS = 5000;

type Analysis =
  | {type: "profile"; stats: FunctionStatsInfo[]}
  | {type: "timeline"; events: TimelineEventInfo[]; truncated: boolean};

type Status = "loading" | "ready" | "running";

function runButtonLabel(status: Status): string {
  switch (status) {
    case "loading":
      return "Loading VM…";
    case "running":
      return "Running…";
    case "ready":
      return "Run";
  }
}

/**
 * Maps an open file to the `source_id` `DebugSession.setBreakpoint` expects:
 * the entry file uses its own name verbatim, but a `require()`d file is
 * named after the *require argument*, not its virtual-FS filename (see
 * debug-session.ts's `setBreakpoint` doc comment). This assumes the common
 * `require("name")` <-> file `"name.lua"` convention the project's own
 * default files use - a project that calls `require()` with something else
 * for a given file needs its breakpoints set accordingly; that isn't
 * something a filename alone can tell us.
 */
function sourceIdFor(fileName: string, entry: string): string {
  return fileName === entry ? fileName : fileName.replace(/\.lua$/, "");
}

/**
 * Inverse of `sourceIdFor`: maps an error/stop's `source` (a `source_id`,
 * per `sourceIdFor`'s doc comment) back to the open project file it names,
 * for placing a Monaco marker on the right model. `null` if it doesn't
 * resolve to any open file (e.g. the common-convention assumption
 * `sourceIdFor` documents doesn't hold for this project).
 */
function fileNameForSourceId(
  sourceId: string,
  files: Record<string, string>,
): string | null {
  if (files[sourceId] !== undefined) return sourceId;
  const withSuffix = `${sourceId}.lua`;
  return files[withSuffix] !== undefined ? withSuffix : null;
}

/** An inline-diagnostics marker: where the last run/debug error happened. */
type ErrorMarker = { source: string; line: number; message: string };

/** User-resizable pane sizes (px), persisted so a reload keeps the layout. */
type PaneSizes = {
  fileTreeWidth: number;
  consoleHeight: number;
  sidePanelWidth: number;
};

const PANE_SIZES_KEY = "lua-playground:paneSizes";
const DEFAULT_PANE_SIZES: PaneSizes = {fileTreeWidth: 180, consoleHeight: 220, sidePanelWidth: 320};

function loadPaneSizes(): PaneSizes {
  try {
    const raw = localStorage.getItem(PANE_SIZES_KEY);
    if (!raw) return DEFAULT_PANE_SIZES;
    return {...DEFAULT_PANE_SIZES, ...JSON.parse(raw)};
  } catch {
    return DEFAULT_PANE_SIZES;
  }
}

function App() {
  const [project, setProject] = useState<Project>(() => loadProject());
  const [activeFile, setActiveFile] = useState<string>(() => Object.keys(loadProject().files)[0]);
  const [output, setOutput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<Status>("loading");
  const workerRef = useRef<Worker | null>(null);

  // ---- Debugger state (docs/debug-protocol.md's DebugSession, Phases 4-7) ----
  const [breakpoints, setBreakpoints] = useState<Record<string, number[]>>({});
  const breakpointIdsRef = useRef<Record<string, Record<number, number>>>({});
  const [debugSession, setDebugSession] = useState<DebugSession | null>(null);
  const [stopEvent, setStopEvent] = useState<StopEvent | null>(null);
  const [debugBusy, setDebugBusy] = useState(false);
  const [analysis, setAnalysis] = useState<Analysis | null>(null);
  const [analysisBusy, setAnalysisBusy] = useState(false);
  const editorRef = useRef<monacoEditor.editor.IStandaloneCodeEditor | null>(null);
  const monacoRef = useRef<typeof monacoEditor | null>(null);
  const decorationsRef = useRef<string[]>([]);
  const activeFileRef = useRef(activeFile);
  activeFileRef.current = activeFile;
  const dirInputRef = useRef<HTMLInputElement | null>(null);
  const projectFileInputRef = useRef<HTMLInputElement | null>(null);
  const [paneSizes, setPaneSizes] = useState<PaneSizes>(loadPaneSizes);
  const [errorMarker, setErrorMarker] = useState<ErrorMarker | null>(null);
  const projectRef = useRef(project);
  projectRef.current = project;

  const fileNames = useMemo(() => Object.keys(project.files).sort(), [project.files]);
  const isTerminated = stopEvent ? stopEvent.reason === "terminated" || stopEvent.reason === "exception" : false;
  const debugSessionRef = useRef(debugSession);
  debugSessionRef.current = debugSession;
  const debugBusyRef = useRef(debugBusy);
  debugBusyRef.current = debugBusy;
  const isTerminatedRef = useRef(isTerminated);
  isTerminatedRef.current = isTerminated;

  useEffect(() => {
    const worker = new Worker(new URL("./lua-worker.ts", import.meta.url), {
      type: "module",
    });
    worker.onmessage = (event: MessageEvent<WorkerEvent>) => {
      const message = event.data;
      if (message.type === "ready") {
        setStatus("ready");
      } else if (message.type === "result") {
        setOutput(message.output);
        setError(message.error);
        setErrorMarker(
          message.errorSource && message.errorLine != null
            ? { source: message.errorSource, line: message.errorLine, message: message.error ?? "Runtime error" }
            : null,
        );
        setStatus("ready");
      }
    };
    workerRef.current = worker;
    return () => worker.terminate();
  }, []);

  useEffect(() => {
    saveProject(project);
  }, [project]);

  useEffect(() => {
    try {
      localStorage.setItem(PANE_SIZES_KEY, JSON.stringify(paneSizes));
    } catch {
      // best-effort, same as saveProject - a private-mode/quota failure here
      // just means the layout resets to defaults next load.
    }
  }, [paneSizes]);

  /**
   * Drags one pane's size (`key`) along `axis` between `min`/`max`, starting
   * from the pointer position at mousedown. `direction` accounts for which
   * side of the handle the resized pane is on: +1 when growing the pane
   * means moving the handle away from the coordinate origin (down/right),
   * -1 when it means moving toward it (e.g. the side-panel, which is *left*
   * of the coordinate the mouse moves right into).
   */
  const startPaneResize = (
    axis: "x" | "y",
    key: keyof PaneSizes,
    direction: 1 | -1,
    min: number,
    max: number,
  ) => (e: ReactMouseEvent) => {
    e.preventDefault();
    const start = axis === "x" ? e.clientX : e.clientY;
    const startSize = paneSizes[key];
    const onMove = (ev: globalThis.MouseEvent) => {
      const current = axis === "x" ? ev.clientX : ev.clientY;
      const next = Math.min(max, Math.max(min, startSize + direction * (current - start)));
      setPaneSizes(p => ({...p, [key]: next}));
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      window.removeEventListener("blur", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    // If the mouse button is released outside the browser window (or the
    // window loses focus mid-drag, e.g. alt-tab), no "mouseup" ever reaches
    // `document` - without this, the drag listeners and the resize cursor
    // would stay stuck on indefinitely.
    window.addEventListener("blur", onUp);
    document.body.style.cursor = axis === "x" ? "col-resize" : "row-resize";
    document.body.style.userSelect = "none";
  };

  const run = () => {
    if (!workerRef.current || status === "loading" || debugSession) return;
    setStatus("running");
    setAnalysis(null);
    setOutput("");
    setError(null);
    setErrorMarker(null);
    workerRef.current.postMessage({type: "run", files: project.files, entry: project.entry});
  };

  // ---- Phase 8: profiler / execution timeline ----

  const runProfileClick = async () => {
    if (!workerRef.current || status !== "ready" || debugSession || analysisBusy) return;
    setAnalysisBusy(true);
    try {
      const stats = await runProfile(workerRef.current, project.files, project.entry);
      setAnalysis({type: "profile", stats});
    } finally {
      setAnalysisBusy(false);
    }
  };

  const runTimelineClick = async () => {
    if (!workerRef.current || status !== "ready" || debugSession || analysisBusy) return;
    setAnalysisBusy(true);
    try {
      const timeline = await runTimeline(workerRef.current, project.files, project.entry, TIMELINE_MAX_EVENTS);
      setAnalysis({type: "timeline", events: timeline.events, truncated: timeline.truncated});
    } finally {
      setAnalysisBusy(false);
    }
  };

  // ---- Debugger controls ----

  /**
   * Fetches the stack trace for whichever thread actually hit the
   * stop - a coroutine, if the stop happened inside one (Phase 8) - and,
   * if the top frame is a known project file, switches to it. Uses
   * `fileNameForSourceId` (not a raw `project.files[top.source]` lookup)
   * since `top.source` is a `source_id`: for a `require()`d file that's the
   * bare require argument, not its `.lua` filename - a raw lookup silently
   * failed to follow execution into a required file (found while wiring up
   * inline diagnostics, which needed the same source_id -> filename mapping
   * for a debug session's exception marker).
   */
  const focusStoppedFrame = async (session: DebugSession) => {
    const threads = await session.getThreads();
    const runningThread = threads.find(t => t.status === "running")?.id ?? 0;
    const frames = await session.getStackTrace(runningThread);
    const top = frames.find(f => f.functionType !== "c");
    const fileName = top?.source && fileNameForSourceId(top.source, project.files);
    if (fileName) setActiveFile(fileName);
  };

  const appendDebugOutput = async (session: DebugSession) => {
    const text = await session.takeOutput();
    if (text) setOutput(o => o + text);
  };

  /**
   * Shared post-stop handling for `startDebugging`/`doDebugAction`: records
   * output/state, and either places an inline-diagnostics marker at the
   * failing frame (an exception) or follows execution to wherever it's now
   * paused (anything else, `terminated` included - `focusStoppedFrame` is a
   * no-op there since there's no frame to follow).
   */
  const handleStopEvent = async (session: DebugSession, stop: StopEvent) => {
    await appendDebugOutput(session);
    setStopEvent(stop);
    if (stop.reason === "exception") {
      setErrorMarker(null);
      const threads = await session.getThreads();
      const runningThread = threads.find(t => t.status === "running")?.id ?? 0;
      const frames = await session.getStackTrace(runningThread);
      const top = frames.find(f => f.functionType !== "c");
      const fileName = top?.source && fileNameForSourceId(top.source, project.files);
      if (fileName && stop.line != null) {
        setErrorMarker({ source: fileName, line: stop.line, message: stop.message ?? "Runtime error" });
        setActiveFile(fileName);
      }
    } else {
      setErrorMarker(null);
      if (stop.reason !== "terminated") {
        await focusStoppedFrame(session);
      }
    }
  };

  const startDebugging = async () => {
    if (!workerRef.current || status !== "ready" || debugSession) return;
    setDebugBusy(true);
    setAnalysis(null);
    setOutput("");
    setError(null);
    setErrorMarker(null);
    const session = new DebugSession(workerRef.current);
    await session.launch(project.files, project.entry);

    const ids: Record<string, Record<number, number>> = {};
    for (const [file, lines] of Object.entries(breakpoints)) {
      const sourceId = sourceIdFor(file, project.entry);
      ids[file] = {};
      for (const line of lines) {
        const bp = await session.setBreakpoint(sourceId, line);
        ids[file][line] = bp.id;
      }
    }
    breakpointIdsRef.current = ids;
    setDebugSession(session);
    // Lets DebugPanel (and its Pause button) render immediately, instead of
    // only after this first continue() resolves - see LAUNCHING_STOP_EVENT's
    // doc comment for why that matters.
    setStopEvent(LAUNCHING_STOP_EVENT);

    try {
      const stop = await session.continue();
      await handleStopEvent(session, stop);
    } finally {
      setDebugBusy(false);
    }
  };

  /**
   * `continue()` now runs in bursts (see debug-session.ts), so it can be
   * in flight for a while - long enough that clicking Stop mid-`continue()`
   * is a realistic, intended interaction, not just a rare race. Stopping
   * disposes the session, which rejects this action's in-flight `send()`
   * call; without the try/finally, that rejection would skip
   * `setDebugBusy(false)` and leave every debug action permanently
   * unusable (the busy guard never clearing) even after starting a fresh
   * session.
   */
  const doDebugAction = async (action: (s: DebugSession) => Promise<StopEvent>) => {
    if (!debugSession || debugBusy) return;
    setDebugBusy(true);
    try {
      const stop = await action(debugSession);
      await handleStopEvent(debugSession, stop);
    } catch {
      // Most likely the session was disposed (Stop clicked) while this
      // action was in flight - stopDebugging() already reset everything
      // that matters; nothing else to do with a stale action's rejection.
    } finally {
      setDebugBusy(false);
    }
  };

  /**
   * Bypasses the `debugBusy` guard `doDebugAction` uses - `pause()` is only
   * useful *while* a `continue()` burst loop has that flag set, so routing
   * it through `doDebugAction` would always no-op. See `DebugSession.pause`.
   */
  const pauseDebugging = () => {
    void debugSession?.pause();
  };

  const stopDebugging = () => {
    debugSession?.dispose();
    setDebugSession(null);
    setStopEvent(null);
    breakpointIdsRef.current = {};
  };

  // Stable identity, not `() => {}` inline in JSX: DebugPanel's main effect
  // depends on this prop, so a fresh closure every App render (e.g. from
  // dragging a resize handle or toggling a breakpoint while paused, both of
  // which update unrelated App state) would spuriously re-run it - resetting
  // the selected frame and wiping every watch's evaluated value without
  // recomputing them (confirmed live: a watch showing `2` reverted to `…`
  // and stuck there after a resize-handle drag).
  const handleFrameSelected = useCallback(() => {}, []);

  /** Toggles a breakpoint from the gutter, keeping a live session in sync. */
  const toggleBreakpoint = async (file: string, line: number) => {
    const has = (breakpoints[file] ?? []).includes(line);
    if (has) {
      setBreakpoints(b => ({...b, [file]: (b[file] ?? []).filter(l => l !== line)}));
      const id = breakpointIdsRef.current[file]?.[line];
      if (id !== undefined && debugSession) {
        await debugSession.removeBreakpoint(id);
        delete breakpointIdsRef.current[file][line];
      }
    } else {
      setBreakpoints(b => ({
        ...b,
        [file]: [...(b[file] ?? []), line].sort((a, c) => a - c),
      }));
      if (debugSession) {
        const bp = await debugSession.setBreakpoint(sourceIdFor(file, project.entry), line);
        breakpointIdsRef.current[file] = {...(breakpointIdsRef.current[file] ?? {}), [line]: bp.id};
      }
    }
  };

  // Keyboard shortcuts (F5 run/continue, F9 toggle breakpoint, F10 step
  // over) - a mount-once window listener reading everything it needs
  // through refs, rather than a `run`/`doDebugAction`/`toggleBreakpoint`
  // dependency array: those three are redefined every render (closing over
  // that render's `project`/`debugSession`/etc.), so re-registering the
  // listener on every relevant change would work too, but a stale-closure
  // effect bug earlier in this project (DebugPanel's `onFrameSelected`) came
  // from exactly this class of mistake - refs updated inline (matching
  // `activeFileRef`'s existing pattern) sidestep it entirely.
  const runRef = useRef(run);
  runRef.current = run;
  const startDebuggingRef = useRef(startDebugging);
  startDebuggingRef.current = startDebugging;
  const doDebugActionRef = useRef(doDebugAction);
  doDebugActionRef.current = doDebugAction;
  const toggleBreakpointRef = useRef(toggleBreakpoint);
  toggleBreakpointRef.current = toggleBreakpoint;
  const hasBreakpointsRef = useRef(false);
  hasBreakpointsRef.current = Object.values(breakpoints).some(lines => lines.length > 0);

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
  }, []);

  const handleEditorMount: OnMount = (editor, monacoInstance) => {
    editorRef.current = editor;
    monacoRef.current = monacoInstance;
    editor.onMouseDown(e => {
      if (e.target.type === monacoInstance.editor.MouseTargetType.GUTTER_GLYPH_MARGIN && e.target.position) {
        void toggleBreakpoint(activeFileRef.current, e.target.position.lineNumber);
      }
    });
  };

  // Breakpoint dots + current-line highlight for the active file.
  useEffect(() => {
    const editor = editorRef.current;
    const monacoInstance = monacoRef.current;
    if (!editor || !monacoInstance) return;
    const decorations: monacoEditor.editor.IModelDeltaDecoration[] = [];
    for (const line of breakpoints[activeFile] ?? []) {
      decorations.push({
        range: new monacoInstance.Range(line, 1, line, 1),
        options: {glyphMarginClassName: "breakpoint-glyph"},
      });
    }
    if (stopEvent && !isTerminated && stopEvent.line != null) {
      decorations.push({
        range: new monacoInstance.Range(stopEvent.line, 1, stopEvent.line, 1),
        options: {
          isWholeLine: true,
          className: "current-line-highlight",
          glyphMarginClassName: "current-line-glyph",
        },
      });
    }
    decorationsRef.current = editor.deltaDecorations(decorationsRef.current, decorations);
  }, [breakpoints, activeFile, stopEvent, isTerminated]);

  // Inline error diagnostics: a Monaco marker (red squiggle) on whichever
  // line/file `errorMarker` names - from a plain Run's `ExecuteResult` or a
  // debug session's exception stop (see `handleStopEvent`). Reads
  // `projectRef` rather than depending on `project` directly: `project`
  // changes on every keystroke (a new `files` object each time), and this
  // only needs to run when the marker itself changes, not on every edit.
  useEffect(() => {
    const monacoInstance = monacoRef.current;
    if (!monacoInstance) return;
    const currentProject = projectRef.current;
    for (const fileName of Object.keys(currentProject.files)) {
      const model = monacoInstance.editor.getModel(monacoInstance.Uri.parse(fileName));
      if (model) monacoInstance.editor.setModelMarkers(model, "lua-runtime", []);
    }
    if (!errorMarker) return;
    const fileName = fileNameForSourceId(errorMarker.source, currentProject.files);
    const model = fileName && monacoInstance.editor.getModel(monacoInstance.Uri.parse(fileName));
    if (!model) return;
    const line = Math.min(Math.max(errorMarker.line, 1), model.getLineCount());
    monacoInstance.editor.setModelMarkers(model, "lua-runtime", [
      {
        startLineNumber: line,
        startColumn: 1,
        endLineNumber: line,
        endColumn: model.getLineMaxColumn(line),
        message: errorMarker.message,
        severity: monacoInstance.MarkerSeverity.Error,
      },
    ]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [errorMarker]);

  const updateActiveFileContent = (content: string) => {
    setProject(p => ({...p, files: {...p.files, [activeFile]: content}}));
  };

  const addFile = () => {
    const name = window.prompt("New file name (e.g. utils.lua):");
    if (!name) return;
    if (!isValidFileName(name)) {
      window.alert("File names must look like 'name.lua' (letters, digits, _ or -).");
      return;
    }
    if (project.files[name] !== undefined) {
      window.alert(`'${name}' already exists.`);
      return;
    }
    setProject(p => ({...p, files: {...p.files, [name]: ""}}));
    setActiveFile(name);
  };

  const renameActiveFile = () => {
    const name = window.prompt("Rename file to:", activeFile);
    if (!name || name === activeFile) return;
    if (!isValidFileName(name)) {
      window.alert("File names must look like 'name.lua' (letters, digits, _ or -).");
      return;
    }
    if (project.files[name] !== undefined) {
      window.alert(`'${name}' already exists.`);
      return;
    }
    setProject(p => {
      const files = {...p.files};
      files[name] = files[activeFile];
      delete files[activeFile];
      return {files, entry: p.entry === activeFile ? name : p.entry};
    });
    setActiveFile(name);
  };

  const deleteFile = (name: string) => {
    if (fileNames.length <= 1) {
      window.alert("A project needs at least one file.");
      return;
    }
    if (!window.confirm(`Delete '${name}'?`)) return;
    setProject(p => {
      const files = {...p.files};
      delete files[name];
      const entry = p.entry === name ? Object.keys(files)[0] : p.entry;
      return {files, entry};
    });
    if (activeFile === name) {
      setActiveFile(current => {
        const remaining = fileNames.filter(f => f !== current);
        return remaining[0];
      });
    }
  };

  const setEntry = (name: string) => {
    setProject(p => ({...p, entry: name}));
  };

  const importDirectoryClick = () => dirInputRef.current?.click();

  /**
   * Imports every `.lua` file under a locally-picked directory into the
   * (flat, string-keyed) virtual FS - see `isValidFileName`'s doc comment.
   * The top-level folder name itself is dropped from each path so imported
   * files land at the project root (`lib/utils.lua`, not
   * `my-project/lib/utils.lua`); files whose path doesn't fit the
   * `[A-Za-z0-9_-]` charset are skipped rather than silently mangled.
   */
  const handleImportDirChange = async (e: ChangeEvent<HTMLInputElement>) => {
    const input = e.target;
    // `input.files` is live - snapshot it into a plain array *before*
    // resetting `input.value` below (needed so re-importing the same
    // directory later still fires a "change" event), since clearing
    // `.value` also clears the underlying FileList out from under any
    // reference still pointing at it.
    const luaFiles = Array.from(input.files ?? []).filter(f => f.name.endsWith(".lua"));
    input.value = "";
    const entries = await Promise.all(
      luaFiles.map(async f => {
        const relativePath = (f as File & {webkitRelativePath?: string}).webkitRelativePath || f.name;
        const path = relativePath.split("/").slice(1).join("/") || f.name;
        return [path, await f.text()] as const;
      })
    );

    if (entries.length === 0) {
      window.alert("No .lua files found in that directory.");
      return;
    }

    const valid = entries.filter(([path]) => isValidFileName(path));
    const invalid = entries.filter(([path]) => !isValidFileName(path));
    if (invalid.length > 0) {
      window.alert(
        `Skipped ${invalid.length} file(s) with unsupported names (letters, digits, _, - and / only):\n${invalid
          .map(([p]) => p)
          .join("\n")}`
      );
    }
    if (valid.length === 0) return;

    setProject(p => ({...p, files: {...p.files, ...Object.fromEntries(valid)}}));
    setActiveFile(valid[0][0]);
  };

  const exportProjectClick = () => downloadProject(project);

  const importProjectClick = () => projectFileInputRef.current?.click();

  /** Replaces the current project outright (unlike directory import, which merges .lua files in). */
  const handleImportProjectChange = async (e: ChangeEvent<HTMLInputElement>) => {
    const input = e.target;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;

    const parsed = parseProjectFile(await file.text());
    if (!parsed) {
      window.alert("That file isn't a valid Lua Playground project export.");
      return;
    }
    setProject(parsed);
    setActiveFile(Object.keys(parsed.files)[0]);
    setBreakpoints({});
    breakpointIdsRef.current = {};
    setOutput("");
    setError(null);
    setErrorMarker(null);
    setAnalysis(null);
  };

  return (
    <div className="playground">
      <header>
        <h1>Lua Playground</h1>
        <div className="header-actions">
          <button
            type="button"
            className="run-button"
            onClick={run}
            disabled={status !== "ready" || !!debugSession}
            title="Run (F5)"
          >
            {status === "ready" ? "▶" : "⏳"} {runButtonLabel(status)}
          </button>
          {!debugSession && (
            <button type="button" className="debug-button" onClick={startDebugging} disabled={status !== "ready"}>
              🐞 Debug
            </button>
          )}
          {!debugSession && (
            <button
              type="button"
              className="analysis-button"
              onClick={runProfileClick}
              disabled={status !== "ready" || analysisBusy}
              title="Phase 8: profile calls/instructions per function">
              📊 Profile
            </button>
          )}
          {!debugSession && (
            <button
              type="button"
              className="analysis-button"
              onClick={runTimelineClick}
              disabled={status !== "ready" || analysisBusy}
              title="Phase 8: record a capped execution timeline">
              ⏱ Timeline
            </button>
          )}
        </div>
      </header>
      <main>
        <aside className="file-tree" style={{width: paneSizes.fileTreeWidth}}>
          <div className="file-tree-header">
            <h2>Files</h2>
            <div className="file-tree-header-actions">
              <button
                type="button"
                className="icon-button"
                onClick={exportProjectClick}
                title="Download this project as a .json file">
                ⬇
              </button>
              <button
                type="button"
                className="icon-button"
                onClick={importProjectClick}
                title="Load a project .json file (replaces the current project)">
                ⬆
              </button>
              <button
                type="button"
                className="icon-button"
                onClick={importDirectoryClick}
                title="Import a directory of .lua files">
                📁
              </button>
              <button type="button" className="icon-button" onClick={addFile} title="New file">
                +
              </button>
            </div>
            <input
              ref={dirInputRef}
              type="file"
              className="visually-hidden"
              multiple
              // @ts-expect-error non-standard attributes (Chrome/Firefox/Edge); no directory picker without them
              webkitdirectory=""
              directory=""
              onChange={handleImportDirChange}
            />
            <input
              ref={projectFileInputRef}
              type="file"
              className="visually-hidden"
              accept="application/json,.json"
              onChange={handleImportProjectChange}
            />
          </div>
          <ul>
            {fileNames.map(name => (
              <li key={name} className={name === activeFile ? "active" : ""}>
                <button type="button" className="file-name" onClick={() => setActiveFile(name)}>
                  {name}
                </button>
                <button
                  type="button"
                  className={`entry-badge ${name === project.entry ? "is-entry" : ""}`}
                  onClick={() => setEntry(name)}
                  title={name === project.entry ? "Entry file" : "Set as entry file"}>
                  ▶
                </button>
                <button type="button" className="icon-button" onClick={() => deleteFile(name)} title="Delete file">
                  ×
                </button>
              </li>
            ))}
          </ul>
        </aside>
        <div
          className="resize-handle resize-handle-v"
          onMouseDown={startPaneResize("x", "fileTreeWidth", 1, 140, 480)}
        />
        <div className="center-pane">
          <section className="editor-pane">
            <div className="editor-toolbar">
              <span className="active-file-name">
                {activeFile}
                {activeFile === project.entry ? " (entry)" : ""}
              </span>
              <button type="button" className="icon-button" onClick={renameActiveFile} title="Rename file">
                rename
              </button>
            </div>
            <Editor
              height="100%"
              language="lua"
              theme="vs-dark"
              path={activeFile}
              value={project.files[activeFile] ?? ""}
              onChange={value => updateActiveFileContent(value ?? "")}
              onMount={handleEditorMount}
              options={{
                minimap: {enabled: false},
                fontSize: 14,
                automaticLayout: true,
                glyphMargin: true,
                readOnly: !!debugSession,
              }}
            />
          </section>
          <div
            className="resize-handle resize-handle-h"
            onMouseDown={startPaneResize("y", "consoleHeight", -1, 80, 560)}
          />
          <section className="console-pane" style={{height: paneSizes.consoleHeight}}>
            <h2>Console</h2>
            <pre className={error ? "console-error" : "console-output"}>{error ?? (output || "(no output yet)")}</pre>
          </section>
        </div>
        <div
          className="resize-handle resize-handle-v"
          onMouseDown={startPaneResize("x", "sidePanelWidth", -1, 220, 640)}
        />
        <aside className="side-panel" style={{width: paneSizes.sidePanelWidth}}>
          {debugSession && stopEvent ? (
            <DebugPanel
              session={debugSession}
              stop={stopEvent}
              isTerminated={isTerminated}
              busy={debugBusy}
              onContinue={() => doDebugAction(s => s.continue())}
              onPause={pauseDebugging}
              onStepOver={() => doDebugAction(s => s.stepOver())}
              onStepInto={() => doDebugAction(s => s.stepInto())}
              onStepOut={() => doDebugAction(s => s.stepOut())}
              onStop={stopDebugging}
              onFrameSelected={handleFrameSelected}
            />
          ) : analysis?.type === "profile" ? (
            <ProfilerPanel stats={analysis.stats} onClose={() => setAnalysis(null)} />
          ) : analysis?.type === "timeline" ? (
            <TimelinePanel events={analysis.events} truncated={analysis.truncated} onClose={() => setAnalysis(null)} />
          ) : (
            <div className="side-panel-placeholder">
              <p>Debug, Profile, or Timeline output shows up here.</p>
            </div>
          )}
        </aside>
      </main>
    </div>
  );
}

export default App;
