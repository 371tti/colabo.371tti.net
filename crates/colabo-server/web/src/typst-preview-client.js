let worker = null;
let nextRequestId = 0;
const pendingRequests = new Map();

function getWorker() {
  if (worker) return worker;
  worker = new Worker(new URL("./typst-preview-worker.js", import.meta.url), {
    type: "module",
    name: "typst-preview",
  });
  worker.addEventListener("message", handleWorkerMessage);
  worker.addEventListener("error", handleWorkerFailure);
  worker.addEventListener("messageerror", handleWorkerFailure);
  return worker;
}

export function invalidateTypstWorkspace(workspaceId, event) {
  if (!worker || !event) return;
  worker.postMessage({ type: "invalidate", workspaceId, event });
}

export function renderTypstWorkspace({ workspaceId, path, source, openSources = [] }) {
  const requestId = ++nextRequestId;
  const requestKey = `${workspaceId}\n${path}`;
  return new Promise((resolve, reject) => {
    pendingRequests.set(requestId, { resolve, reject });
    try {
      getWorker().postMessage({
        type: "render",
        requestId,
        requestKey,
        workspaceId,
        path,
        source,
        openSources,
      });
    } catch (error) {
      pendingRequests.delete(requestId);
      reject(error);
    }
  });
}

function handleWorkerMessage({ data }) {
  if (!data || data.type !== "result" && data.type !== "error") return;
  const pending = pendingRequests.get(data.requestId);
  if (!pending) return;
  pendingRequests.delete(data.requestId);
  if (data.type === "error") {
    const error = new Error(data.message || "Typst のプレビューに失敗しました。");
    if (data.name) error.name = data.name;
    pending.reject(error);
    return;
  }
  pending.resolve(data.svg
    ? { svg: data.svg, warnings: data.warnings || [] }
    : null);
}

function handleWorkerFailure(event) {
  const currentWorker = worker;
  worker = null;
  currentWorker?.terminate();
  const error = new Error(event.message || "Typst プレビュー Worker を起動できませんでした。");
  for (const pending of pendingRequests.values()) pending.reject(error);
  pendingRequests.clear();
}
