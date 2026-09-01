// One-shot analysis requests (Phase 8: profiler, execution timeline) over
// the same worker `DebugSession` uses - these aren't part of a debug
// session's lifecycle (no launch/continue/breakpoints), just "run this
// project once, gathering stats/events instead of just output," so they
// get their own tiny request/response helper rather than living on
// `DebugSession`. Each call adds a short-lived `message` listener, sends
// one request, and removes the listener once its matching response
// arrives - same id-correlation idea as `DebugSession.send`, just without
// a whole class's worth of state for two one-shot calls.

import { allocateRequestId, type FunctionStatsInfo, type TimelineInfo, type WorkerEvent } from "./debug-protocol";

function sendOneShot<T extends WorkerEvent>(
  worker: Worker,
  request: object,
  isMatch: (event: WorkerEvent) => event is T,
): Promise<T> {
  const id = allocateRequestId();
  return new Promise<T>((resolve, reject) => {
    const handleMessage = (event: MessageEvent<WorkerEvent>) => {
      const message = event.data;
      if ("id" in message && message.id === id) {
        worker.removeEventListener("message", handleMessage);
        if (message.type === "error") {
          reject(new Error(message.message));
        } else if (isMatch(message)) {
          resolve(message);
        }
      }
    };
    worker.addEventListener("message", handleMessage);
    worker.postMessage({ ...request, id });
  });
}

export async function runProfile(
  worker: Worker,
  files: Record<string, string>,
  entry: string,
): Promise<FunctionStatsInfo[]> {
  const result = await sendOneShot(
    worker,
    { type: "profile", files, entry },
    (e): e is Extract<WorkerEvent, { type: "profileResult" }> => e.type === "profileResult",
  );
  return result.stats;
}

export async function runTimeline(
  worker: Worker,
  files: Record<string, string>,
  entry: string,
  maxEvents: number,
): Promise<TimelineInfo> {
  const result = await sendOneShot(
    worker,
    { type: "recordTimeline", files, entry, maxEvents },
    (e): e is Extract<WorkerEvent, { type: "timelineResult" }> => e.type === "timelineResult",
  );
  return result.timeline;
}
