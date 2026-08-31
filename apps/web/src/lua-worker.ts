/// <reference lib="webworker" />
import init, { execute } from "@lua-playground/runtime";

export type WorkerRequest = { type: "run"; source: string };
export type WorkerEvent =
  | { type: "result"; output: string; error: string | null }
  | { type: "ready" };

let ready: Promise<unknown> | null = null;

async function ensureReady() {
  if (!ready) {
    ready = init();
  }
  await ready;
}

self.onmessage = async (event: MessageEvent<WorkerRequest>) => {
  const message = event.data;
  if (message.type !== "run") return;

  await ensureReady();
  const result = execute(message.source);
  const response: WorkerEvent = {
    type: "result",
    output: result.output,
    error: result.error ?? null,
  };
  self.postMessage(response);
};

ensureReady().then(() => {
  const response: WorkerEvent = { type: "ready" };
  self.postMessage(response);
});
