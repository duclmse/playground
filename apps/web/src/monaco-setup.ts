// Points @monaco-editor/react at the locally bundled `monaco-editor` package
// instead of its default behavior of fetching Monaco from a CDN at runtime.
// Keeps the playground self-contained and working offline, matching the
// rest of the app (WASM Lua runtime runs entirely in-browser, no backend).
import * as monaco from "monaco-editor";
import { loader } from "@monaco-editor/react";

// Without an explicit `getWorker`, Monaco falls back to instantiating its
// editorWorkerService worker from a `blob:` URL, whose relative ESM imports
// then fail to resolve (`Invalid relative url or base scheme isn't
// hierarchical` — a browser limitation, not a Monaco bug) — confirmed via a
// real Playwright run against both dev and production builds. Serving the
// worker file from its real, non-blob URL avoids that entirely.
self.MonacoEnvironment = {
  getWorker() {
    return new Worker(
      new URL("../../../node_modules/monaco-editor/esm/vs/editor/editor.worker.js", import.meta.url),
      { type: "module" },
    );
  },
};

loader.config({ monaco });
