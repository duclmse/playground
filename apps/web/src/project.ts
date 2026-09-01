// Client-only virtual filesystem model backing multi-file projects and
// `require()` (product-brief.md: "Browser-only execution environment; no
// real filesystem - a virtual FS backs `require()`"). Persisted to
// localStorage only - no backend, no shareable links (risks.md §4).

export type Project = {
  files: Record<string, string>;
  entry: string;
};

const STORAGE_KEY = "lua-playground:project";

const DEFAULT_MAIN = `-- Real Lua, executed by piccolo (Rust) compiled to WebAssembly.
local greet = require("greet")

print(greet.hello("Lua Playground"))

local sum = 0
for i = 1, 10 do
  sum = sum + i
end
print("sum 1..10 =", sum)
`;

const DEFAULT_GREET = `local M = {}

function M.hello(name)
  return "Hello, " .. name .. "!"
end

return M
`;

export function defaultProject(): Project {
  return {
    files: {
      "main.lua": DEFAULT_MAIN,
      "greet.lua": DEFAULT_GREET,
    },
    entry: "main.lua",
  };
}

/** Structural check shared by `loadProject` (localStorage) and `parseProjectFile` (an imported file). */
function isProjectShaped(value: unknown): value is Project {
  const p = value as Partial<Project> | null;
  return (
    !!p &&
    typeof p.entry === "string" &&
    typeof p.files === "object" &&
    p.files !== null &&
    Object.keys(p.files).length > 0 &&
    Object.values(p.files).every(content => typeof content === "string")
  );
}

export function loadProject(): Project {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return defaultProject();
    const parsed: unknown = JSON.parse(raw);
    return isProjectShaped(parsed) ? parsed : defaultProject();
  } catch {
    return defaultProject();
  }
}

export function saveProject(project: Project) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(project));
  } catch {
    // localStorage unavailable (private mode, quota, etc.) - save is best
    // effort per risks.md §4's client-only persistence model; nothing else
    // in the app depends on it succeeding.
  }
}

/**
 * Downloads `project` as a single JSON file - the counterpart to importing
 * a project back with `parseProjectFile`. There's no backend/shareable-link
 * story here (risks.md §4), so this plus "import a directory" (App.tsx's
 * `handleImportDirChange`, which only pulls in `.lua` files, not the entry
 * pointer) are the two ways a project round-trips outside `localStorage`:
 * this one preserves `entry` and works for a full backup/restore; that one
 * is better for merging code from an existing folder on disk into the
 * *current* project.
 */
export function downloadProject(project: Project) {
  const blob = new Blob([JSON.stringify(project, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = url;
    a.download = "lua-playground-project.json";
    a.click();
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** Parses an imported project file's text; `null` if it isn't project-shaped JSON. */
export function parseProjectFile(text: string): Project | null {
  try {
    const parsed: unknown = JSON.parse(text);
    return isProjectShaped(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

/**
 * A bare file name (`utils.lua`) or a `/`-separated path within an imported
 * directory (`lib/utils.lua`) - the virtual FS is a flat string-keyed map
 * (see `install_require` in lib.rs), so a path is just a key that happens to
 * contain slashes; no directory entities actually exist.
 */
export function isValidFileName(name: string): boolean {
  return /^[A-Za-z0-9_-]+(\/[A-Za-z0-9_-]+)*\.lua$/.test(name);
}
