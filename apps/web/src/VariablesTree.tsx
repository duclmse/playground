// Lazy-expanding variable tree (docs/debug-protocol.md's "Lazy-load
// entries" requirement: never enumerate a huge table eagerly). Each node
// only calls `getVariables(reference)` when the user actually expands it.
import { useState } from "react";
import type { DebugSession, VariableInfo } from "./debug-session";

interface VariablesTreeProps {
  session: DebugSession;
  variables: VariableInfo[];
}

export function VariablesTree({ session, variables }: VariablesTreeProps) {
  if (variables.length === 0) {
    return <div className="variables-empty">(none)</div>;
  }
  return (
    <ul className="variables-tree">
      {variables.map((v) => (
        <VariableNode key={`${v.name}:${v.reference ?? v.display}`} session={session} variable={v} />
      ))}
    </ul>
  );
}

function VariableNode({ session, variable }: { session: DebugSession; variable: VariableInfo }) {
  const [expanded, setExpanded] = useState(false);
  const [children, setChildren] = useState<VariableInfo[] | null>(null);
  const [metaRef, setMetaRef] = useState<number | null | undefined>(undefined);

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

  return (
    <li className="variable-node">
      <div
        className={`variable-row ${variable.expandable ? "expandable" : ""}`}
        onClick={variable.expandable ? toggle : undefined}
      >
        {variable.expandable && <span className="variable-arrow">{expanded ? "▼" : "▶"}</span>}
        <span className="variable-name">{variable.name}</span>
        <span className="variable-display" title={variable.valueType}>
          {variable.display}
        </span>
      </div>
      {expanded && children && (
        <ul className="variables-tree nested">
          {children.map((c) => (
            <VariableNode key={`${c.name}:${c.reference ?? c.display}`} session={session} variable={c} />
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
            <VariableNode key={`${c.name}:${c.reference ?? c.display}`} session={session} variable={c} />
          ))}
        </ul>
      )}
    </>
  );
}
