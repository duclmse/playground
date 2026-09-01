import Editor, { type OnMount } from "@monaco-editor/react";
import * as monacoEditor from "monaco-editor";
import { useEffect, useMemo, useRef, useState, type ChangeEvent } from "react";
import "./monaco-setup";
import type { WorkerEvent } from "./lua-worker";
import type { FunctionStatsInfo, TimelineEventInfo } from "./debug-protocol";
import { runProfile, runTimeline } from "./analysis";
import { DebugSession, type StopEvent } from "./debug-session";
import { DebugPanel } from "./DebugPanel";
import { ProfilerPanel } from "./ProfilerPanel";
import { TimelinePanel } from "./TimelinePanel";
import { isValidFileName, loadProject, saveProject, type Project } from "./project";
import "./App.css";

const TIMELINE_MAX_EVENTS = 5000;

type Analysis =
  | { type: "profile"; stats: FunctionStatsInfo[] }
  | { type: "timeline"; events: TimelineEventInfo[]; truncated: boolean };

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

function App() {
  const [project, setProject] = useState<Project>(() => loadProject());
  const [activeFile, setActiveFile] = useState<string>(
    () => Object.keys(loadProject().files)[0],
  );
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

  const fileNames = useMemo(() => Object.keys(project.files).sort(), [project.files]);
  const isTerminated = stopEvent
    ? stopEvent.reason === "terminated" || stopEvent.reason === "exception"
    : false;

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
        setStatus("ready");
      }
    };
    workerRef.current = worker;
    return () => worker.terminate();
  }, []);

  useEffect(() => {
    saveProject(project);
  }, [project]);

  const run = () => {
    if (!workerRef.current || status === "loading" || debugSession) return;
    setStatus("running");
    setAnalysis(null);
    setOutput("");
    setError(null);
    workerRef.current.postMessage({ type: "run", files: project.files, entry: project.entry });
  };

  // ---- Phase 8: profiler / execution timeline ----

  const runProfileClick = async () => {
    if (!workerRef.current || status !== "ready" || debugSession || analysisBusy) return;
    setAnalysisBusy(true);
    try {
      const stats = await runProfile(workerRef.current, project.files, project.entry);
      setAnalysis({ type: "profile", stats });
    } finally {
      setAnalysisBusy(false);
    }
  };

  const runTimelineClick = async () => {
    if (!workerRef.current || status !== "ready" || debugSession || analysisBusy) return;
    setAnalysisBusy(true);
    try {
      const timeline = await runTimeline(
        workerRef.current,
        project.files,
        project.entry,
        TIMELINE_MAX_EVENTS,
      );
      setAnalysis({ type: "timeline", events: timeline.events, truncated: timeline.truncated });
    } finally {
      setAnalysisBusy(false);
    }
  };

  // ---- Debugger controls ----

  /**
   * Fetches the stack trace for whichever thread actually hit the
   * stop - a coroutine, if the stop happened inside one (Phase 8) - and,
   * if the top frame is a known project file, switches to it.
   */
  const focusStoppedFrame = async (session: DebugSession) => {
    const threads = await session.getThreads();
    const runningThread = threads.find((t) => t.status === "running")?.id ?? 0;
    const frames = await session.getStackTrace(runningThread);
    const top = frames.find((f) => f.functionType !== "c");
    if (top?.source && project.files[top.source] !== undefined) {
      setActiveFile(top.source);
    }
  };

  const appendDebugOutput = async (session: DebugSession) => {
    const text = await session.takeOutput();
    if (text) setOutput((o) => o + text);
  };

  const startDebugging = async () => {
    if (!workerRef.current || status !== "ready" || debugSession) return;
    setDebugBusy(true);
    setAnalysis(null);
    setOutput("");
    setError(null);
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

    const stop = await session.continue();
    await appendDebugOutput(session);
    setStopEvent(stop);
    if (!(stop.reason === "terminated" || stop.reason === "exception")) {
      await focusStoppedFrame(session);
    }
    setDebugBusy(false);
  };

  const doDebugAction = async (action: (s: DebugSession) => Promise<StopEvent>) => {
    if (!debugSession || debugBusy) return;
    setDebugBusy(true);
    const stop = await action(debugSession);
    await appendDebugOutput(debugSession);
    setStopEvent(stop);
    if (!(stop.reason === "terminated" || stop.reason === "exception")) {
      await focusStoppedFrame(debugSession);
    }
    setDebugBusy(false);
  };

  const stopDebugging = () => {
    debugSession?.dispose();
    setDebugSession(null);
    setStopEvent(null);
    breakpointIdsRef.current = {};
  };

  /** Toggles a breakpoint from the gutter, keeping a live session in sync. */
  const toggleBreakpoint = async (file: string, line: number) => {
    const has = (breakpoints[file] ?? []).includes(line);
    if (has) {
      setBreakpoints((b) => ({ ...b, [file]: (b[file] ?? []).filter((l) => l !== line) }));
      const id = breakpointIdsRef.current[file]?.[line];
      if (id !== undefined && debugSession) {
        await debugSession.removeBreakpoint(id);
        delete breakpointIdsRef.current[file][line];
      }
    } else {
      setBreakpoints((b) => ({
        ...b,
        [file]: [...(b[file] ?? []), line].sort((a, c) => a - c),
      }));
      if (debugSession) {
        const bp = await debugSession.setBreakpoint(sourceIdFor(file, project.entry), line);
        breakpointIdsRef.current[file] = { ...(breakpointIdsRef.current[file] ?? {}), [line]: bp.id };
      }
    }
  };

  const handleEditorMount: OnMount = (editor, monacoInstance) => {
    editorRef.current = editor;
    monacoRef.current = monacoInstance;
    editor.onMouseDown((e) => {
      if (
        e.target.type === monacoInstance.editor.MouseTargetType.GUTTER_GLYPH_MARGIN &&
        e.target.position
      ) {
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
        options: { glyphMarginClassName: "breakpoint-glyph" },
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

  const updateActiveFileContent = (content: string) => {
    setProject((p) => ({ ...p, files: { ...p.files, [activeFile]: content } }));
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
    setProject((p) => ({ ...p, files: { ...p.files, [name]: "" } }));
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
    setProject((p) => {
      const files = { ...p.files };
      files[name] = files[activeFile];
      delete files[activeFile];
      return { files, entry: p.entry === activeFile ? name : p.entry };
    });
    setActiveFile(name);
  };

  const deleteFile = (name: string) => {
    if (fileNames.length <= 1) {
      window.alert("A project needs at least one file.");
      return;
    }
    if (!window.confirm(`Delete '${name}'?`)) return;
    setProject((p) => {
      const files = { ...p.files };
      delete files[name];
      const entry = p.entry === name ? Object.keys(files)[0] : p.entry;
      return { files, entry };
    });
    if (activeFile === name) {
      setActiveFile((current) => {
        const remaining = fileNames.filter((f) => f !== current);
        return remaining[0];
      });
    }
  };

  const setEntry = (name: string) => {
    setProject((p) => ({ ...p, entry: name }));
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
    const luaFiles = Array.from(input.files ?? []).filter((f) => f.name.endsWith(".lua"));
    input.value = "";
    const entries = await Promise.all(
      luaFiles.map(async (f) => {
        const relativePath = (f as File & { webkitRelativePath?: string }).webkitRelativePath || f.name;
        const path = relativePath.split("/").slice(1).join("/") || f.name;
        return [path, await f.text()] as const;
      }),
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
          .join("\n")}`,
      );
    }
    if (valid.length === 0) return;

    setProject((p) => ({ ...p, files: { ...p.files, ...Object.fromEntries(valid) } }));
    setActiveFile(valid[0][0]);
  };

  return (
    <div className="playground">
      <header>
        <h1>Lua Playground</h1>
        <span className="subtitle">Rust + piccolo, compiled to WebAssembly</span>
        <div className="header-actions">
          <button
            type="button"
            className="run-button"
            onClick={run}
            disabled={status !== "ready" || !!debugSession}
          >
            {status === "ready" ? "▶" : "⏳"} {runButtonLabel(status)}
          </button>
          {!debugSession && (
            <button
              type="button"
              className="debug-button"
              onClick={startDebugging}
              disabled={status !== "ready"}
            >
              🐞 Debug
            </button>
          )}
          {!debugSession && (
            <button
              type="button"
              className="analysis-button"
              onClick={runProfileClick}
              disabled={status !== "ready" || analysisBusy}
              title="Phase 8: profile calls/instructions per function"
            >
              📊 Profile
            </button>
          )}
          {!debugSession && (
            <button
              type="button"
              className="analysis-button"
              onClick={runTimelineClick}
              disabled={status !== "ready" || analysisBusy}
              title="Phase 8: record a capped execution timeline"
            >
              ⏱ Timeline
            </button>
          )}
        </div>
      </header>
      <main>
        <aside className="file-tree">
          <div className="file-tree-header">
            <h2>Files</h2>
            <div className="file-tree-header-actions">
              <button
                type="button"
                className="icon-button"
                onClick={importDirectoryClick}
                title="Import a directory of .lua files"
              >
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
          </div>
          <ul>
            {fileNames.map((name) => (
              <li key={name} className={name === activeFile ? "active" : ""}>
                <button type="button" className="file-name" onClick={() => setActiveFile(name)}>
                  {name}
                </button>
                <button
                  type="button"
                  className={`entry-badge ${name === project.entry ? "is-entry" : ""}`}
                  onClick={() => setEntry(name)}
                  title={name === project.entry ? "Entry file" : "Set as entry file"}
                >
                  ▶
                </button>
                <button
                  type="button"
                  className="icon-button"
                  onClick={() => deleteFile(name)}
                  title="Delete file"
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        </aside>
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
              onChange={(value) => updateActiveFileContent(value ?? "")}
              onMount={handleEditorMount}
              options={{
                minimap: { enabled: false },
                fontSize: 14,
                automaticLayout: true,
                glyphMargin: true,
                readOnly: !!debugSession,
              }}
            />
          </section>
          <section className="console-pane">
            <h2>Console</h2>
            <pre className={error ? "console-error" : "console-output"}>
              {error ?? (output || "(no output yet)")}
            </pre>
          </section>
        </div>
        <aside className="side-panel">
          {debugSession && stopEvent ? (
            <DebugPanel
              session={debugSession}
              stop={stopEvent}
              isTerminated={isTerminated}
              onContinue={() => doDebugAction((s) => s.continue())}
              onStepOver={() => doDebugAction((s) => s.stepOver())}
              onStepInto={() => doDebugAction((s) => s.stepInto())}
              onStepOut={() => doDebugAction((s) => s.stepOut())}
              onStop={stopDebugging}
              onFrameSelected={() => {}}
            />
          ) : analysis?.type === "profile" ? (
            <ProfilerPanel stats={analysis.stats} onClose={() => setAnalysis(null)} />
          ) : analysis?.type === "timeline" ? (
            <TimelinePanel
              events={analysis.events}
              truncated={analysis.truncated}
              onClose={() => setAnalysis(null)}
            />
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
