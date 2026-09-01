// Phase 8 profiler UI (docs/debug-protocol.md#advanced-profiler-phase-8):
// renders the `FunctionStats` a `profile()` run produces. All the actual
// profiling logic lives in crates/lua-vm/src/profiler.rs; this is just a
// sortable table over its output.
import { useMemo, useState } from "react";
import type { FunctionStatsInfo } from "./debug-protocol";

type SortKey = "functionId" | "calls" | "totalInstructions" | "selfInstructions";

export interface ProfilerPanelProps {
  stats: FunctionStatsInfo[];
  onClose: () => void;
}

export function ProfilerPanel({ stats, onClose }: ProfilerPanelProps) {
  const [sortKey, setSortKey] = useState<SortKey>("selfInstructions");
  const [descending, setDescending] = useState(true);

  const sorted = useMemo(() => {
    const copy = [...stats];
    copy.sort((a, b) => {
      const av = a[sortKey];
      const bv = b[sortKey];
      const cmp = typeof av === "string" ? av.localeCompare(bv as string) : (av as number) - (bv as number);
      return descending ? -cmp : cmp;
    });
    return copy;
  }, [stats, sortKey, descending]);

  const toggleSort = (key: SortKey) => {
    if (key === sortKey) {
      setDescending((d) => !d);
    } else {
      setSortKey(key);
      setDescending(true);
    }
  };

  const arrow = (key: SortKey) => (key === sortKey ? (descending ? " ▼" : " ▲") : "");

  return (
    <section className="analysis-panel">
      <div className="analysis-header">
        <h2>Profiler</h2>
        <button type="button" className="icon-button" onClick={onClose} title="Close">
          ×
        </button>
      </div>
      {stats.length === 0 ? (
        <p className="analysis-empty">No profile data.</p>
      ) : (
        <table className="profiler-table">
          <thead>
            <tr>
              <th onClick={() => toggleSort("functionId")}>Function{arrow("functionId")}</th>
              <th onClick={() => toggleSort("calls")}>Calls{arrow("calls")}</th>
              <th onClick={() => toggleSort("totalInstructions")}>
                Total instr.{arrow("totalInstructions")}
              </th>
              <th onClick={() => toggleSort("selfInstructions")}>
                Self instr.{arrow("selfInstructions")}
              </th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((s) => (
              <tr key={s.functionId}>
                <td className="profiler-function-id">{s.functionId}</td>
                <td>{s.calls}</td>
                <td>{s.totalInstructions.toLocaleString()}</td>
                <td>{s.selfInstructions.toLocaleString()}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
