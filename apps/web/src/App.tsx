import Editor, { type OnMount } from "@monaco-editor/react";
import * as monacoEditor from "monaco-editor";
import { useEffect, useMemo, useRef, useState } from "react";
import "./monaco-setup";
import type { WorkerEvent } from "./lua-worker";
import { DebugSession, type StopEvent } from "./debug-session";
import { DebugPanel } from "./DebugPanel";
import { isValidFileName, loadProject, saveProject, type Project } from "./project";
import "./App.css";

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
  const editorRef = useRef<monacoEditor.editor.IStandaloneCodeEditor | null>(null);
  const monacoRef = useRef<typeof monacoEditor | null>(null);
  const decorationsRef = useRef<string[]>([]);
  const activeFileRef = useRef(activeFile);
  activeFileRef.current = activeFile;

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
    setOutput("");
    setError(null);
    workerRef.current.postMessage({ type: "run", files: project.files, entry: project.entry });
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

  return (
    <div className="playground">
      <header>
        <h1>Lua Playground</h1>
        <span className="subtitle">Rust + piccolo, compiled to WebAssembly</span>
      </header>
      <main>
        <aside className="file-tree">
          <div className="file-tree-header">
            <h2>Files</h2>
            <button type="button" className="icon-button" onClick={addFile} title="New file">
              +
            </button>
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
          <div className="editor-actions">
            <button type="button" onClick={run} disabled={status !== "ready" || !!debugSession}>
              {runButtonLabel(status)}
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
          </div>
        </section>
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
        ) : (
          <section className="console-pane">
            <h2>Console</h2>
            <pre className={error ? "console-error" : "console-output"}>
              {error ?? (output || "(no output yet)")}
            </pre>
          </section>
        )}
      </main>
    </div>
  );
}

export default App;
