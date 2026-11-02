import { invalidateTypstWorkspace, renderTypstWorkspace } from "./typst-preview.js";

const latestRequests = new Map();

self.addEventListener("message", (event) => {
  const message = event.data;
  if (message?.type === "invalidate") {
    invalidateTypstWorkspace(message.workspaceId, message.event);
  } else if (message?.type === "render") {
    void render(message);
  }
});

async function render(message) {
  const { requestId, requestKey, workspaceId, path } = message;
  latestRequests.set(requestKey, requestId);
  try {
    const result = await renderTypstWorkspace({
      workspaceId,
      path,
      source: message.source,
      openSources: message.openSources,
      listDirectory: (directory) => listWorkspaceDirectory(workspaceId, directory),
      readFile: (filePath) => readWorkspaceFile(workspaceId, filePath),
      isCurrent: () => latestRequests.get(requestKey) === requestId,
    });
    if (!result) {
      self.postMessage({ type: "result", requestId, pdf: null });
      return;
    }
    const svg = new TextEncoder().encode(result.svg);
    const buffer = svg.buffer.slice(svg.byteOffset, svg.byteOffset + svg.byteLength);
    self.postMessage({ type: "result", requestId, svg: buffer, warnings: result.warnings }, [buffer]);
  } catch (error) {
    self.postMessage({
      type: "error",
      requestId,
      name: error instanceof Error ? error.name : "Error",
      message: error instanceof Error ? error.message : String(error),
    });
  } finally {
    if (latestRequests.get(requestKey) === requestId) latestRequests.delete(requestKey);
  }
}

async function listWorkspaceDirectory(workspaceId, path) {
  const response = await fetch(workspaceApiPath(workspaceId, "files", { path }), {
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  });
  if (!response.ok) throw await responseError(response, "ワークスペースのファイル一覧を読み込めませんでした。");
  return response.json();
}

async function readWorkspaceFile(workspaceId, path) {
  const response = await fetch(workspaceApiPath(workspaceId, "file", { path }), {
    credentials: "same-origin",
  });
  if (!response.ok) throw await responseError(response, `ファイルを読み込めません (${response.status})`);
  return new Uint8Array(await response.arrayBuffer());
}

function workspaceApiPath(workspaceId, endpoint, query) {
  const url = new URL(`/api/workspaces/${encodeURIComponent(workspaceId)}/${endpoint}`, self.location.origin);
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== null) url.searchParams.set(key, value);
  }
  return url.href;
}

async function responseError(response, fallback) {
  try {
    const payload = await response.json();
    return new Error(payload.error || fallback);
  } catch (_) {
    return new Error(fallback);
  }
}
