// Resizable-pane support for the playground's three draggable splits
// (file-tree/editor, editor/console, center/side-panel) - split out of
// App.tsx, which was otherwise growing past ~850 lines. Self-contained:
// owns its own state, persistence, and drag-handling, and hands back only
// `paneSizes` (to size the panes) and `startPaneResize` (an onMouseDown
// factory for each resize handle).
import { useEffect, useState, type MouseEvent as ReactMouseEvent } from "react";

export type PaneSizes = {
  fileTreeWidth: number;
  consoleHeight: number;
  sidePanelWidth: number;
};

const PANE_SIZES_KEY = "lua-playground:paneSizes";
const DEFAULT_PANE_SIZES: PaneSizes = { fileTreeWidth: 180, consoleHeight: 220, sidePanelWidth: 320 };

function loadPaneSizes(): PaneSizes {
  try {
    const raw = localStorage.getItem(PANE_SIZES_KEY);
    if (!raw) return DEFAULT_PANE_SIZES;
    return { ...DEFAULT_PANE_SIZES, ...JSON.parse(raw) };
  } catch {
    return DEFAULT_PANE_SIZES;
  }
}

export function usePaneResize() {
  const [paneSizes, setPaneSizes] = useState<PaneSizes>(loadPaneSizes);

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
  const startPaneResize =
    (axis: "x" | "y", key: keyof PaneSizes, direction: 1 | -1, min: number, max: number) =>
    (e: ReactMouseEvent) => {
      e.preventDefault();
      const start = axis === "x" ? e.clientX : e.clientY;
      const startSize = paneSizes[key];
      const onMove = (ev: globalThis.MouseEvent) => {
        const current = axis === "x" ? ev.clientX : ev.clientY;
        const next = Math.min(max, Math.max(min, startSize + direction * (current - start)));
        setPaneSizes(p => ({ ...p, [key]: next }));
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

  return { paneSizes, startPaneResize };
}
