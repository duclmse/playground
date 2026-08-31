import { useEffect, useRef, useState } from "react";
import type { WorkerEvent } from "./lua-worker";
import "./App.css";

const DEFAULT_SOURCE = `-- Real Lua, executed by piccolo (Rust) compiled to WebAssembly.
local function greet(name)
  return "Hello, " .. name .. "!"
end

print(greet("Lua Playground"))

local sum = 0
for i = 1, 10 do
  sum = sum + i
end
print("sum 1..10 =", sum)
`;

type Status = "loading" | "ready" | "running";

function App() {
  const [source, setSource] = useState(DEFAULT_SOURCE);
  const [output, setOutput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<Status>("loading");
  const workerRef = useRef<Worker | null>(null);

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

  const run = () => {
    if (!workerRef.current || status === "loading") return;
    setStatus("running");
    setOutput("");
    setError(null);
    workerRef.current.postMessage({ type: "run", source });
  };

  return (
    <div className="playground">
      <header>
        <h1>Lua Playground</h1>
        <span className="subtitle">Rust + piccolo, compiled to WebAssembly</span>
      </header>
      <main>
        <section className="editor-pane">
          <textarea
            spellCheck={false}
            value={source}
            onChange={(e) => setSource(e.target.value)}
          />
          <button onClick={run} disabled={status !== "ready"}>
            {status === "loading" ? "Loading VM…" : status === "running" ? "Running…" : "Run"}
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
