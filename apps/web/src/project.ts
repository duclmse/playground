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

export function loadProject(): Project {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return defaultProject();
    const parsed = JSON.parse(raw) as Project;
    if (
      !parsed ||
      typeof parsed.entry !== "string" ||
      typeof parsed.files !== "object" ||
      Object.keys(parsed.files).length === 0
    ) {
      return defaultProject();
    }
    return parsed;
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
 * A bare file name (`utils.lua`) or a `/`-separated path within an imported
 * directory (`lib/utils.lua`) - the virtual FS is a flat string-keyed map
 * (see `install_require` in lib.rs), so a path is just a key that happens to
 * contain slashes; no directory entities actually exist.
 */
export function isValidFileName(name: string): boolean {
  return /^[A-Za-z0-9_-]+(\/[A-Za-z0-9_-]+)*\.lua$/.test(name);
}
