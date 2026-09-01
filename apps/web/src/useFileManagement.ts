// File-tree management: add/rename/delete a file, set the entry point, and
// the three ways a project's files can come from outside the browser
// (import a directory of .lua files, export/import the whole project as
// JSON) - split out of App.tsx, which was otherwise growing past ~850
// lines. Self-contained aside from `onProjectImported`, a callback for
// whatever App-level state (breakpoints, console output, analysis panel)
// should reset when `handleImportProjectChange` replaces the project
// outright - this hook only owns file/project state, not that.
import { useRef, type ChangeEvent, type Dispatch, type SetStateAction } from "react";
import {
  downloadProject,
  isValidFileName,
  parseProjectFile,
  type Project,
} from "./project";

export interface FileManagementOptions {
  project: Project;
  setProject: Dispatch<SetStateAction<Project>>;
  activeFile: string;
  setActiveFile: Dispatch<SetStateAction<string>>;
  fileNames: string[];
  onProjectImported: () => void;
}

export function useFileManagement({
  project,
  setProject,
  activeFile,
  setActiveFile,
  fileNames,
  onProjectImported,
}: FileManagementOptions) {
  const dirInputRef = useRef<HTMLInputElement | null>(null);
  const projectFileInputRef = useRef<HTMLInputElement | null>(null);

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
    setProject(p => ({ ...p, files: { ...p.files, [name]: "" } }));
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
    setProject(p => {
      const files = { ...p.files };
      delete files[name];
      const entry = p.entry === name ? Object.keys(files)[0] : p.entry;
      return { files, entry };
    });
    if (activeFile === name) {
      setActiveFile(current => {
        const remaining = fileNames.filter(f => f !== current);
        return remaining[0];
      });
    }
  };

  const setEntry = (name: string) => {
    setProject(p => ({ ...p, entry: name }));
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

    setProject(p => ({ ...p, files: { ...p.files, ...Object.fromEntries(valid) } }));
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
    onProjectImported();
  };

  return {
    dirInputRef,
    projectFileInputRef,
    addFile,
    renameActiveFile,
    deleteFile,
    setEntry,
    importDirectoryClick,
    handleImportDirChange,
    exportProjectClick,
    importProjectClick,
    handleImportProjectChange,
  };
}
