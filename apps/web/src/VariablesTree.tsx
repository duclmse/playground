// Lazy-expanding variable tree (docs/debug-protocol.md's "Lazy-load
// entries" requirement: never enumerate a huge table eagerly). Each node
// only calls `getVariables(reference)` when the user actually expands it.
//
// `editable` (plus `threadId`/`frameIndex`) turns on click-to-edit for a
// *top-level, non-expandable* row, backed by `DebugSession.setVariable` -
// see `DebugPanel.tsx`'s Locals/Upvalues sections for the only places this
// is turned on. It's never propagated into nested table/metatable children
// below, since `set_variable` only resolves a name against the current
// frame's named locals/upvalues - it has no way to address a table field or
// a global.
import { useState } from "react";
import type { DebugSession, VariableInfo } from "./debug-session";

interface VariablesTreeProps {
  session: DebugSession;
  variables: VariableInfo[];
  editable?: boolean;
  threadId?: number;
  frameIndex?: number;
  /** Called after a successful edit, so the parent can refresh this list (and anything derived from it, like watches). */
  onEdited?: () => void;
}

export function VariablesTree({
  session,
  variables,
  editable,
  threadId,
  frameIndex,
  onEdited,
}: VariablesTreeProps) {
  if (variables.length === 0) {
    return <div className="variables-empty">(none)</div>;
  }
  return (
    <ul className="variables-tree">
      {variables.map((v) => (
        <VariableNode
          key={variableKey(v)}
          session={session}
          variable={v}
          editable={editable && !v.expandable}
          threadId={threadId}
          frameIndex={frameIndex}
          onEdited={onEdited}
        />
      ))}
    </ul>
  );
}

/**
 * A stable React key for a variable row. Table/userdata rows (`expandable`)
 * are keyed by name *and* reference, so a name rebound to a genuinely
 * different object (its subtree's expand/children state no longer applies)
 * gets a fresh component instance. Scalar rows are keyed by name alone -
 * keying them by `display` too (as an earlier version of this file did)
 * meant every value change, including a `setVariable` edit, force-remounted
 * the row instead of letting React just re-render it in place, which is
 * needless churn and was observed to occasionally leave the freshly
 * mounted node's text unpainted under headless Chromium immediately after
 * replacing a focused `<input>`.
 */
function variableKey(variable: VariableInfo): string {
  return variable.expandable ? `${variable.name}:${variable.reference}` : variable.name;
}

/** A raw display string re-quoted as a valid Lua literal, so editing a string starts from `"hello"`, not the bare `hello` `display` shows. */
function editableLiteral(variable: VariableInfo): string {
  if (variable.valueType !== "string") return variable.display;
  const escaped = variable.display.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
  return `"${escaped}"`;
}

function VariableNode({
  session,
  variable,
  editable,
  threadId,
  frameIndex,
  onEdited,
}: {
  session: DebugSession;
  variable: VariableInfo;
  editable?: boolean;
  threadId?: number;
  frameIndex?: number;
  onEdited?: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [children, setChildren] = useState<VariableInfo[] | null>(null);
  const [metaRef, setMetaRef] = useState<number | null | undefined>(undefined);
  const [editing, setEditing] = useState(false);
  const [editValue, setEditValue] = useState("");
  const [editError, setEditError] = useState<string | null>(null);

  const toggle = async () => {
    if (!variable.expandable || variable.reference === null) return;
    if (!expanded) {
      const [entries, meta] = await Promise.all([
        session.getVariables(variable.reference),
        session.getMetatable(variable.reference),
      ]);
      setChildren(entries);
      setMetaRef(meta);
    }
    setExpanded((e) => !e);
  };

  const startEditing = (e: React.MouseEvent) => {
    if (!editable || threadId === undefined || frameIndex === undefined) return;
    e.stopPropagation();
    setEditError(null);
    setEditValue(editableLiteral(variable));
    setEditing(true);
  };

  const commitEdit = async () => {
    if (threadId === undefined || frameIndex === undefined) return;
    const result = await session.setVariable(threadId, frameIndex, variable.name, editValue);
    if (result.ok) {
      setEditing(false);
      setEditError(null);
      onEdited?.();
    } else {
      setEditError(result.display);
    }
  };

  const cancelEdit = () => {
    setEditing(false);
    setEditError(null);
  };

  return (
    <li className="variable-node">
      <div
        className={`variable-row ${variable.expandable ? "expandable" : ""} ${editable ? "editable" : ""}`}
        onClick={variable.expandable ? toggle : undefined}
      >
        {variable.expandable && <span className="variable-arrow">{expanded ? "▼" : "▶"}</span>}
        <span className="variable-name">{variable.name}</span>
        {editing ? (
          <input
            type="text"
            className="variable-edit-input"
            autoFocus
            value={editValue}
            onClick={(e) => e.stopPropagation()}
            onChange={(e) => setEditValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitEdit();
              if (e.key === "Escape") cancelEdit();
            }}
            onBlur={cancelEdit}
          />
        ) : (
          <span
            className={`variable-display ${editable ? "editable" : ""}`}
            title={variable.valueType}
            onClick={startEditing}
          >
            {variable.display}
          </span>
        )}
      </div>
      {editError && <div className="variable-edit-error">{editError}</div>}
      {expanded && children && (
        <ul className="variables-tree nested">
          {children.map((c) => (
            <VariableNode key={variableKey(c)} session={session} variable={c} />
          ))}
          {metaRef != null && (
            <li className="variable-node">
              <MetatableNode session={session} reference={metaRef} />
            </li>
          )}
        </ul>
      )}
    </li>
  );
}

function MetatableNode({ session, reference }: { session: DebugSession; reference: number }) {
  const [expanded, setExpanded] = useState(false);
  const [entries, setEntries] = useState<VariableInfo[] | null>(null);

  const toggle = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!expanded && !entries) {
      setEntries(await session.getVariables(reference));
    }
    setExpanded((v) => !v);
  };

  return (
    <>
      <div className="variable-row expandable" onClick={toggle}>
        <span className="variable-arrow">{expanded ? "▼" : "▶"}</span>
        <span className="variable-name">metatable</span>
      </div>
      {expanded && entries && (
        <ul className="variables-tree nested">
          {entries.map((c) => (
            <VariableNode key={variableKey(c)} session={session} variable={c} />
          ))}
        </ul>
      )}
    </>
  );
}
