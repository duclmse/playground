// Phase 8 execution timeline UI (docs/debug-protocol.md#advanced-execution-timeline-phase-8):
// renders the capped `line`/`call`/`return`/`exception`/`terminated` event
// stream `record_timeline()` produces. debug-protocol.md frames this as
// "primarily an educational/visualization feature," so this stays a
// straightforward indented event list (indentation tracks call depth) with
// type-colored badges - not a heavier visualization - rather than
// something this pass would need its own design process to build well.
import type { TimelineEventInfo } from "./debug-protocol";

export interface TimelinePanelProps {
  events: TimelineEventInfo[];
  truncated: boolean;
  onClose: () => void;
}

export function TimelinePanel({ events, truncated, onClose }: TimelinePanelProps) {
  let depth = 0;
  const rows = events.map((e, i) => {
    if (e.eventType === "return") depth = Math.max(0, depth - 1);
    const row = { event: e, depth, index: i };
    if (e.eventType === "call") depth += 1;
    return row;
  });

  return (
    <section className="analysis-panel">
      <div className="analysis-header">
        <h2>Execution Timeline</h2>
        <button type="button" className="icon-button" onClick={onClose} title="Close">
          ×
        </button>
      </div>
      {truncated && (
        <p className="analysis-note">
          Recording stopped early at the event cap - the program still ran to completion.
        </p>
      )}
      {events.length === 0 ? (
        <p className="analysis-empty">No events recorded.</p>
      ) : (
        <ol className="timeline-list">
          {rows.map(({ event, depth, index }) => (
            <li key={index} style={{ paddingLeft: `${depth * 1.1}rem` }}>
              <span className="timeline-duration" title="Opcode steps since the previous event">
                +{event.duration}
              </span>
              <span className={`timeline-badge timeline-${event.eventType}`}>{event.eventType}</span>
              {event.source && (
                <span className="timeline-location">
                  {event.source}:{event.line ?? "?"}
                </span>
              )}
              {event.local0 != null && <span className="timeline-local0">R0={event.local0}</span>}
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
