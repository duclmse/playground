import Editor from "@monaco-editor/react";
import { useEffect, useMemo, useRef, useState } from "react";
import "./monaco-setup";
import type { WorkerEvent } from "./lua-worker";
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

function App() {
  const [project, setProject] = useState<Project>(() => loadProject());
  const [activeFile, setActiveFile] = useState<string>(
    () => Object.keys(loadProject().files)[0],
  );
  const [output, setOutput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<Status>("loading");
  const workerRef = useRef<Worker | null>(null);

  const fileNames = useMemo(() => Object.keys(project.files).sort(), [project.files]);

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
    if (!workerRef.current || status === "loading") return;
    setStatus("running");
    setOutput("");
    setError(null);
    workerRef.current.postMessage({ type: "run", files: project.files, entry: project.entry });
  };

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
            options={{
              minimap: { enabled: false },
              fontSize: 14,
              automaticLayout: true,
            }}
          />
          <button type="button" onClick={run} disabled={status !== "ready"}>
            {runButtonLabel(status)}
          </button>
        </section>
        <section className="console-pane">
          <h2>Console</h2>
          <pre className={error ? "console-error" : "console-output"}>
            {error ?? (output || "(no output yet)")}
          </pre>
        </section>
      </main>
    </div>
  );
}

export default App;
