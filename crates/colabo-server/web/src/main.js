import DOMPurify from "dompurify";
import { marked } from "marked";
import { tablerIcon } from "./icons.js";
import {
  createEditorGroup,
  editorGroupForPath,
  editorGroups,
  findEditorGroup,
  loadEditorLayout,
  removeEditorGroup,
  saveEditorLayout,
  splitEditorGroup,
} from "./editor-layout.js";
import { confirmDialog, inputDialog, quickOpenDialog, workspaceCreationDialog } from "./workspace-dialogs.js";
import "./app.css";
import "./embed.js";

const app = document.querySelector("#app");
const WORKSPACE_VIEWS = ["explorer", "chat", "members", "history", "settings"];
const WORKSPACE_VIEW_NAMES = { explorer: "エクスプローラー", chat: "チャット", members: "メンバー", history: "履歴", settings: "設定" };
const WORKSPACE_VIEW_SHORT_NAMES = { explorer: "ファイル", chat: "チャット", members: "メンバー", history: "履歴", settings: "設定" };
const WORKSPACE_VIEW_ICONS = { explorer: "folder-open", chat: "messages", members: "users", history: "history", settings: "settings" };
const WORKSPACE_VIEW_PREFIX = "workspace-view:";
const FILE_PREVIEW_PREFIX = "workspace-preview:";
const MAX_TEXT_FILE_BYTES = 24 * 1024 * 1024;
const state = {
  user: null,
  csrf: "",
  workspaces: [],
  workspace: null,
  workspaceRole: "reader",
  workspaceId: "",
  view: "files",
  currentDir: "",
  entries: [],
  activeFile: null,
  editorMount: null,
  editorChange: null,
  activeTabId: null,
  editorLayout: createEditorGroup(),
  editorLayoutKey: "",
  focusedGroupId: "editor-1",
  openFiles: new Map(),
  eventSocket: null,
  chatSocket: null,
  reconnectTimers: [],
  chatMessages: [],
  memberProfiles: new Map(),
  notice: "",
  typstRefreshTimer: 0,
};
let typstPreviewModulePromise = null;

function loadTypstPreviewModule() {
  typstPreviewModulePromise ||= import("./typst-preview-client.js");
  return typstPreviewModulePromise;
}

function invalidateTypstWorkspace(workspaceId = state.workspaceId, event) {
  const previewEvent = event?.kind === "file_replaced" ? { ...event, kind: "file_changed" } : event;
  typstPreviewModulePromise?.then((module) => module.invalidateTypstWorkspace(workspaceId, previewEvent));
}

function scheduleTypstPreviewRefresh() {
  clearTimeout(state.typstRefreshTimer);
  state.typstRefreshTimer = setTimeout(() => {
    for (const file of state.openFiles.values()) {
      if (extension(file.path) === "typ" && file.previewTarget) updatePreview(file);
    }
  }, 320);
}

function workspaceViewTab(view) {
  return `${WORKSPACE_VIEW_PREFIX}${view}`;
}

function workspaceViewForTab(tabId) {
  if (typeof tabId !== "string" || !tabId.startsWith(WORKSPACE_VIEW_PREFIX)) return null;
  const view = tabId.slice(WORKSPACE_VIEW_PREFIX.length);
  return WORKSPACE_VIEWS.includes(view) ? view : null;
}

function activeWorkspaceView() {
  if (!state.workspace) return "";
  return state.view === "files" ? "explorer" : state.view;
}

function filePreviewTab(path) {
  return `${FILE_PREVIEW_PREFIX}${path}`;
}

function filePreviewSource(tabId) {
  return typeof tabId === "string" && tabId.startsWith(FILE_PREVIEW_PREFIX)
    ? tabId.slice(FILE_PREVIEW_PREFIX.length)
    : null;
}

function editorTabName(tabId) {
  const view = workspaceViewForTab(tabId);
  if (view) return WORKSPACE_VIEW_NAMES[view];
  const previewPath = filePreviewSource(tabId);
  const path = previewPath || tabId;
  const name = path.split("/").pop();
  return previewPath ? `${name} プレビュー` : name;
}

function userName(user) {
  return user?.display_name || user?.preferred_username || user?.subject || "ユーザー";
}

function avatarContent(user) {
  let picture = null;
  try {
    const url = new URL(user?.picture_url);
    if (["http:", "https:"].includes(url.protocol) && url.hostname && !url.username && !url.password) picture = url.href;
  } catch (_) {}
  return `${tablerIcon("user-circle")}${picture ? `<img class="user-avatar-image" src="${escapeHtml(picture)}" alt="" loading="lazy" referrerpolicy="no-referrer">` : ""}`;
}

function avatarMarkup(user, className, decorative = false) {
  return `<span class="user-avatar ${className}"${decorative ? ' aria-hidden="true"' : ""}>${avatarContent(user)}</span>`;
}

function bindAvatarImages(root = document) {
  root.querySelectorAll(".user-avatar-image").forEach((image) => {
    image.addEventListener("error", () => image.remove(), { once: true });
  });
}

function removeLegacyWorkspaceTabs(layout, focusedGroupId) {
  for (const group of editorGroups(layout)) {
    group.tabs = group.tabs.filter((tabId) => !workspaceViewForTab(tabId));
    if (!group.tabs.includes(group.activePath)) group.activePath = group.tabs[0] || null;
  }
  let next = layout;
  for (const group of [...editorGroups(next)]) {
    if (editorGroups(next).length > 1 && !group.tabs.length) next = removeEditorGroup(next, group.id);
  }
  const groups = editorGroups(next);
  if (!groups.length || groups.every((group) => !group.tabs.length)) {
    return { layout: createEditorGroup(), focusedGroupId: "editor-1" };
  }
  return {
    layout: next,
    focusedGroupId: groups.some((group) => group.id === focusedGroupId) ? focusedGroupId : groups[0].id,
  };
}

function ensureExplorerTab(layout, focusedGroupId) {
  const tabId = workspaceViewTab("explorer");
  const existing = editorGroupForPath(layout, tabId);
  if (existing) return { layout, focusedGroupId: focusedGroupId || existing.id };
  let group = findEditorGroup(layout, focusedGroupId) || editorGroups(layout)[0];
  if (!group) {
    group = createEditorGroup();
    layout = group;
  }
  group.tabs.push(tabId);
  if (!group.activePath) group.activePath = tabId;
  return { layout, focusedGroupId: focusedGroupId || group.id };
}

marked.setOptions({ gfm: true, breaks: true });

class ApiFault extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

async function request(path, options = {}) {
  const method = (options.method || "GET").toUpperCase();
  const headers = new Headers(options.headers || {});
  let body = options.body;
  if (body !== undefined && !(body instanceof FormData) && !(body instanceof Blob) && typeof body !== "string") {
    headers.set("Content-Type", "application/json");
    body = JSON.stringify(body);
  }
  if (["POST", "PUT", "PATCH", "DELETE"].includes(method)) headers.set("X-CSRF-Token", state.csrf);
  const response = await fetch(path, { ...options, method, headers, body, credentials: "same-origin" });
  if (!response.ok) {
    let message = `リクエストに失敗しました (${response.status})`;
    try { message = (await response.json()).error || message; } catch (_) {}
    throw new ApiFault(response.status, message);
  }
  if (response.status === 204) return null;
  const contentType = response.headers.get("content-type") || "";
  return contentType.includes("application/json") ? response.json() : response;
}

function apiPath(workspaceId, endpoint, query = {}) {
  const url = new URL(`/api/workspaces/${encodeURIComponent(workspaceId)}/${endpoint}`, location.origin);
  for (const [key, value] of Object.entries(query)) if (value !== undefined && value !== null) url.searchParams.set(key, value);
  return url.pathname + url.search;
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (character) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[character]);
}

function inviteFromLocation() {
  const match = location.pathname.match(/^\/invite\/([0-9a-f-]{36})\/([A-Za-z0-9_-]{32,128})$/i);
  return match ? { workspaceId: match[1], token: match[2], path: location.pathname + location.search } : null;
}

function loginPath(returnTo = location.pathname + location.search) {
  return `/auth/login?returnTo=${encodeURIComponent(returnTo.startsWith("/") ? returnTo : "/")}`;
}

async function boot() {
  const invite = inviteFromLocation();
  let session;
  try {
    session = await request("/api/session");
  } catch (error) {
    if (!(error instanceof ApiFault) || error.status !== 401) {
      renderMessage("接続できません", error.message || "サーバーに接続できませんでした。");
      return;
    }
    const status = await fetch("/auth/status").then((response) => response.json()).catch(() => ({ configured: false }));
    const invitation = invite ? await request(`/api/invites/${invite.workspaceId}/${invite.token}`).catch(() => null) : null;
    renderSignIn(status.configured, invitation, invite?.path);
    return;
  }
  state.user = session.user;
  state.csrf = session.csrf_token;
  if (invite) {
    await renderInvite(invite);
    return;
  }
  await loadWorkspaces();
  const selected = new URLSearchParams(location.search).get("workspace");
  if (selected && state.workspaces.some((workspace) => workspace.id === selected)) {
    await selectWorkspace(selected, false);
  } else {
    renderHome();
  }
}

function renderSignIn(configured, invitation, returnTo = "/") {
  app.innerHTML = `
    <main class="signin-screen">
      <section class="signin-card" aria-labelledby="signin-title">
        <p class="wordmark">colabo<span aria-hidden="true">.</span></p>
        <p class="eyebrow">共同作業ワークスペース</p>
        <h1 id="signin-title">一緒に、つくる。</h1>
        <p class="muted">ファイルを囲んで、考えを共有するための小さな作業場です。</p>
        ${invitation ? `<div class="invite-summary"><span class="eyebrow">招待されています</span><strong>${escapeHtml(invitation.workspace_name)}</strong><span>${invitation.role === "writer" ? "編集者" : "閲覧者"}として参加できます</span></div>` : ""}
        ${configured
          ? `<a class="button button-primary signin-action" href="${escapeHtml(loginPath(returnTo))}">OIDC でログイン</a>`
          : `<div class="notice notice-warning" role="status">OIDC ログイン設定がありません。サーバーに OIDC クライアント ID、シークレット、コールバック URL を設定してください。</div>`}
        ${new URLSearchParams(location.search).has("login") ? `<p class="notice">ログインはキャンセルされました。</p>` : ""}
        <p class="signin-footnote">ワークスペースとファイルは招待されたメンバーだけが利用できます。</p>
      </section>
    </main>`;
}

function renderMessage(title, message) {
  app.innerHTML = `<main class="signin-screen"><section class="signin-card"><p class="wordmark">colabo<span>.</span></p><h1>${escapeHtml(title)}</h1><p class="muted">${escapeHtml(message)}</p><a class="button button-secondary" href="/">戻る</a></section></main>`;
}

async function renderInvite(invite) {
  let preview;
  try { preview = await request(`/api/invites/${invite.workspaceId}/${invite.token}`); }
  catch (error) { renderMessage("招待を確認できません", error.message); return; }
  app.innerHTML = `
    <main class="signin-screen">
      <section class="signin-card">
        <p class="wordmark">colabo<span>.</span></p>
        <p class="eyebrow">WORKSPACE INVITATION</p>
        <h1>${escapeHtml(preview.workspace_name)} に参加</h1>
        <p class="muted">参加すると、${preview.role === "writer" ? "ファイルの編集と管理" : "ファイルの閲覧"}、プレビュー、チャットを利用できます。</p>
        <p class="invite-expiry">有効期限: ${escapeHtml(new Date(preview.expires_at).toLocaleString("ja-JP"))}</p>
        <div class="button-row"><button class="button button-primary" id="acceptInvite">参加する</button><a class="button button-secondary" href="/">あとで</a></div>
        <p class="form-error" id="inviteError" role="alert" hidden></p>
      </section>
    </main>`;
  document.querySelector("#acceptInvite").addEventListener("click", async () => {
    const button = document.querySelector("#acceptInvite");
    button.disabled = true;
    try {
      await request(`/api/invites/${invite.workspaceId}/${invite.token}/accept`, { method: "POST" });
      location.href = `/?workspace=${encodeURIComponent(invite.workspaceId)}`;
    } catch (error) {
      const target = document.querySelector("#inviteError");
      target.textContent = error.message;
      target.hidden = false;
      button.disabled = false;
    }
  });
}

async function loadWorkspaces() {
  state.workspaces = await request("/api/workspaces");
}

function renderShell() {
  app.innerHTML = `
    <div class="app-shell">
      <header class="app-topbar">
        <a class="brand" href="/" aria-label="colabo ホーム"><span class="brand-mark">c</span></a>
        <div class="workspace-switcher">
          <button class="workspace-switcher-trigger" id="workspaceMenuButton" type="button" aria-haspopup="true" aria-expanded="false"><span id="topbarTitle">ワークスペース</span>${tablerIcon("chevron-down")}</button>
          <section class="workspace-switcher-menu" id="workspaceMenu" hidden>
            <header><span>ワークスペース</span><button class="icon-button" id="createWorkspace" type="button" aria-label="ワークスペースを作成" title="新しいワークスペース">${tablerIcon("plus")}</button></header>
            <nav class="workspace-list" id="workspaceList" aria-label="ワークスペース一覧"></nav>
          </section>
        </div>
        <button class="quick-open-trigger" id="quickOpen" type="button" aria-label="ファイルへ移動" title="ファイルへ移動">${tablerIcon("search")}<span>ファイルへ移動</span><kbd>Ctrl P</kbd></button>
        <div class="topbar-spacer"></div>
        <button class="topbar-user" id="accountButton" type="button" aria-label="アカウント設定: ${escapeHtml(userName(state.user))}" title="アカウント設定">${avatarMarkup(state.user, "topbar-avatar", true)}<span>${escapeHtml(userName(state.user))}</span></button>
        <button class="icon-button topbar-action" id="logoutButton" type="button" aria-label="ログアウト" title="ログアウト">${tablerIcon("logout")}</button>
      </header>
      <nav class="activity-bar" aria-label="ワークスペースビュー">
        ${WORKSPACE_VIEWS.map((view) => `<button class="activity-button${activeWorkspaceView() === view ? " is-active" : ""}" type="button" data-activity="${view}" aria-label="${WORKSPACE_VIEW_NAMES[view]}"${activeWorkspaceView() === view ? ' aria-current="page"' : ""} title="${WORKSPACE_VIEW_NAMES[view]}"${state.workspace ? "" : " disabled"}>${tablerIcon(WORKSPACE_VIEW_ICONS[view])}<span class="activity-label">${WORKSPACE_VIEW_SHORT_NAMES[view]}</span></button>`).join("")}
        <span class="activity-spacer"></span>
        ${avatarMarkup(state.user, "activity-user", true)}
      </nav>
      <main class="main-column"><main class="page-content" id="pageContent"></main></main>
      <footer class="editor-statusbar" id="editorStatusbar" hidden></footer>
    </div>`;
  const list = document.querySelector("#workspaceList");
  for (const workspace of state.workspaces) {
    const button = document.createElement("button");
    button.className = `workspace-item${workspace.id === state.workspaceId ? " is-selected" : ""}`;
    button.type = "button";
    button.textContent = workspace.name;
    button.title = `${workspace.name} · ${workspace.role}`;
    button.addEventListener("click", () => {
      document.querySelector("#workspaceMenu").hidden = true;
      document.querySelector("#workspaceMenuButton").setAttribute("aria-expanded", "false");
      selectWorkspace(workspace.id);
    });
    list.append(button);
  }
  bindAvatarImages(app);
  const workspaceMenuButton = document.querySelector("#workspaceMenuButton");
  const workspaceMenu = document.querySelector("#workspaceMenu");
  workspaceMenuButton.addEventListener("click", () => {
    workspaceMenu.hidden = !workspaceMenu.hidden;
    workspaceMenuButton.setAttribute("aria-expanded", String(!workspaceMenu.hidden));
  });
  document.querySelector("#createWorkspace").addEventListener("click", createWorkspace);
  document.querySelector("#accountButton").addEventListener("click", () => showAccount(false));
  document.querySelector("#logoutButton").addEventListener("click", logout);
  document.querySelector("#quickOpen").addEventListener("click", showQuickOpen);
  document.querySelectorAll("[data-activity]").forEach((button) => button.addEventListener("click", async () => {
    const view = button.dataset.activity;
    if (!state.workspace || !WORKSPACE_VIEWS.includes(view)) return;
    if (!document.querySelector("#tabContent")) await selectWorkspace(state.workspaceId, false);
    await openWorkspaceView(view);
  }));
}

function renderHome() {
  closeWorkspaceSockets();
  disposeEditor();
  state.workspace = null;
  state.workspaceId = "";
  state.view = "files";
  renderShell();
  document.querySelector("#pageContent").classList.remove("is-workspace-page");
  document.querySelector("#topbarTitle").textContent = "ワークスペース";
  const content = document.querySelector("#pageContent");
  content.innerHTML = `
    <section class="welcome-block"><p class="eyebrow">YOUR WORKSPACES</p><h1>作業を、続けよう。</h1><p class="muted">参加しているワークスペースを開くか、新しく作成します。</p><button class="button button-primary" id="welcomeCreate">ワークスペースを作成</button></section>
    <section class="workspace-cards" aria-label="ワークスペース一覧"></section>`;
  content.querySelector("#welcomeCreate").addEventListener("click", createWorkspace);
  const cards = content.querySelector(".workspace-cards");
  if (!state.workspaces.length) {
    cards.innerHTML = `<p class="empty-state">まだワークスペースがありません。</p>`;
  } else {
    state.workspaces.forEach((workspace) => {
      const card = document.createElement("button");
      card.className = "workspace-card";
      card.type = "button";
      card.innerHTML = `<span class="card-mark">${escapeHtml(workspace.name.slice(0, 1))}</span><span class="card-copy"><strong>${escapeHtml(workspace.name)}</strong><small>${escapeHtml(workspace.role)} · ${new Date(workspace.created_at).toLocaleDateString("ja-JP")}</small></span><span class="card-arrow" aria-hidden="true">${tablerIcon("arrow-right")}</span>`;
      card.addEventListener("click", () => selectWorkspace(workspace.id));
      cards.append(card);
    });
  }
}

async function createWorkspace() {
  const choice = await workspaceCreationDialog();
  if (!choice) return;
  let workspace = null;
  try {
    workspace = await request("/api/workspaces", { method: "POST", body: { name: choice.name } });
    if (choice.kind === "folder") {
      showNotice("フォルダーのファイルを取り込んでいます…");
      try {
        const form = new FormData();
        form.append("folder", "");
        for (const file of choice.files) {
          const relative = file.webkitRelativePath || file.name;
          const segments = relative.split("/");
          const path = segments[0] === choice.folderName ? segments.slice(1).join("/") : relative;
          if (!path) continue;
          form.append("file_path", path);
          form.append("files", file, file.name);
        }
        await request(apiPath(workspace.id, "upload"), { method: "POST", body: form });
      } catch (error) {
        await request(`/api/workspaces/${workspace.id}`, { method: "DELETE" }).catch(() => {});
        throw error;
      }
    }
    await loadWorkspaces();
    await selectWorkspace(workspace.id);
  } catch (error) { showNotice(error.message); }
}

async function selectWorkspace(workspaceId, updateLocation = true) {
  try {
    closeWorkspaceSockets();
    state.workspaceId = workspaceId;
    state.workspace = await request(`/api/workspaces/${workspaceId}`);
    const item = state.workspaces.find((workspace) => workspace.id === workspaceId);
    state.workspaceRole = item?.role || "reader";
    state.view = "files";
    state.currentDir = "";
    disposeEditor();
    state.editorLayoutKey = `colabo.editor-layout.v3:${workspaceId}`;
    let savedLayout = loadEditorLayout(state.editorLayoutKey);
    try {
      if (!localStorage.getItem(state.editorLayoutKey)) {
        const v2Key = `colabo.editor-layout.v2:${workspaceId}`;
        if (localStorage.getItem(v2Key)) {
          savedLayout = loadEditorLayout(v2Key);
        } else {
          const legacyKey = `colabo.editor-layout.v1:${workspaceId}`;
          if (localStorage.getItem(legacyKey)) {
            const legacy = loadEditorLayout(legacyKey);
            savedLayout = removeLegacyWorkspaceTabs(legacy.layout, legacy.focusedGroupId);
          }
        }
        savedLayout = ensureExplorerTab(savedLayout.layout, savedLayout.focusedGroupId);
      }
    } catch (_) {}
    state.editorLayout = savedLayout.layout;
    state.focusedGroupId = savedLayout.focusedGroupId;
    state.openFiles = new Map();
    editorLayoutChanged();
    if (updateLocation) history.replaceState(null, "", `/?workspace=${encodeURIComponent(workspaceId)}`);
    renderShell();
    connectWorkspaceSockets(workspaceId);
    await renderWorkspace();
  } catch (error) {
    showNotice(error.message);
    if (error.status === 403 || error.status === 404) {
      await loadWorkspaces().catch(() => {});
      renderHome();
    }
  }
}

async function renderWorkspace() {
  if (!state.workspace) return;
  document.querySelector("#topbarTitle").textContent = state.workspace.name;
  const content = document.querySelector("#pageContent");
  content.classList.add("is-workspace-page");
  content.innerHTML = `
    <div class="workspace-notice" id="workspaceNotice" role="status" hidden></div>
    <section class="tab-content" id="tabContent"></section>`;
  await renderActiveTab();
}

async function renderActiveTab() {
  const target = document.querySelector("#tabContent");
  if (!target) return;
  try { await renderFiles(target); }
  catch (error) { target.innerHTML = `<div class="notice notice-error">${escapeHtml(error.message)}</div>`; }
}

async function renderFiles(target, entries = null) {
  const listing = entries || await request(apiPath(state.workspaceId, "files", { path: state.currentDir }));
  state.entries = listing;
  const groups = [...editorGroups(state.editorLayout)];
  for (const group of groups) {
    if (!group.activePath || workspaceViewForTab(group.activePath)) continue;
    const previewPath = filePreviewSource(group.activePath);
    const path = previewPath || group.activePath;
    if (!state.openFiles.has(path)) {
      try {
        state.openFiles.set(path, await loadFileData(path));
      } catch (_) {
        removeOpenPath(group.activePath);
        showNotice(`開いていたファイルを読み込めません: ${path}`);
        continue;
      }
    }
    if (!previewPath && state.openFiles.get(path)?.previewEnabled) ensureFilePreview(path, group.id);
  }
  const mountedPanels = new Map([...target.querySelectorAll(".workspace-panel-page")].filter((page) => page.dataset.workspaceView !== "explorer").map((page) => [
    page.dataset.workspaceView,
    { content: page.querySelector(".workspace-panel-content"), rendered: page.dataset.panelRendered === "true" },
  ]));
  const canWrite = ["owner", "writer"].includes(state.workspaceRole);
  syncFocusedFile();
  target.innerHTML = `
    <div class="file-workbench">
      <section class="editor-workbench" aria-label="ファイル編集">
        <div class="editor-layout" id="editorLayout">${renderEditorNode(state.editorLayout, canWrite)}</div>
      </section>
    </div>`;
  for (const page of target.querySelectorAll(".workspace-panel-page")) {
    const mounted = mountedPanels.get(page.dataset.workspaceView);
    if (!mounted?.content) continue;
    page.querySelector(".workspace-panel-content").replaceWith(mounted.content);
    if (mounted.rendered) page.dataset.panelRendered = "true";
  }
  await attachWorkspacePanels(target);
  attachEditorWorkbenchEvents(target, canWrite);
  attachEditorMounts(target);
  renderEditorStatus();
}

function renderExplorerPanel(content) {
  const listing = state.entries;
  const canWrite = ["owner", "writer"].includes(state.workspaceRole);
  content.classList.add("explorer-view-content");
  content.innerHTML = `
    <section class="explorer-view" aria-label="エクスプローラー">
      <header class="explorer-view-header">
        <div class="explorer-view-title"><p class="eyebrow">WORKSPACE EXPLORER</p><h1>ファイル</h1><span class="item-count">${listing.length}</span></div>
        <div class="explorer-file-actions">${canWrite ? `<button class="icon-button" type="button" data-explorer-action="new-file" aria-label="新規ファイル" title="新規ファイル">${tablerIcon("file-plus")}</button><button class="icon-button" type="button" data-explorer-action="new-folder" aria-label="新しいフォルダー" title="新しいフォルダー">${tablerIcon("folder-plus")}</button><button class="icon-button" type="button" data-explorer-action="upload" aria-label="ファイルをアップロード" title="ファイルをアップロード">${tablerIcon("upload")}</button><input class="explorer-file-picker" type="file" multiple hidden />` : ""}</div>
      </header>
      <nav class="breadcrumbs explorer-breadcrumbs" aria-label="現在のフォルダー"></nav>
      <ul class="file-list explorer-file-list"></ul>
    </section>`;
  renderBreadcrumbs(content.querySelector(".explorer-breadcrumbs"));
  const list = content.querySelector(".explorer-file-list");
  if (!listing.length) list.innerHTML = `<li class="empty-list">このフォルダーは空です。</li>`;
  listing.forEach((entry) => list.append(renderFileRow(entry, canWrite)));
  content.querySelector('[data-explorer-action="new-file"]')?.addEventListener("click", newFile);
  content.querySelector('[data-explorer-action="new-folder"]')?.addEventListener("click", newFolder);
  content.querySelector('[data-explorer-action="upload"]')?.addEventListener("click", () => content.querySelector(".explorer-file-picker").click());
  content.querySelector(".explorer-file-picker")?.addEventListener("change", uploadFiles);
}

async function attachWorkspacePanels(target) {
  for (const page of target.querySelectorAll(".workspace-panel-page:not([data-panel-rendered='true'])")) {
    const content = page.querySelector(".workspace-panel-content");
    if (!content) continue;
    try {
      const view = page.dataset.workspaceView;
      if (view === "explorer") renderExplorerPanel(content);
      else if (view === "chat") await renderChat(content);
      else if (view === "members") await renderMembers(content);
      else if (view === "history") await renderHistory(content);
      else if (view === "settings") await renderWorkspaceSettings(content);
      page.dataset.panelRendered = "true";
    } catch (error) {
      content.innerHTML = `<div class="notice notice-error">${escapeHtml(error.message)}</div>`;
      page.dataset.panelRendered = "true";
    }
  }
}

function renderEditorNode(node, canWrite) {
  if (node.type === "split") {
    return `<div class="editor-split editor-split-${node.direction}" data-split-id="${escapeHtml(node.id)}" style="--split-ratio:${Math.round(node.ratio * 100)}%"><div class="editor-split-child">${renderEditorNode(node.children[0], canWrite)}</div><div class="editor-splitter" role="separator" tabindex="0" aria-orientation="${node.direction === "horizontal" ? "vertical" : "horizontal"}" data-split-id="${escapeHtml(node.id)}"></div><div class="editor-split-child">${renderEditorNode(node.children[1], canWrite)}</div></div>`;
  }
  return renderEditorGroup(node, canWrite);
}

function renderEditorGroup(group, canWrite) {
  const focused = group.id === state.focusedGroupId;
  const activeView = workspaceViewForTab(group.activePath);
  const activePreviewPath = filePreviewSource(group.activePath);
  const activeFile = group.activePath && !activeView ? state.openFiles.get(activePreviewPath || group.activePath) : null;
  const tabs = group.tabs.map((tabId) => {
    const view = workspaceViewForTab(tabId);
    const name = editorTabName(tabId);
    const glyph = view ? tablerIcon(WORKSPACE_VIEW_ICONS[view]) : filePreviewSource(tabId) ? tablerIcon("eye") : fileGlyph(name);
    const active = tabId === group.activePath;
    return `<div class="editor-tab${active ? " is-active" : ""}" role="tab" aria-selected="${active}" title="${escapeHtml(view ? name : tabId)}" data-tab-path="${escapeHtml(tabId)}" data-group-id="${escapeHtml(group.id)}" draggable="true"><button class="editor-tab-select" type="button" data-tab-select="true"><span class="file-icon" aria-hidden="true">${glyph}</span><span class="editor-tab-name">${escapeHtml(name)}</span></button><button class="editor-tab-close" type="button" data-tab-close="true" aria-label="${escapeHtml(name)} を閉じる" title="タブを閉じる">${tablerIcon("x")}</button></div>`;
  }).join("");
  const fileActions = activeFile ? `<div class="editor-file-actions"><button class="icon-button editor-file-actions-toggle" type="button" data-file-menu-toggle="true" aria-label="ファイルの操作" aria-expanded="false" title="ファイルの操作">${tablerIcon("dots")}</button><div class="editor-file-menu" role="menu" hidden>${activePreviewPath ? `<button type="button" role="menuitem" data-file-action="refresh-preview" data-path="${escapeHtml(activeFile.path)}">${tablerIcon("refresh")}プレビューを更新</button>` : isPreviewable(activeFile.path) ? `<button type="button" role="menuitem" data-file-action="preview" data-path="${escapeHtml(activeFile.path)}">${tablerIcon("eye")}プレビューを開く</button>` : ""}<button type="button" role="menuitem" data-file-action="download" data-path="${escapeHtml(activeFile.path)}">${tablerIcon("download")}ダウンロード</button>${canWrite ? `<button type="button" role="menuitem" data-file-action="move" data-path="${escapeHtml(activeFile.path)}">${tablerIcon("pencil")}移動・名前変更</button><button class="is-danger" type="button" role="menuitem" data-file-action="delete" data-path="${escapeHtml(activeFile.path)}">${tablerIcon("trash")}削除</button>` : ""}</div></div>` : "";
  const pages = group.tabs.map((tabId) => renderEditorPage(tabId, state.openFiles.get(filePreviewSource(tabId) || tabId), tabId === group.activePath, canWrite)).join("");
  const groupCount = editorGroups(state.editorLayout).length;
  return `<section class="editor-group${focused ? " is-focused" : ""}" data-group-id="${escapeHtml(group.id)}" aria-label="エディターグループ">
    <header class="editor-group-header"><div class="editor-tabs" role="tablist">${tabs}</div><div class="editor-group-actions">${fileActions}<button class="icon-button" type="button" data-group-split="horizontal" data-group-id="${escapeHtml(group.id)}" aria-label="右にエディターを分割" title="右に分割">${tablerIcon("layout-columns")}</button><button class="icon-button" type="button" data-group-split="vertical" data-group-id="${escapeHtml(group.id)}" aria-label="下にエディターを分割" title="下に分割">${tablerIcon("layout-rows")}</button>${groupCount > 1 ? `<button class="icon-button" type="button" data-group-close="${escapeHtml(group.id)}" aria-label="エディターグループを閉じる" title="グループを閉じる">${tablerIcon("x")}</button>` : ""}</div></header>
    <div class="editor-group-pages">${pages || `<div class="empty-editor"><div class="empty-editor-mark">${tablerIcon("layout-columns")}</div><h2>エディターを分割しました</h2><p>ファイルをここにドラッグするか、エクスプローラーから開きます。</p></div>`}</div>
  </section>`;
}

function renderEditorPage(path, file, active, canWrite) {
  const hidden = active ? "" : " hidden";
  const view = workspaceViewForTab(path);
  const previewPath = filePreviewSource(path);
  if (view) return `<div class="editor-file-page workspace-panel-page" data-file-path="${escapeHtml(path)}" data-workspace-view="${view}"${hidden}><div class="workspace-panel-content"></div></div>`;
  if (!file) return `<div class="editor-file-page editor-file-loading" data-file-path="${escapeHtml(path)}"${hidden}><span class="loading-indicator"></span><span>ファイルを開いています…</span></div>`;
  if (previewPath) return `<div class="editor-file-page preview-editor-page" data-file-path="${escapeHtml(path)}"${hidden}><section class="preview-panel"><header class="preview-heading"><span>${escapeHtml(`${file.path} プレビュー`)}</span><button class="button button-quiet" type="button" data-file-action="refresh-preview" data-path="${escapeHtml(file.path)}">更新</button></header><div class="preview-content" data-preview-content="true"></div></section></div>`;
  if (file.largeTypst) return `<div class="editor-file-page" data-file-path="${escapeHtml(path)}"${hidden}><div class="empty-editor"><div class="empty-editor-mark">${tablerIcon("file-text")}</div><h2>このTypstファイルは読み取り専用です</h2><p class="muted">24 MiBを超えるため、Monacoでの編集と共同編集はできません。ワークスペース全体が256 MiB以内ならWASMプレビューできます。</p><button class="button button-secondary" type="button" data-file-action="preview" data-path="${escapeHtml(file.path)}">${tablerIcon("eye")}プレビューを開く</button></div></div>`;
  return `<div class="editor-file-page" data-file-path="${escapeHtml(path)}"${hidden}><div class="editor-host" data-editor-host="true" aria-label="${escapeHtml(path)} の編集領域"></div></div>`;
}

function attachEditorWorkbenchEvents(target, canWrite) {
  target.querySelectorAll(".editor-tabs").forEach((tabs) => {
    tabs.addEventListener("wheel", (event) => {
      if (tabs.scrollWidth <= tabs.clientWidth) return;
      const delta = event.deltaX || event.deltaY;
      if (!delta) return;
      const multiplier = event.deltaMode === WheelEvent.DOM_DELTA_LINE
        ? 16
        : event.deltaMode === WheelEvent.DOM_DELTA_PAGE
          ? tabs.clientWidth
          : 1;
      tabs.scrollLeft += delta * multiplier;
      event.preventDefault();
    }, { passive: false });
  });
  target.querySelectorAll(".editor-group").forEach((groupNode) => {
    groupNode.addEventListener("focusin", () => focusEditorGroup(groupNode.dataset.groupId));
    groupNode.addEventListener("pointerdown", (event) => {
      if (!event.target.closest("button")) focusEditorGroup(groupNode.dataset.groupId);
    });
    groupNode.addEventListener("dragover", (event) => {
      if (!event.dataTransfer.types.includes("application/x-colabo-path")) return;
      event.preventDefault();
      groupNode.dataset.dropZone = editorDropZone(event, groupNode);
    });
    groupNode.addEventListener("dragleave", (event) => {
      if (!groupNode.contains(event.relatedTarget)) delete groupNode.dataset.dropZone;
    });
    groupNode.addEventListener("drop", (event) => handleEditorDrop(event, groupNode));
  });
  target.querySelectorAll(".editor-tab").forEach((tab) => {
    tab.addEventListener("dragstart", (event) => {
      event.dataTransfer.setData("application/x-colabo-path", tab.dataset.tabPath);
      event.dataTransfer.effectAllowed = "move";
      tab.classList.add("is-dragging");
    });
    tab.addEventListener("dragend", () => tab.classList.remove("is-dragging"));
  });
  target.querySelectorAll(".file-open").forEach((button) => {
    button.draggable = !button.closest(".file-row")?.classList.contains("is-directory");
    button.addEventListener("dragstart", (event) => {
      const path = button.closest(".file-row")?.dataset.path;
      if (!path) return;
      event.dataTransfer.setData("application/x-colabo-path", path);
      event.dataTransfer.effectAllowed = "copy";
    });
  });
  target.onclick = async (event) => {
    const menuToggle = event.target.closest("[data-file-menu-toggle]");
    if (menuToggle) {
      const menu = menuToggle.nextElementSibling;
      const willOpen = menu.hidden;
      target.querySelectorAll(".editor-file-menu").forEach((item) => { item.hidden = true; item.previousElementSibling?.setAttribute("aria-expanded", "false"); });
      menu.hidden = !willOpen;
      menuToggle.setAttribute("aria-expanded", String(willOpen));
      return;
    }
    if (!event.target.closest(".editor-file-menu")) {
      target.querySelectorAll(".editor-file-menu").forEach((item) => { item.hidden = true; item.previousElementSibling?.setAttribute("aria-expanded", "false"); });
    }
    const splitButton = event.target.closest("[data-group-split]");
    if (splitButton) return splitEditor(splitButton.dataset.groupId, splitButton.dataset.groupSplit);
    const closeGroup = event.target.closest("[data-group-close]");
    if (closeGroup) return closeEditorGroup(closeGroup.dataset.groupClose);
    const closeTab = event.target.closest("[data-tab-close]");
    if (closeTab) return closeEditorTab(closeTab.closest(".editor-tab").dataset.groupId, closeTab.closest(".editor-tab").dataset.tabPath);
    const selectTab = event.target.closest("[data-tab-select]");
    if (selectTab) {
      const tab = selectTab.closest(".editor-tab");
      return activateEditorTab(tab.dataset.groupId, tab.dataset.tabPath);
    }
    const action = event.target.closest("[data-file-action]");
    if (!action) return;
    action.closest(".editor-file-menu")?.previousElementSibling?.setAttribute("aria-expanded", "false");
    if (action.closest(".editor-file-menu")) action.closest(".editor-file-menu").hidden = true;
    const path = action.dataset.path;
    if (action.dataset.fileAction === "preview") await openFilePreview(path);
    else if (action.dataset.fileAction === "download") await downloadFile(path);
    else if (action.dataset.fileAction === "move") await moveEntry(path);
    else if (action.dataset.fileAction === "delete") await deleteEntry(path);
    else if (action.dataset.fileAction === "refresh-preview") await updatePreview(state.openFiles.get(path));
  };
  target.querySelectorAll(".editor-splitter").forEach((splitter) => {
    splitter.addEventListener("pointerdown", (event) => startEditorResize(event, splitter));
    splitter.addEventListener("keydown", (event) => resizeEditorByKeyboard(event, splitter));
  });
}

function attachEditorMounts(target) {
  for (const file of state.openFiles.values()) file.previewTarget = null;
  for (const group of editorGroups(state.editorLayout)) {
    for (const tabId of group.tabs) {
      const previewPath = filePreviewSource(tabId);
      const path = previewPath || tabId;
      const file = state.openFiles.get(path);
      const page = [...target.querySelectorAll(".editor-file-page")].find((element) => element.dataset.filePath === tabId);
      if (!file || !page) continue;
      if (previewPath) {
        file.previewTarget = page.querySelector("[data-preview-content]");
        if (file.collabInfo) updateTextPreview(file);
        else if (file.objectUrl) renderBinaryPreview(file.previewTarget, file);
        else if (file.text !== undefined) renderTextPreview(file.previewTarget, file.text, file);
        if (extension(file.path) === "typ" && (file.collabInfo || file.largeTypst)) updatePreview(file);
        continue;
      }
      file.pageElement = page;
      if (file.collabInfo) {
        const host = page.querySelector("[data-editor-host]");
        if (!file.editorMount) {
          file.editorHost = host;
          file.editorMount = window.CollabEditor.mount(host, {
            sessionId: file.collabInfo.document_id,
            actorId: file.collabInfo.actor_id,
            userId: file.collabInfo.identity.id,
            userName: file.collabInfo.identity.name,
            sessionData: { avatarUrl: state.user?.picture_url || "" },
            websocketPath: file.collabInfo.websocket_path,
            fileName: file.path,
            onConnectionChange: (connectionState, text) => {
              file.connectionStatus = { state: connectionState, text };
              if (state.activeFile === file) renderEditorStatus();
            },
            editorOptions: { readOnly: !file.collabInfo.can_write },
          });
          file.editorChange = file.editorMount.editor.onDidChangeModelContent(() => {
            file.text = file.editorMount?.editor.getValue() ?? file.text;
            if (file.previewTarget) {
              clearTimeout(file.previewTimer);
              const isTypst = extension(file.path) === "typ";
              if (isTypst) followTypstEditorPosition(file);
              file.previewTimer = setTimeout(() => isTypst ? updatePreview(file) : updateTextPreview(file), isTypst ? 500 : 180);
              if (isTypst) {
                const status = file.previewTarget.querySelector("[data-typst-status]");
                if (status) {
                  status.textContent = "更新中…";
                  status.dataset.state = "updating";
                  status.hidden = false;
                }
              }
            }
          });
          file.editorCursorChange = file.editorMount.editor.onDidChangeCursorPosition(() => {
            if (extension(file.path) === "typ") followTypstEditorPosition(file);
          });
          file.typstCursorLine = file.editorMount.editor.getPosition()?.lineNumber || 1;
        } else {
          const shell = file.editorHost?.querySelector(".collab-editor-shell");
          if (shell && shell.parentElement !== host) host.append(shell);
          file.editorHost = host;
        }
      }
    }
  }
  requestAnimationFrame(() => {
    for (const group of editorGroups(state.editorLayout)) {
      const activePath = group.activePath;
      const file = activePath ? state.openFiles.get(filePreviewSource(activePath) || activePath) : null;
      file?.editorMount?.editor.layout();
    }
  });
}

function editorLayoutChanged() {
  saveEditorLayout(state.editorLayoutKey, state.editorLayout, state.focusedGroupId);
  syncFocusedFile();
}

function syncFocusedFile() {
  const groups = editorGroups(state.editorLayout);
  let group = findEditorGroup(state.editorLayout, state.focusedGroupId);
  if (!group) {
    group = groups[0] || createEditorGroup();
    state.editorLayout = group;
    state.focusedGroupId = group.id;
  }
  state.activeTabId = group.activePath || null;
  state.view = workspaceViewForTab(group.activePath) || "files";
  state.activeFile = group.activePath && state.view === "files"
    ? state.openFiles.get(filePreviewSource(group.activePath) || group.activePath) || null
    : null;
  state.editorMount = state.activeFile?.editorMount || null;
  state.editorChange = state.activeFile?.editorChange || null;
  document.querySelectorAll(".editor-group").forEach((element) => {
    element.classList.toggle("is-focused", element.dataset.groupId === group.id);
  });
  syncActivityBar();
  renderEditorStatus();
}

function activeEditorTabLabel() {
  const view = workspaceViewForTab(state.activeTabId);
  return view ? WORKSPACE_VIEW_NAMES[view] : state.activeTabId ? editorTabName(state.activeTabId) : "ファイルを選択";
}

function syncActivityBar() {
  const activeView = activeWorkspaceView();
  document.querySelectorAll("[data-activity]").forEach((button) => {
    const active = button.dataset.activity === activeView;
    button.classList.toggle("is-active", active);
    if (active) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  });
}

function renderEditorStatus() {
  const status = document.querySelector("#editorStatusbar");
  if (!status) return;
  status.hidden = !state.workspace;
  if (!state.workspace) return;
  const connection = state.activeFile?.connectionStatus;
  const connectionStatus = connection
    ? `<span class="status-connection" data-state="${escapeHtml(connection.state)}" role="status" aria-live="polite"><span class="status-connection-dot" aria-hidden="true"></span>${escapeHtml(connection.text)}</span>`
    : "";
  status.innerHTML = `${connectionStatus}<span class="status-role">${escapeHtml(state.workspaceRole)}</span><span class="editor-status-file">${escapeHtml(activeEditorTabLabel())}</span><span>${editorGroups(state.editorLayout).length} グループ</span><span>UTF-8</span>`;
}

function newEditorGroupId() {
  return `editor-${crypto.randomUUID()}`;
}

function loadFileData(path, workspaceId = state.workspaceId) {
  return (async () => {
    const fileResponse = await request(apiPath(workspaceId, "file", { path }));
    const active = {
      path,
      mime: fileResponse.headers.get("content-type") || "application/octet-stream",
      text: undefined,
      objectUrl: "",
      previewUrl: "",
      previewTimer: 0,
      previewRequestId: 0,
      previewEnabled: isPreviewable(path),
      editorMount: null,
      editorChange: null,
      editorCursorChange: null,
      editorHost: null,
      pageElement: null,
      previewTarget: null,
    };
    const contentLength = Number(fileResponse.headers.get("content-length"));
    if (extension(path) === "typ" && Number.isFinite(contentLength) && contentLength > MAX_TEXT_FILE_BYTES) {
      active.largeTypst = true;
      await fileResponse.body?.cancel();
      return active;
    }
    const bytes = await fileResponse.arrayBuffer();
    if (isTextBytes(bytes)) {
      active.text = new TextDecoder("utf-8").decode(bytes);
      active.collabInfo = await request(apiPath(workspaceId, "collaboration", { path }));
    } else if (extension(path) === "typ" && bytes.byteLength > MAX_TEXT_FILE_BYTES) {
      active.largeTypst = true;
    } else {
      active.objectUrl = URL.createObjectURL(new Blob([bytes], { type: active.mime }));
      active.previewEnabled = true;
    }
    return active;
  })();
}

function disposeFile(file) {
  if (!file) return;
  file.typstPreviewResizeObserver?.disconnect();
  file.typstPreviewResizeObserver = null;
  clearTimeout(file.previewTimer);
  if (file.typstPreviewFollowFrame) cancelAnimationFrame(file.typstPreviewFollowFrame);
  file.typstPreviewFollowFrame = 0;
  file.editorChange?.dispose();
  file.editorChange = null;
  file.editorCursorChange?.dispose();
  file.editorCursorChange = null;
  file.editorMount?.dispose();
  file.editorMount = null;
  if (file.objectUrl) URL.revokeObjectURL(file.objectUrl);
  if (file.previewUrl) URL.revokeObjectURL(file.previewUrl);
  if (file.typstPreviewUrl) URL.revokeObjectURL(file.typstPreviewUrl);
}

function disposeEditor() {
  for (const file of state.openFiles.values()) disposeFile(file);
  state.openFiles.clear();
  state.activeFile = null;
  state.activeTabId = null;
  state.editorMount = null;
  state.editorChange = null;
}

function focusEditorGroup(groupId) {
  if (!findEditorGroup(state.editorLayout, groupId)) return;
  state.focusedGroupId = groupId;
  editorLayoutChanged();
}

async function activateEditorTab(groupId, path) {
  const group = findEditorGroup(state.editorLayout, groupId);
  if (!group || !group.tabs.includes(path)) return;
  group.activePath = path;
  state.focusedGroupId = groupId;
  const view = workspaceViewForTab(path);
  const previewPath = filePreviewSource(path);
  state.view = view || "files";
  const filePath = previewPath || path;
  if (!view && !state.openFiles.has(filePath)) {
    try { state.openFiles.set(filePath, await loadFileData(filePath)); }
    catch (error) {
      removeOpenPath(path);
      showNotice(error.message);
      return;
    }
  }
  editorLayoutChanged();
  await renderFiles(document.querySelector("#tabContent"));
  if (view === "chat") renderChatMessages();
}

async function openWorkspaceView(view) {
  if (!WORKSPACE_VIEWS.includes(view)) return;
  const tabId = workspaceViewTab(view);
  const existingGroup = editorGroupForPath(state.editorLayout, tabId);
  const group = existingGroup || findEditorGroup(state.editorLayout, state.focusedGroupId) || editorGroups(state.editorLayout)[0];
  if (!group) return;
  if (!existingGroup) group.tabs.push(tabId);
  group.activePath = tabId;
  state.focusedGroupId = group.id;
  state.view = view;
  editorLayoutChanged();
  await renderFiles(document.querySelector("#tabContent"));
  if (view === "chat") renderChatMessages();
}

function removePathFromGroups(path) {
  for (const group of editorGroups(state.editorLayout)) {
    const index = group.tabs.indexOf(path);
    if (index < 0) continue;
    group.tabs.splice(index, 1);
    if (group.activePath === path) group.activePath = group.tabs[Math.min(index, group.tabs.length - 1)] || null;
  }
}

function pruneEmptyEditorGroups() {
  for (const group of [...editorGroups(state.editorLayout)]) {
    if (editorGroups(state.editorLayout).length > 1 && !group.tabs.length) {
      state.editorLayout = removeEditorGroup(state.editorLayout, group.id);
    }
  }
  if (!findEditorGroup(state.editorLayout, state.focusedGroupId)) {
    state.focusedGroupId = editorGroups(state.editorLayout)[0]?.id || "editor-1";
  }
}

function removeOpenPath(path) {
  const previewPath = filePreviewSource(path);
  const filePath = previewPath || path;
  removePathFromGroups(path);
  if (!previewPath) removePathFromGroups(filePreviewTab(path));
  pruneEmptyEditorGroups();
  if (!editorGroupForPath(state.editorLayout, filePath) && !editorGroupForPath(state.editorLayout, filePreviewTab(filePath))) {
    disposeFile(state.openFiles.get(filePath));
    state.openFiles.delete(filePath);
  }
  editorLayoutChanged();
}

async function closeEditorTab(groupId, path) {
  const group = findEditorGroup(state.editorLayout, groupId);
  if (!group) return;
  const index = group.tabs.indexOf(path);
  if (index < 0) return;
  const previewSource = filePreviewSource(path);
  const filePath = previewSource || path;
  if (previewSource) {
    const file = state.openFiles.get(filePath);
    if (file) {
      file.previewEnabled = false;
      file.previewTarget = null;
      file.typstPreviewResizeObserver?.disconnect();
      file.typstPreviewResizeObserver = null;
      if (file.typstPreviewUrl) URL.revokeObjectURL(file.typstPreviewUrl);
      file.typstPreviewUrl = "";
    }
  } else {
    removePathFromGroups(filePreviewTab(path));
  }
  group.tabs.splice(index, 1);
  if (group.activePath === path) group.activePath = group.tabs[Math.min(index, group.tabs.length - 1)] || null;
  pruneEmptyEditorGroups();
  if (!editorGroupForPath(state.editorLayout, filePath) && !editorGroupForPath(state.editorLayout, filePreviewTab(filePath))) {
    disposeFile(state.openFiles.get(filePath));
    state.openFiles.delete(filePath);
  }
  state.focusedGroupId = findEditorGroup(state.editorLayout, groupId)?.id || editorGroups(state.editorLayout)[0]?.id;
  editorLayoutChanged();
  await renderFiles(document.querySelector("#tabContent"));
}

async function splitEditor(groupId, direction, side = direction === "horizontal" ? "right" : "bottom", path = null) {
  if (editorGroups(state.editorLayout).length >= 8) {
    showNotice("エディターグループは8つまで作成できます。");
    return;
  }
  const group = findEditorGroup(state.editorLayout, groupId);
  if (!group) return;
  const nextGroup = createEditorGroup(newEditorGroupId());
  if (path) {
    nextGroup.tabs.push(path);
    nextGroup.activePath = path;
  }
  const nextLayout = splitEditorGroup(state.editorLayout, groupId, direction, side, nextGroup);
  if (nextLayout === state.editorLayout) return;
  state.editorLayout = nextLayout;
  state.focusedGroupId = nextGroup.id;
  const previewPath = filePreviewSource(path);
  const filePath = previewPath || path;
  if (path && !workspaceViewForTab(path) && !state.openFiles.has(filePath)) {
    try { state.openFiles.set(filePath, await loadFileData(filePath)); }
    catch (error) {
      removeOpenPath(path);
      showNotice(error.message);
      return;
    }
  }
  editorLayoutChanged();
  await renderFiles(document.querySelector("#tabContent"));
}

async function moveEditorPath(path, targetGroupId, zone = "center", anchorPath = null, after = false) {
  const targetGroup = findEditorGroup(state.editorLayout, targetGroupId);
  if (!targetGroup) return;
  const previewPath = filePreviewSource(path);
  const filePath = previewPath || path;
  if (!workspaceViewForTab(path) && !state.openFiles.has(filePath)) {
    try { state.openFiles.set(filePath, await loadFileData(filePath)); }
    catch (error) { showNotice(error.message); return; }
  }
  const sourceGroup = editorGroupForPath(state.editorLayout, path);
  if (zone !== "center") {
    if (sourceGroup) removePathFromGroups(path);
    const direction = zone === "left" || zone === "right" ? "horizontal" : "vertical";
    const nextGroup = createEditorGroup(newEditorGroupId());
    nextGroup.tabs.push(path);
    nextGroup.activePath = path;
    const nextLayout = splitEditorGroup(state.editorLayout, targetGroupId, direction, zone, nextGroup);
    if (nextLayout === state.editorLayout) return;
    state.editorLayout = nextLayout;
    state.focusedGroupId = nextGroup.id;
  } else {
    if (sourceGroup?.id === targetGroupId && anchorPath === path) return;
    if (sourceGroup) removePathFromGroups(path);
    const destination = findEditorGroup(state.editorLayout, targetGroupId);
    if (!destination) return;
    const index = anchorPath ? destination.tabs.indexOf(anchorPath) : -1;
    destination.tabs.splice(index < 0 ? destination.tabs.length : index + (after ? 1 : 0), 0, path);
    destination.activePath = path;
    state.focusedGroupId = destination.id;
  }
  state.view = workspaceViewForTab(path) || "files";
  editorLayoutChanged();
  await renderFiles(document.querySelector("#tabContent"));
}

function editorDropZone(event, groupNode) {
  if (event.target.closest(".editor-tab")) return "center";
  const rect = groupNode.getBoundingClientRect();
  const x = (event.clientX - rect.left) / Math.max(rect.width, 1);
  const y = (event.clientY - rect.top) / Math.max(rect.height, 1);
  const distances = [
    { side: "left", distance: x, direction: "horizontal" },
    { side: "right", distance: 1 - x, direction: "horizontal" },
    { side: "top", distance: y, direction: "vertical" },
    { side: "bottom", distance: 1 - y, direction: "vertical" },
  ].sort((a, b) => a.distance - b.distance);
  return distances[0].distance < 0.22 ? distances[0].side : "center";
}

async function handleEditorDrop(event, groupNode) {
  const path = event.dataTransfer.getData("application/x-colabo-path");
  if (!path) return;
  event.preventDefault();
  delete groupNode.dataset.dropZone;
  const groupId = groupNode.dataset.groupId;
  const tab = event.target.closest(".editor-tab");
  if (tab) {
    const anchorPath = tab.dataset.tabPath;
    const after = event.clientX > tab.getBoundingClientRect().left + tab.getBoundingClientRect().width / 2;
    await moveEditorPath(path, groupId, "center", anchorPath, after);
    return;
  }
  const zone = editorDropZone(event, groupNode);
  await moveEditorPath(path, groupId, zone);
}

async function closeEditorGroup(groupId) {
  const group = findEditorGroup(state.editorLayout, groupId);
  const groups = editorGroups(state.editorLayout);
  if (!group || groups.length <= 1) return;
  const groupIndex = groups.findIndex((item) => item.id === groupId);
  const destination = groups[groupIndex + 1] || groups[groupIndex - 1];
  const movedTabs = [...group.tabs];
  const nextLayout = removeEditorGroup(state.editorLayout, groupId);
  const remaining = findEditorGroup(nextLayout, destination.id);
  for (const path of movedTabs) if (!remaining.tabs.includes(path)) remaining.tabs.push(path);
  if (group.activePath && movedTabs.includes(group.activePath)) remaining.activePath = group.activePath;
  state.editorLayout = nextLayout;
  state.focusedGroupId = remaining.id;
  editorLayoutChanged();
  await renderFiles(document.querySelector("#tabContent"));
}

function findSplitNode(layout, splitId) {
  if (layout?.type !== "split") return null;
  if (layout.id === splitId) return layout;
  return findSplitNode(layout.children[0], splitId) || findSplitNode(layout.children[1], splitId);
}

function startEditorResize(event, splitter) {
  if (event.button !== 0) return;
  const splitElement = splitter.closest(".editor-split");
  const splitNode = findSplitNode(state.editorLayout, splitter.dataset.splitId);
  if (!splitElement || !splitNode) return;
  event.preventDefault();
  const rect = splitElement.getBoundingClientRect();
  const update = (pointerEvent) => {
    const position = splitNode.direction === "horizontal"
      ? (pointerEvent.clientX - rect.left) / rect.width
      : (pointerEvent.clientY - rect.top) / rect.height;
    splitNode.ratio = Math.min(0.8, Math.max(0.2, position));
    splitElement.style.setProperty("--split-ratio", `${splitNode.ratio * 100}%`);
  };
  const finish = () => {
    document.removeEventListener("pointermove", update);
    document.body.classList.remove("is-resizing-editor");
    editorLayoutChanged();
  };
  document.body.classList.add("is-resizing-editor");
  document.addEventListener("pointermove", update);
  document.addEventListener("pointerup", finish, { once: true });
}

function resizeEditorByKeyboard(event, splitter) {
  const splitNode = findSplitNode(state.editorLayout, splitter.dataset.splitId);
  if (!splitNode) return;
  const negative = splitNode.direction === "horizontal" ? "ArrowLeft" : "ArrowUp";
  const positive = splitNode.direction === "horizontal" ? "ArrowRight" : "ArrowDown";
  if (event.key !== negative && event.key !== positive) return;
  event.preventDefault();
  splitNode.ratio = Math.min(0.8, Math.max(0.2, splitNode.ratio + (event.key === positive ? 0.04 : -0.04)));
  splitter.closest(".editor-split").style.setProperty("--split-ratio", `${splitNode.ratio * 100}%`);
  editorLayoutChanged();
}

async function showQuickOpen() {
  try {
    const paths = new Set();
    const folders = [""];
    const visited = new Set();
    while (folders.length && paths.size < 1000 && visited.size < 250) {
      const folder = folders.pop();
      if (visited.has(folder)) continue;
      visited.add(folder);
      const listing = folder === state.currentDir
        ? state.entries
        : await request(apiPath(state.workspaceId, "files", { path: folder }));
      for (const entry of listing) {
        if (entry.directory) {
          if (!visited.has(entry.path) && folders.length < 250) folders.push(entry.path);
        } else {
          paths.add(entry.path);
          if (paths.size >= 1000) break;
        }
      }
    }
    for (const group of editorGroups(state.editorLayout)) {
      group.tabs.forEach((tabId) => { if (!workspaceViewForTab(tabId) && !filePreviewSource(tabId)) paths.add(tabId); });
    }
    const selected = await quickOpenDialog([...paths].sort((a, b) => a.localeCompare(b)));
    if (selected) await openFile(selected);
  } catch (error) {
    showNotice(error.message);
  }
}

function renderFileRow(entry, canWrite) {
  const item = document.createElement("li");
  item.className = `file-row${entry.directory ? " is-directory" : ""}`;
  item.dataset.path = entry.path;
  const openGroup = entry.directory ? null : editorGroupForPath(state.editorLayout, entry.path);
  if (openGroup) item.classList.add(openGroup.activePath === entry.path ? "is-open-active" : "is-open");
  const open = document.createElement("button");
  open.className = "file-open";
  open.type = "button";
  if (openGroup?.activePath === entry.path) open.setAttribute("aria-current", "page");
  open.innerHTML = `<span class="file-icon" aria-hidden="true">${entry.directory ? tablerIcon("folder") : fileGlyph(entry.name)}</span><span class="file-name">${escapeHtml(entry.name)}</span><span class="file-size">${entry.directory ? "" : formatSize(entry.size)}</span>`;
  open.addEventListener("click", () => entry.directory ? navigateDirectory(entry.path) : openFile(entry.path));
  item.append(open);
  if (canWrite) {
    const actions = document.createElement("div");
    actions.className = "file-row-actions";
    const rename = document.createElement("button");
    rename.className = "icon-button";
    rename.type = "button";
    rename.innerHTML = tablerIcon("pencil");
    rename.title = "移動または名前変更";
    rename.setAttribute("aria-label", `${entry.name} を移動または名前変更`);
    rename.addEventListener("click", () => moveEntry(entry.path));
    const remove = document.createElement("button");
    remove.className = "icon-button";
    remove.type = "button";
    remove.innerHTML = tablerIcon("trash");
    remove.title = "削除";
    remove.setAttribute("aria-label", `${entry.name} を削除`);
    remove.addEventListener("click", () => deleteEntry(entry.path));
    actions.append(rename, remove);
    item.append(actions);
  }
  return item;
}

function renderBreadcrumbs(container) {
  const root = document.createElement("button");
  root.type = "button";
  root.textContent = "ファイル";
  root.addEventListener("click", () => navigateDirectory(""));
  container.append(root);
  let path = "";
  for (const segment of state.currentDir.split("/").filter(Boolean)) {
    path = path ? `${path}/${segment}` : segment;
    const crumbPath = path;
    const crumb = document.createElement("button");
    crumb.type = "button";
    crumb.textContent = segment;
    crumb.addEventListener("click", () => navigateDirectory(crumbPath));
    container.append(crumb);
  }
}

async function navigateDirectory(path) {
  state.currentDir = path;
  await refreshFiles();
}

async function newFile() {
  const name = await inputDialog({ title: "新しいファイル", label: "ファイル名", placeholder: "例: README.md", confirmLabel: "作成" });
  if (!name) return;
  const path = state.currentDir ? `${state.currentDir}/${name}` : name;
  try {
    await request(apiPath(state.workspaceId, "file", { path }), { method: "PUT", body: new Blob([""]) });
    await refreshFiles();
    await openFile(path);
  } catch (error) { showNotice(error.message); }
}

async function newFolder() {
  const name = await inputDialog({ title: "新しいフォルダー", label: "フォルダー名", placeholder: "フォルダー名", confirmLabel: "作成" });
  if (!name) return;
  const path = state.currentDir ? `${state.currentDir}/${name}` : name;
  try {
    await request(apiPath(state.workspaceId, "directory"), { method: "POST", body: { path } });
    await refreshFiles();
  } catch (error) { showNotice(error.message); }
}

async function uploadFiles(event) {
  const selected = [...event.target.files || []];
  if (!selected.length) return;
  const form = new FormData();
  form.append("folder", state.currentDir);
  selected.forEach((file) => form.append("files", file, file.name));
  try {
    await request(apiPath(state.workspaceId, "upload"), { method: "POST", body: form });
    await refreshFiles();
  } catch (error) { showNotice(error.message); }
  event.target.value = "";
}

async function moveEntry(source) {
  const destination = await inputDialog({ title: "ファイルを移動・名前変更", message: "ワークスペース内の移動先を入力してください。", label: "パス", value: source, confirmLabel: "移動" });
  if (!destination || destination === source) return;
  try {
    await request(apiPath(state.workspaceId, "move"), { method: "POST", body: { source, destination } });
    renameOpenPaths(source, destination);
    await refreshFiles();
    await renderFiles(document.querySelector("#tabContent"));
  } catch (error) { showNotice(error.message); }
}

async function deleteEntry(path) {
  if (!await confirmDialog({ title: "ファイルを削除", message: `「${path}」を削除します。この操作は元に戻せません。`, confirmLabel: "削除", danger: true })) return;
  try {
    await request(apiPath(state.workspaceId, "file", { path }), { method: "DELETE" });
    removeOpenPaths(path);
    await refreshFiles();
    await renderFiles(document.querySelector("#tabContent"));
  } catch (error) { showNotice(error.message); }
}

async function refreshFiles() {
  if (!state.workspace) return;
  const target = document.querySelector("#tabContent");
  if (!target) return;
  const listing = await request(apiPath(state.workspaceId, "files", { path: state.currentDir }));
  await renderFiles(target, listing);
}

function renameOpenPaths(source, destination) {
  const replacements = new Map();
  const fileReplacements = new Map();
  for (const group of editorGroups(state.editorLayout)) {
    group.tabs = group.tabs.map((path) => {
      const previewSource = filePreviewSource(path);
      const filePath = previewSource || path;
      if (filePath !== source && !filePath.startsWith(`${source}/`)) return path;
      const nextFilePath = `${destination}${filePath.slice(source.length)}`;
      const next = previewSource ? filePreviewTab(nextFilePath) : nextFilePath;
      replacements.set(path, next);
      fileReplacements.set(filePath, nextFilePath);
      return next;
    });
    if (group.activePath && replacements.has(group.activePath)) group.activePath = replacements.get(group.activePath);
  }
  for (const [oldPath, newPath] of fileReplacements) {
    const file = state.openFiles.get(oldPath);
    if (!file) continue;
    file.path = newPath;
    state.openFiles.delete(oldPath);
    state.openFiles.set(newPath, file);
  }
  editorLayoutChanged();
}

function removeOpenPaths(path) {
  const removed = new Set();
  for (const group of editorGroups(state.editorLayout)) {
    for (const candidate of group.tabs) {
      const filePath = filePreviewSource(candidate) || candidate;
      if (filePath === path || filePath.startsWith(`${path}/`)) removed.add(candidate);
    }
    const activeIndex = group.tabs.indexOf(group.activePath);
    group.tabs = group.tabs.filter((candidate) => !removed.has(candidate));
    if (removed.has(group.activePath)) group.activePath = group.tabs[Math.min(Math.max(activeIndex, 0), group.tabs.length - 1)] || null;
  }
  const removedFiles = new Set();
  for (const candidate of removed) {
    const filePath = filePreviewSource(candidate) || candidate;
    if (removedFiles.has(filePath)) continue;
    removedFiles.add(filePath);
    disposeFile(state.openFiles.get(filePath));
    state.openFiles.delete(filePath);
  }
  pruneEmptyEditorGroups();
  editorLayoutChanged();
}

function extension(path) {
  const name = path.split("/").pop();
  return name.includes(".") ? name.split(".").pop().toLowerCase() : "";
}

function isTextBytes(bytes) {
  if (bytes.byteLength > MAX_TEXT_FILE_BYTES) return false;
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return !text.includes("\0");
  } catch (_) { return false; }
}

async function openFile(path, shouldRender = true) {
  try {
    let group = editorGroupForPath(state.editorLayout, path);
    if (!group) group = findEditorGroup(state.editorLayout, state.focusedGroupId) || editorGroups(state.editorLayout)[0];
    if (!group) {
      group = createEditorGroup(newEditorGroupId());
      state.editorLayout = group;
    }
    if (!state.openFiles.has(path)) state.openFiles.set(path, await loadFileData(path));
    if (!group.tabs.includes(path)) group.tabs.push(path);
    group.activePath = path;
    state.focusedGroupId = group.id;
    state.view = "files";
    editorLayoutChanged();
    if (shouldRender) await renderFiles(document.querySelector("#tabContent"));
  } catch (error) {
    showNotice(error.message);
  }
}

function isPreviewable(path) {
  return ["typ", "md", "markdown", "csv", "txt", "html", "htm", "png", "jpg", "jpeg", "webp", "gif", "svg", "pdf", "mp3", "wav", "ogg", "mp4", "webm"].includes(extension(path));
}

function ensureFilePreview(path, sourceGroupId = null) {
  const file = state.openFiles.get(path);
  if (!file?.previewEnabled) return false;
  const previewTab = filePreviewTab(path);
  const sourceGroup = editorGroupForPath(state.editorLayout, path)
    || findEditorGroup(state.editorLayout, sourceGroupId)
    || editorGroups(state.editorLayout)[0];
  if (!sourceGroup) return false;
  let previewGroup = editorGroupForPath(state.editorLayout, previewTab);
  let changed = false;
  if (previewGroup?.id === sourceGroup.id) {
    removePathFromGroups(previewTab);
    sourceGroup.activePath = path;
    previewGroup = null;
    changed = true;
  }
  if (!previewGroup) {
    if (editorGroups(state.editorLayout).length >= 8) {
      file.previewEnabled = false;
      return false;
    }
    previewGroup = createEditorGroup(newEditorGroupId());
    previewGroup.tabs.push(previewTab);
    previewGroup.activePath = previewTab;
    const nextLayout = splitEditorGroup(state.editorLayout, sourceGroup.id, "horizontal", "right", previewGroup);
    if (nextLayout === state.editorLayout) return false;
    state.editorLayout = nextLayout;
    changed = true;
  } else {
    if (!previewGroup.tabs.includes(previewTab)) previewGroup.tabs.push(previewTab);
    if (previewGroup.activePath !== previewTab) {
      previewGroup.activePath = previewTab;
      changed = true;
    }
  }
  if (changed) editorLayoutChanged();
  return true;
}

async function openFilePreview(path) {
  const file = state.openFiles.get(path);
  const sourceGroup = editorGroupForPath(state.editorLayout, path);
  if (!file || !sourceGroup || !isPreviewable(path)) return;
  file.previewEnabled = true;
  ensureFilePreview(path, sourceGroup.id);
  await renderFiles(document.querySelector("#tabContent"));
}

function updateTextPreview(file = state.activeFile) {
  if (!file?.previewTarget) return;
  renderTextPreview(file.previewTarget, file.editorMount?.editor.getValue() ?? file.text ?? "", file);
}

function followTypstEditorPosition(file) {
  const editor = file?.editorMount?.editor;
  const line = editor?.getPosition()?.lineNumber;
  if (Number.isFinite(line)) file.typstCursorLine = line;
  if (!file || file.typstPreviewFollowFrame) return;
  file.typstPreviewFollowFrame = requestAnimationFrame(() => {
    file.typstPreviewFollowFrame = 0;
    const viewport = file.previewTarget?.querySelector(".typst-preview-scroll");
    const model = file.editorMount?.editor.getModel();
    if (!viewport || !model || viewport.clientHeight <= 0 || viewport.clientWidth <= 0) return;
    const image = viewport.querySelector(".typst-svg-render");
    if (!image) return;
    const viewportRect = viewport.getBoundingClientRect();
    const imageRect = image.getBoundingClientRect();
    if (imageRect.height <= 0 || imageRect.width <= 0) return;
    const lineCount = model.getLineCount();
    const progress = lineCount <= 1 ? 0 : (Math.max(1, Math.min(lineCount, file.typstCursorLine || 1)) - 1) / (lineCount - 1);
    const sourcePointY = imageRect.top + imageRect.height * progress;
    const margin = Math.min(96, viewport.clientHeight * 0.15);
    const hysteresis = Math.min(margin, viewport.clientHeight * 0.1);
    const safeTop = viewportRect.top + margin;
    const safeBottom = viewportRect.bottom - margin;
    let nextScrollTop = viewport.scrollTop;
    if (sourcePointY < safeTop) nextScrollTop -= safeTop - sourcePointY + hysteresis;
    else if (sourcePointY > safeBottom) nextScrollTop += sourcePointY - safeBottom + hysteresis;
    viewport.scrollTop = Math.max(0, Math.min(viewport.scrollHeight - viewport.clientHeight, nextScrollTop));
  });
}

function renderTextPreview(target, text, file) {
  if (!target || !file) return;
  target.classList.remove("typst-preview");
  const type = extension(file.path);
  if (["md", "markdown"].includes(type)) {
    const html = marked.parse(text);
    target.innerHTML = DOMPurify.sanitize(html, { USE_PROFILES: { html: true } });
  } else if (type === "csv") {
    renderCsv(target, text);
  } else if (["html", "htm"].includes(type)) {
    const frame = document.createElement("iframe");
    frame.className = "html-preview-frame";
    frame.title = "隔離された HTML プレビュー";
    frame.setAttribute("sandbox", "");
    frame.srcdoc = text;
    target.replaceChildren(frame);
  } else if (type === "typ") {
    target.classList.add("typst-preview");
    target.innerHTML = `<div class="preview-placeholder"><p>Typst SVG プレビュー</p><p class="muted">ワークスペース内のファイルを読み込み、編集内容を自動で反映します。</p><p class="typst-stale-note muted" hidden>ソースを変更しました。組版しています…</p><button class="button button-secondary" data-compile-typst>${tablerIcon("refresh")}プレビューを更新</button></div>`;
    target.querySelector("[data-compile-typst]").addEventListener("click", () => updatePreview(file));
  } else {
    const pre = document.createElement("pre");
    pre.className = "text-preview";
    pre.textContent = text;
    target.replaceChildren(pre);
  }
}

function renderBinaryPreview(target, file) {
  if (!target || !file) return;
  const { path, mime, objectUrl } = file;
  const ext = extension(path);
  if (["png", "jpg", "jpeg", "webp", "gif", "svg"].includes(ext)) {
    const image = document.createElement("img");
    image.className = "media-preview-image";
    image.alt = path.split("/").pop();
    image.src = objectUrl;
    target.replaceChildren(image);
  } else if (["mp3", "wav", "ogg"].includes(ext)) {
    const audio = document.createElement("audio");
    audio.controls = true;
    audio.src = objectUrl;
    target.replaceChildren(audio);
  } else if (["mp4", "webm"].includes(ext)) {
    const video = document.createElement("video");
    video.controls = true;
    video.src = objectUrl;
    target.replaceChildren(video);
  } else if (ext === "pdf") {
    const frame = document.createElement("iframe");
    frame.className = "pdf-preview-frame";
    frame.title = "PDF プレビュー";
    frame.src = objectUrl;
    target.replaceChildren(frame);
  } else {
    target.innerHTML = `<div class="preview-placeholder"><p>${escapeHtml(mime)}</p><p class="muted">この形式はプレビューできません。</p><button class="button button-secondary" data-download-binary>ダウンロード</button></div>`;
    target.querySelector("[data-download-binary]").addEventListener("click", () => downloadFile(path));
  }
}

async function updatePreview(file = state.activeFile) {
  const target = file?.previewTarget;
  if (!target || !file) return;
  if (extension(file.path) !== "typ") {
    if (file.collabInfo || file.text !== undefined) updateTextPreview(file);
    else renderBinaryPreview(target, file);
    return;
  }
  const workspaceId = state.workspaceId;
  const requestId = (file.previewRequestId || 0) + 1;
  file.previewRequestId = requestId;
  let pendingPreviewUrl = "";
  const source = file.largeTypst ? undefined : file.editorMount?.editor.getValue() ?? file.text ?? "";
  const openSources = [...state.openFiles.values()]
    .map((openFile) => ({ path: openFile.path, source: openFile.editorMount?.editor.getValue() ?? openFile.text }))
    .filter((openFile) => typeof openFile.source === "string");
  target.classList.add("typst-preview");
  const status = target.querySelector("[data-typst-status]");
  if (status) {
    status.textContent = "更新中…";
    status.dataset.state = "updating";
    status.removeAttribute("title");
    status.hidden = false;
  } else if (!target.querySelector(".typst-preview-scroll")) {
    target.innerHTML = `<p class="preview-loading" role="status">Typst WASM で SVG を組版しています…</p>`;
  }
  try {
    const { renderTypstWorkspace } = await loadTypstPreviewModule();
    const result = await renderTypstWorkspace({
      workspaceId,
      path: file.path,
      source,
      openSources,
    });
    if (result === null) return;
    if (workspaceId !== state.workspaceId || file.previewRequestId !== requestId || file.previewTarget !== target) return;
    pendingPreviewUrl = URL.createObjectURL(new Blob([result.svg], { type: "image/svg+xml" }));
    const image = new Image();
    image.className = "typst-svg-render";
    image.alt = `${file.path} の Typst SVG プレビュー`;
    image.src = pendingPreviewUrl;
    await image.decode();
    if (workspaceId !== state.workspaceId || file.previewRequestId !== requestId || file.previewTarget !== target) {
      URL.revokeObjectURL(pendingPreviewUrl);
      pendingPreviewUrl = "";
      return;
    }
    const contents = [];
    if (result.warnings.length) {
      const warning = document.createElement("p");
      warning.className = "typst-font-warning";
      warning.setAttribute("role", "status");
      warning.textContent = `Typst プレビューの警告: ${result.warnings.join(" / ")}`;
      contents.push(warning);
    }
    const previousPreviewUrl = file.typstPreviewUrl;
    renderTypstSvgPreview(target, file, image, contents);
    file.typstPreviewUrl = pendingPreviewUrl;
    pendingPreviewUrl = "";
    if (previousPreviewUrl) URL.revokeObjectURL(previousPreviewUrl);
  } catch (error) {
    if (pendingPreviewUrl) URL.revokeObjectURL(pendingPreviewUrl);
    if (file.previewRequestId !== requestId || file.previewTarget !== target) return;
    const message = error && typeof error.message === "string"
      ? error.message
      : typeof error === "string" ? error : "Typst のプレビューに失敗しました。ブラウザーのコンソールを確認してください。";
    const status = target.querySelector("[data-typst-status]");
    if (target.querySelector(".typst-preview-scroll") && status) {
      status.textContent = "更新に失敗";
      status.dataset.state = "error";
      status.title = message;
      status.hidden = false;
    } else {
      target.innerHTML = `<div class="preview-placeholder"><p class="notice notice-error" role="alert">${escapeHtml(message)}</p><button class="button button-secondary" data-compile-typst>${tablerIcon("refresh")}再試行</button></div>`;
      target.querySelector("[data-compile-typst]").addEventListener("click", () => updatePreview(file));
    }
  }
}

function renderTypstSvgPreview(target, file, image, warnings) {
  target.classList.add("typst-preview");

  const toolbar = document.createElement("div");
  toolbar.className = "typst-preview-toolbar";
  toolbar.setAttribute("role", "toolbar");
  toolbar.setAttribute("aria-label", "SVG プレビューの表示倍率");
  toolbar.innerHTML = `<button class="icon-button" type="button" data-typst-zoom="out" aria-label="縮小" title="縮小 (Ctrl+ホイール)" disabled>${tablerIcon("minus")}</button><button class="typst-preview-zoom-value" type="button" data-typst-zoom="actual" title="実際のサイズに戻す">100%</button><button class="icon-button" type="button" data-typst-zoom="in" aria-label="拡大" title="拡大 (Ctrl+ホイール)" disabled>${tablerIcon("plus")}</button><span class="typst-preview-dimensions" data-typst-dimensions></span><span class="typst-preview-status" data-typst-status role="status" aria-live="polite" hidden></span><span class="typst-preview-toolbar-spacer"></span><button class="button button-quiet" type="button" data-typst-zoom="fit" disabled>幅に合わせる</button>`;

  const viewport = document.createElement("div");
  viewport.className = "typst-preview-scroll";
  viewport.setAttribute("aria-label", "SVG プレビュー。通常のスクロールで移動、Ctrl キーとホイールで拡大縮小できます。");
  const canvas = document.createElement("div");
  canvas.className = "typst-preview-canvas";
  image.classList.add("typst-svg-render");
  image.alt = `${file.path} の Typst SVG プレビュー`;
  image.removeAttribute("style");
  const intrinsicWidth = image.naturalWidth;
  const intrinsicHeight = image.naturalHeight;
  const previewMode = file.typstZoomMode || "actual";
  let initialScale = file.typstZoomScale || 1;
  if (previewMode === "fit" && intrinsicWidth) {
    const targetStyle = getComputedStyle(target);
    const availableWidth = target.clientWidth - parseFloat(targetStyle.paddingLeft) - parseFloat(targetStyle.paddingRight);
    if (availableWidth > 0) initialScale = Math.min(8, availableWidth / intrinsicWidth);
  }
  file.typstZoomScale = Math.max(0.05, Math.min(8, initialScale));
  image.style.width = `${intrinsicWidth * file.typstZoomScale}px`;
  image.style.height = `${intrinsicHeight * file.typstZoomScale}px`;
  toolbar.querySelector("[data-typst-zoom=actual]").textContent = `${Math.round(file.typstZoomScale * 100)}%`;
  toolbar.querySelector("[data-typst-dimensions]").textContent = `${intrinsicWidth} × ${intrinsicHeight} px`;
  toolbar.querySelectorAll("[data-typst-zoom]").forEach((button) => { button.disabled = false; });
  canvas.append(image);
  viewport.append(canvas);

  const zoomValue = toolbar.querySelector("[data-typst-zoom=actual]");
  const dimensions = toolbar.querySelector("[data-typst-dimensions]");
  const zoomButtons = toolbar.querySelectorAll("[data-typst-zoom]");
  file.typstPreviewResizeObserver?.disconnect();

  function updateZoom(nextScale, preserveCenter, mode = "custom") {
    if (!Number.isFinite(nextScale) || nextScale <= 0) return;
    const oldScale = file.typstZoomScale || 1;
    const centerX = viewport.scrollLeft + viewport.clientWidth / 2;
    const centerY = viewport.scrollTop + viewport.clientHeight / 2;
    file.typstZoomScale = nextScale;
    file.typstZoomMode = mode;
    zoomValue.textContent = `${Math.round(nextScale * 100)}%`;
    if (!intrinsicWidth || !intrinsicHeight) return;
    image.style.width = `${intrinsicWidth * nextScale}px`;
    image.style.height = `${intrinsicHeight * nextScale}px`;
    if (preserveCenter) {
      requestAnimationFrame(() => {
        const ratio = nextScale / oldScale;
        viewport.scrollLeft = centerX * ratio - viewport.clientWidth / 2;
        viewport.scrollTop = centerY * ratio - viewport.clientHeight / 2;
      });
    }
  }

  function fitToWidth() {
    const style = getComputedStyle(viewport);
    const availableWidth = viewport.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
    if (!intrinsicWidth || availableWidth <= 0) return;
    updateZoom(Math.min(8, availableWidth / intrinsicWidth), false, "fit");
  }

  file.typstPreviewResizeObserver = new ResizeObserver(() => {
    if (file.typstZoomMode === "fit") fitToWidth();
  });
  file.typstPreviewResizeObserver.observe(viewport);

  toolbar.addEventListener("click", (event) => {
    const button = event.target.closest("[data-typst-zoom]");
    if (!button || button.disabled) return;
    const action = button.dataset.typstZoom;
    if (action === "fit") fitToWidth();
    else if (action === "actual") updateZoom(1, false, "actual");
    else {
      const factor = action === "in" ? 1.2 : 1 / 1.2;
      updateZoom(Math.max(0.05, Math.min(8, (file.typstZoomScale || 1) * factor)), true);
    }
  });
  viewport.addEventListener("wheel", (event) => {
    if (!event.ctrlKey || !intrinsicWidth) return;
    event.preventDefault();
    const factor = Math.exp(-event.deltaY * 0.002);
    updateZoom(Math.max(0.05, Math.min(8, (file.typstZoomScale || 1) * factor)), true);
  }, { passive: false });
  const previousViewport = target.querySelector(".typst-preview-scroll");
  const previousScrollLeft = previousViewport
    ? (previousViewport.scrollWidth > previousViewport.clientWidth
      ? previousViewport.scrollLeft / (previousViewport.scrollWidth - previousViewport.clientWidth)
      : 0)
    : 0;
  const previousScrollTop = previousViewport
    ? (previousViewport.scrollHeight > previousViewport.clientHeight
      ? previousViewport.scrollTop / (previousViewport.scrollHeight - previousViewport.clientHeight)
      : 0)
    : 0;
  target.replaceChildren(...warnings, toolbar, viewport);
  viewport.scrollLeft = previousScrollLeft * Math.max(0, viewport.scrollWidth - viewport.clientWidth);
  viewport.scrollTop = previousScrollTop * Math.max(0, viewport.scrollHeight - viewport.clientHeight);
  followTypstEditorPosition(file);
}

function renderCsv(target, text) {
  const rows = parseCsv(text).slice(0, 1000);
  const table = document.createElement("table");
  table.className = "csv-table";
  const body = document.createElement("tbody");
  rows.forEach((row, rowIndex) => {
    const tr = document.createElement("tr");
    row.slice(0, 100).forEach((value) => {
      const cell = document.createElement(rowIndex === 0 ? "th" : "td");
      cell.textContent = value;
      tr.append(cell);
    });
    body.append(tr);
  });
  table.append(body);
  target.replaceChildren(table);
}

function parseCsv(text) {
  const rows = [];
  let row = [];
  let cell = "";
  let quoted = false;
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (quoted && character === '"' && text[index + 1] === '"') { cell += '"'; index += 1; }
    else if (character === '"') quoted = !quoted;
    else if (!quoted && character === ",") { row.push(cell); cell = ""; }
    else if (!quoted && (character === "\n" || character === "\r")) {
      if (character === "\r" && text[index + 1] === "\n") index += 1;
      row.push(cell); rows.push(row); row = []; cell = "";
    } else cell += character;
  }
  if (cell || row.length) { row.push(cell); rows.push(row); }
  return rows;
}

async function downloadFile(path) {
  try {
    const response = await request(apiPath(state.workspaceId, "download", { path }));
    const blob = await response.blob();
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = path.split("/").pop();
    anchor.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  } catch (error) { showNotice(error.message); }
}

async function renderChat(target) {
  target.innerHTML = `<section class="chat-view"><header class="section-heading"><div><p class="eyebrow">WORKSPACE CHAT</p><h2>会話</h2></div><span class="muted">メンバーと共有されます</span></header><div class="chat-history" id="chatHistory" role="log" aria-live="polite"></div><form class="chat-compose" id="chatForm"><label class="visually-hidden" for="chatInput">メッセージ</label><textarea id="chatInput" rows="2" maxlength="4000" placeholder="メッセージを書く"></textarea><button class="button button-primary" type="submit">送信</button></form></section>`;
  const history = await request(apiPath(state.workspaceId, "chat", { limit: 100 }));
  state.chatMessages = history;
  await loadMemberNames();
  renderChatMessages();
  target.querySelector("#chatForm").addEventListener("submit", sendChatMessage);
}

async function loadMemberNames() {
  try {
    const members = await request(apiPath(state.workspaceId, "members"));
    state.memberProfiles = new Map(members.map((member) => [member.user.id, member.user]));
  } catch (_) {}
}

function renderChatMessages() {
  const history = document.querySelector("#chatHistory");
  if (!history) return;
  const wasNearBottom = history.scrollHeight - history.scrollTop - history.clientHeight < 80;
  history.replaceChildren();
  if (!state.chatMessages.length) history.innerHTML = `<p class="empty-state">会話はまだありません。</p>`;
  state.chatMessages.forEach((message) => {
    const article = document.createElement("article");
    article.className = `chat-message${message.user_id === state.user.id ? " is-own" : ""}`;
    const header = document.createElement("div");
    header.className = "chat-message-meta";
    const profile = state.memberProfiles.get(message.user_id) || { display_name: "メンバー" };
    const avatar = document.createElement("span");
    avatar.className = "user-avatar chat-avatar";
    avatar.setAttribute("aria-hidden", "true");
    avatar.innerHTML = avatarContent(profile);
    const name = document.createElement("strong");
    name.textContent = userName(profile);
    const time = document.createElement("time");
    time.dateTime = message.sent_at;
    time.textContent = new Date(message.sent_at).toLocaleString("ja-JP", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
    const heading = document.createElement("div");
    heading.className = "chat-message-heading";
    heading.append(name, time);
    const body = document.createElement("p");
    body.textContent = message.body;
    header.append(avatar, heading);
    bindAvatarImages(avatar);
    article.append(header, body);
    history.append(article);
  });
  if (wasNearBottom) history.scrollTop = history.scrollHeight;
}

async function sendChatMessage(event) {
  event.preventDefault();
  const input = document.querySelector("#chatInput");
  const body = input.value;
  if (!body.trim()) return;
  input.disabled = true;
  try {
    const message = await request(apiPath(state.workspaceId, "chat"), { method: "POST", body: { body } });
    addChatMessage(message);
    input.value = "";
  } catch (error) { showNotice(error.message); }
  input.disabled = false;
  input.focus();
}

function addChatMessage(message) {
  if (state.chatMessages.some((existing) => existing.id === message.id)) return;
  state.chatMessages.push(message);
  const history = document.querySelector("#chatHistory");
  if (history && !history.closest(".editor-file-page[hidden]")) renderChatMessages();
}

async function renderMembers(target) {
  const members = await request(apiPath(state.workspaceId, "members"));
  const owner = state.workspaceRole === "owner";
  target.innerHTML = `
    <section class="members-view">
      <header class="section-heading"><div><p class="eyebrow">PEOPLE</p><h2>メンバー</h2></div><span class="muted">${members.length} 人</span></header>
      ${owner ? `<section class="invite-panel"><div><h3>招待 URL を発行</h3><p class="muted">URL は発行時に一度だけ表示されます。有効期限は7日間です。</p></div><form class="invite-form" id="inviteForm">${rolePickerMarkup("reader", "inviteRole", "参加権限")}<button class="button button-primary" type="submit">招待を作成</button></form><p id="inviteResult" class="invite-result" hidden></p></section>` : ""}
      <ul class="member-list" id="memberList"></ul>
      ${owner ? `<section class="invitation-list-section"><h3>発行済み招待</h3><ul id="invitationList" class="invitation-list"></ul></section>` : ""}
    </section>`;
  const list = target.querySelector("#memberList");
  members.forEach((member) => {
    const row = document.createElement("li");
    row.className = "member-row";
    const avatar = document.createElement("span");
    avatar.className = "user-avatar avatar";
    avatar.innerHTML = avatarContent(member.user);
    avatar.setAttribute("aria-hidden", "true");
    const name = document.createElement("div");
    name.className = "member-name";
    name.innerHTML = `<strong>${escapeHtml(member.user.display_name)}</strong><small>${member.owner ? "ワークスペース所有者" : escapeHtml(member.role)}</small>`;
    bindAvatarImages(avatar);
    row.append(avatar, name);
    if (owner && !member.owner) {
      const roleContainer = document.createElement("div");
      roleContainer.innerHTML = rolePickerMarkup(member.role, `memberRole-${member.user.id}`, `${member.user.display_name} の権限`);
      const rolePicker = roleContainer.firstElementChild;
      attachRolePicker(rolePicker, (role) => updateMember(member.user.id, role));
      const remove = document.createElement("button");
      remove.className = "button button-danger-quiet";
      remove.type = "button";
      remove.textContent = "削除";
      remove.addEventListener("click", () => removeMember(member.user.id, member.user.display_name));
      row.append(rolePicker, remove);
    }
    list.append(row);
  });
  if (owner) {
    attachRolePicker(target.querySelector("#inviteRole"));
    target.querySelector("#inviteForm").addEventListener("submit", createInvitation);
    const invitations = await request(apiPath(state.workspaceId, "invitations"));
    const inviteList = target.querySelector("#invitationList");
    if (!invitations.length) inviteList.innerHTML = `<li class="empty-list">招待はありません。</li>`;
    invitations.forEach((invitation) => {
      const item = document.createElement("li");
      item.className = "invitation-row";
      const text = document.createElement("span");
      const expired = new Date(invitation.expires_at) <= new Date();
      text.textContent = `${invitation.role === "writer" ? "編集者" : "閲覧者"} · ${invitation.revoked_at ? "失効済み" : expired ? "期限切れ" : `期限 ${new Date(invitation.expires_at).toLocaleDateString("ja-JP")}`}`;
      item.append(text);
      if (!invitation.revoked_at && !expired) {
        const revoke = document.createElement("button");
        revoke.className = "button button-quiet";
        revoke.type = "button";
        revoke.textContent = "失効";
        revoke.addEventListener("click", () => revokeInvitation(invitation.id));
        item.append(revoke);
      }
      inviteList.append(item);
    });
  }
}

async function createInvitation(event) {
  event.preventDefault();
  try {
    const result = await request(apiPath(state.workspaceId, "invitations"), { method: "POST", body: { role: document.querySelector("#inviteRole").dataset.value } });
    const url = new URL(result.path, location.origin).toString();
    const target = document.querySelector("#inviteResult");
    target.innerHTML = `<strong>招待 URL</strong><input readonly value="${escapeHtml(url)}" aria-label="招待 URL"><button class="button button-secondary" type="button" id="copyInvite">コピー</button>`;
    target.hidden = false;
    target.querySelector("#copyInvite").addEventListener("click", () => navigator.clipboard.writeText(url).then(() => showNotice("招待 URL をコピーしました")));
    const list = document.querySelector("#invitationList");
    if (list) {
      const item = document.createElement("li");
      item.className = "invitation-row";
      item.textContent = `${result.invitation.role === "writer" ? "編集者" : "閲覧者"} · 期限 ${new Date(result.invitation.expires_at).toLocaleDateString("ja-JP")}`;
      list.prepend(item);
    }
  } catch (error) { showNotice(error.message); }
}

function rolePickerMarkup(value, id, label) {
  const options = [
    { value: "reader", name: "閲覧者", detail: "ファイルを閲覧" },
    { value: "writer", name: "編集者", detail: "ファイルを編集" },
  ];
  const selected = options.find((option) => option.value === value) || options[0];
  return `<div class="role-picker" id="${escapeHtml(id)}" data-value="${selected.value}"><button class="role-picker-trigger" type="button" aria-label="${escapeHtml(label)}" aria-haspopup="listbox" aria-expanded="false"><span data-role-label>${selected.name}</span>${tablerIcon("chevron-down")}</button><div class="role-picker-menu" role="listbox" aria-label="${escapeHtml(label)}" hidden>${options.map((option) => `<button class="role-picker-option${option.value === selected.value ? " is-selected" : ""}" type="button" role="option" data-role-value="${option.value}" aria-selected="${option.value === selected.value}"><span>${option.name}</span><small>${option.detail}</small></button>`).join("")}</div></div>`;
}

function attachRolePicker(root, onChange = () => {}) {
  if (!root) return;
  const trigger = root.querySelector(".role-picker-trigger");
  const menu = root.querySelector(".role-picker-menu");
  const close = () => {
    menu.hidden = true;
    trigger.setAttribute("aria-expanded", "false");
  };
  const open = () => {
    menu.hidden = false;
    trigger.setAttribute("aria-expanded", "true");
    menu.querySelector(`[data-role-value="${root.dataset.value}"]`)?.focus();
  };
  trigger.addEventListener("click", () => menu.hidden ? open() : close());
  trigger.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      open();
    }
  });
  menu.addEventListener("click", (event) => {
    const option = event.target.closest("[data-role-value]");
    if (!option) return;
    root.dataset.value = option.dataset.roleValue;
    root.querySelector("[data-role-label]").textContent = option.querySelector("span").textContent;
    menu.querySelectorAll("[data-role-value]").forEach((item) => {
      const selected = item === option;
      item.classList.toggle("is-selected", selected);
      item.setAttribute("aria-selected", String(selected));
    });
    close();
    trigger.focus();
    onChange(root.dataset.value);
  });
  menu.addEventListener("keydown", (event) => {
    if (event.key === "Escape") { event.preventDefault(); close(); trigger.focus(); return; }
    if (!(["ArrowDown", "ArrowUp"].includes(event.key))) return;
    event.preventDefault();
    const options = [...menu.querySelectorAll("[data-role-value]")];
    const current = options.indexOf(document.activeElement);
    const delta = event.key === "ArrowDown" ? 1 : -1;
    options[(current + delta + options.length) % options.length]?.focus();
  });
  root.addEventListener("focusout", (event) => {
    if (!root.contains(event.relatedTarget)) close();
  });
}

async function updateMember(userId, role) {
  try { await request(apiPath(state.workspaceId, `members/${userId}`), { method: "PATCH", body: { role } }); await renderWorkspace(); }
  catch (error) { showNotice(error.message); }
}

async function removeMember(userId, name) {
  if (!await confirmDialog({ title: "メンバーを削除", message: `${name} さんをワークスペースから削除しますか？`, confirmLabel: "削除", danger: true })) return;
  try { await request(apiPath(state.workspaceId, `members/${userId}`), { method: "DELETE" }); await renderWorkspace(); }
  catch (error) { showNotice(error.message); }
}

async function revokeInvitation(invitationId) {
  try { await request(apiPath(state.workspaceId, `invitations/${invitationId}`), { method: "DELETE" }); await refreshWorkspacePanel("members"); }
  catch (error) { showNotice(error.message); }
}

async function renderHistory(target) {
  const canWrite = ["owner", "writer"].includes(state.workspaceRole);
  const owner = state.workspaceRole === "owner";
  const commits = await request(apiPath(state.workspaceId, "checkpoints", { limit: 100 }));
  target.innerHTML = `
    <section class="history-view">
      <header class="section-heading"><div><p class="eyebrow">VERSION HISTORY</p><h2>チェックポイント</h2></div><span class="muted">main</span></header>
      ${canWrite ? `<form id="checkpointForm" class="checkpoint-form"><label class="visually-hidden" for="checkpointMessage">チェックポイント名</label><input id="checkpointMessage" maxlength="160" placeholder="変更内容をひとことで"><button class="button button-primary" type="submit">チェックポイントを作成</button></form>` : ""}
      <ol class="checkpoint-list" id="checkpointList"></ol>
    </section>`;
  const list = target.querySelector("#checkpointList");
  if (!commits.length) list.innerHTML = `<li class="empty-state">チェックポイントはまだありません。</li>`;
  commits.forEach((commit, index) => {
    const row = document.createElement("li");
    row.className = "checkpoint-row";
    const marker = document.createElement("span");
    marker.className = "checkpoint-marker";
    marker.setAttribute("aria-hidden", "true");
    marker.dataset.isHead = String(index === 0);
    const info = document.createElement("div");
    info.className = "checkpoint-info";
    const committedAt = new Date(commit.committed_at);
    info.innerHTML = `<strong>${escapeHtml(commit.message)}</strong><div class="checkpoint-meta"><span>${escapeHtml(commit.author)}</span><time datetime="${escapeHtml(commit.committed_at)}">${escapeHtml(committedAt.toLocaleString("ja-JP"))}</time><code>${escapeHtml(commit.id.slice(0, 12))}</code></div>`;
    const actions = document.createElement("div");
    actions.className = "checkpoint-actions";
    const inspect = document.createElement("button");
    inspect.className = "button button-secondary";
    inspect.type = "button";
    inspect.textContent = "内容を見る";
    inspect.addEventListener("click", () => inspectCheckpoint(commit));
    actions.append(inspect);
    if (owner) {
      const restore = document.createElement("button");
      restore.className = "button button-quiet";
      restore.type = "button";
      restore.textContent = "復元";
      restore.addEventListener("click", () => restoreCheckpoint(commit));
      actions.append(restore);
    }
    row.append(marker, info, actions);
    list.append(row);
  });
  if (canWrite) target.querySelector("#checkpointForm").addEventListener("submit", createCheckpoint);
}

async function createCheckpoint(event) {
  event.preventDefault();
  const input = document.querySelector("#checkpointMessage");
  if (!input.value.trim()) { showNotice("チェックポイント名を入力してください。"); input.focus(); return; }
  try {
    const commit = await request(apiPath(state.workspaceId, "checkpoints"), { method: "POST", body: { message: input.value } });
    await renderWorkspace();
    if (!commit) showNotice("変更がないため、新しいチェックポイントは作成されませんでした。");
  } catch (error) { showNotice(error.message); }
}

async function inspectCheckpoint(commit) {
  try {
    const changes = await request(apiPath(state.workspaceId, `checkpoints/${commit.id}/changes`));
    const dialog = document.createElement("dialog");
    dialog.className = "history-dialog";
    dialog.innerHTML = `<header><div><p class="eyebrow">COMMIT ${escapeHtml(commit.id.slice(0, 12))}</p><h2>${escapeHtml(commit.message)}</h2><p class="checkpoint-meta">${escapeHtml(commit.author)} · ${escapeHtml(new Date(commit.committed_at).toLocaleString("ja-JP"))}</p></div><button class="icon-button" type="button" aria-label="閉じる">${tablerIcon("x")}</button></header><p class="history-change-summary"></p><ul class="history-files"></ul><div class="history-file-preview"><p class="muted">変更ファイルを選ぶと、このコミットの内容が表示されます。</p></div>`;
    const fileList = dialog.querySelector(".history-files");
    const counts = changes.reduce((result, change) => { result[change.kind] = (result[change.kind] || 0) + 1; return result; }, {});
    dialog.querySelector(".history-change-summary").textContent = `${changes.length} 件の変更 · 追加 ${counts.added || 0} · 変更 ${counts.modified || 0} · 削除 ${counts.deleted || 0}`;
    if (!changes.length) fileList.innerHTML = `<li class="empty-state">親コミットとの差分はありません。</li>`;
    changes.forEach((change) => {
      const item = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      const labels = { added: "A", modified: "M", deleted: "D" };
      button.innerHTML = `<span class="change-kind change-kind-${escapeHtml(change.kind)}">${labels[change.kind] || "?"}</span><span>${escapeHtml(change.path)}</span>`;
      button.addEventListener("click", async () => {
        const preview = dialog.querySelector(".history-file-preview");
        if (change.kind === "deleted") {
          preview.textContent = "このコミットで削除されました。";
          return;
        }
        const response = await request(apiPath(state.workspaceId, `checkpoints/${commit.id}/file`, { path: change.path }));
        const bytes = await response.arrayBuffer();
        if (isTextBytes(bytes)) {
          const pre = document.createElement("pre");
          pre.textContent = new TextDecoder().decode(bytes);
          preview.replaceChildren(pre);
        } else {
          preview.textContent = `バイナリファイル · ${formatSize(bytes.byteLength)}`;
        }
      });
      item.append(button);
      fileList.append(item);
    });
    dialog.querySelector("header button").addEventListener("click", () => dialog.close());
    dialog.addEventListener("close", () => dialog.remove());
    document.body.append(dialog);
    dialog.showModal();
  } catch (error) { showNotice(error.message); }
}

async function restoreCheckpoint(commit) {
  if (!await confirmDialog({ title: "チェックポイントを復元", message: `「${commit.message}」の状態に復元します。現在の状態は先にチェックポイントへ保存されます。`, confirmLabel: "復元", danger: true })) return;
  try {
    await request(apiPath(state.workspaceId, `checkpoints/${commit.id}/restore`), { method: "POST" });
    disposeEditor();
    state.editorLayout = createEditorGroup();
    state.focusedGroupId = state.editorLayout.id;
    editorLayoutChanged();
    state.currentDir = "";
    await renderWorkspace();
    showNotice("ワークスペースを復元しました");
  } catch (error) { showNotice(error.message); }
}

async function renderWorkspaceSettings(target) {
  const owner = state.workspaceRole === "owner";
  target.innerHTML = `
    <section class="settings-view"><header class="section-heading"><div><p class="eyebrow">SETTINGS</p><h2>ワークスペース設定</h2></div></header>
      <dl class="settings-list"><div><dt>名前</dt><dd>${escapeHtml(state.workspace.name)}</dd></div><div><dt>作成日</dt><dd>${new Date(state.workspace.created_at).toLocaleString("ja-JP")}</dd></div><div><dt>権限</dt><dd>${escapeHtml(state.workspaceRole)}</dd></div><div><dt>Workspace ID</dt><dd><code>${escapeHtml(state.workspace.id)}</code></dd></div></dl>
      ${owner ? `<section class="danger-zone"><h3>ワークスペースを削除</h3><p>ファイル、メンバー、チャット、履歴を削除します。</p><button class="button button-danger" id="deleteWorkspace">削除する</button></section>` : ""}
    </section>`;
  if (owner) target.querySelector("#deleteWorkspace").addEventListener("click", deleteWorkspace);
}

async function deleteWorkspace() {
  if (!await confirmDialog({ title: "ワークスペースを削除", message: `「${state.workspace.name}」と、そのファイル・メンバー・チャット・履歴を削除します。この操作は元に戻せません。`, confirmLabel: "ワークスペースを削除", danger: true })) return;
  try {
    await request(`/api/workspaces/${state.workspaceId}`, { method: "DELETE" });
    closeWorkspaceSockets();
    await loadWorkspaces();
    history.replaceState(null, "", "/");
    renderHome();
  } catch (error) { showNotice(error.message); }
}

async function showAccount(fromTab) {
  if (fromTab && state.workspace) { await openWorkspaceView("settings"); return; }
  renderShell();
  document.querySelector("#topbarTitle").textContent = "アカウント設定";
  const target = document.querySelector("#pageContent");
  target.innerHTML = `<section class="settings-view"><header class="section-heading"><div><p class="eyebrow">ACCOUNT</p><h1>アカウント設定</h1></div></header><section class="account-profile">${avatarMarkup(state.user, "account-avatar", true)}<div><strong>${escapeHtml(userName(state.user))}</strong><small>Preferred username · ${escapeHtml(state.user.preferred_username || "プロバイダーから未提供")}</small></div></section><form id="accountForm" class="account-form"><label for="displayName">表示名</label><input id="displayName" maxlength="80" value="${escapeHtml(state.user.display_name)}"><button class="button button-primary" type="submit">保存</button></form><dl class="settings-list"><div><dt>ユーザー ID</dt><dd><code>${escapeHtml(state.user.id)}</code></dd></div><div><dt>作成日</dt><dd>${new Date(state.user.created_at).toLocaleString("ja-JP")}</dd></div></dl></section>`;
  bindAvatarImages(target);
  target.querySelector("#accountForm").addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!target.querySelector("#displayName").value.trim()) { showNotice("表示名を入力してください。"); target.querySelector("#displayName").focus(); return; }
    try {
      state.user = await request("/api/account", { method: "PATCH", body: { display_name: target.querySelector("#displayName").value } });
      await loadWorkspaces();
      showNotice("表示名を保存しました");
      showAccount(false);
    } catch (error) { showNotice(error.message); }
  });
}

async function logout() {
  try { await request("/api/logout", { method: "POST" }); }
  catch (_) {}
  location.href = "/";
}

function connectWorkspaceSockets(workspaceId) {
  connectWorkspaceSocket(workspaceId, "events/ws", "eventSocket", (event) => handleWorkspaceEvent(workspaceId, event));
  connectWorkspaceSocket(workspaceId, "chat/ws", "chatSocket", (event) => {
    if (event.type === "message" && event.message) addChatMessage(event.message);
  });
}

function connectWorkspaceSocket(workspaceId, path, key, onMessage) {
  const endpoint = new URL(apiPath(workspaceId, path), location.origin);
  endpoint.protocol = endpoint.protocol === "https:" ? "wss:" : "ws:";
  const socket = new WebSocket(endpoint);
  state[key] = socket;
  socket.addEventListener("message", (event) => {
    try { onMessage(JSON.parse(event.data)); } catch (_) {}
  });
  socket.addEventListener("close", () => {
    if (state[key] !== socket || state.workspaceId !== workspaceId) return;
    const timer = setTimeout(() => {
      if (state.workspaceId === workspaceId) connectWorkspaceSocket(workspaceId, path, key, onMessage);
    }, 1500);
    state.reconnectTimers.push(timer);
  });
  socket.addEventListener("error", () => socket.close());
}

function closeWorkspaceSockets() {
  state.reconnectTimers.forEach(clearTimeout);
  state.reconnectTimers = [];
  [state.eventSocket, state.chatSocket].forEach((socket) => {
    if (socket && socket.readyState < WebSocket.CLOSING) socket.close();
  });
  state.eventSocket = null;
  state.chatSocket = null;
  state.chatMessages = [];
}

async function handleWorkspaceEvent(workspaceId, event) {
  if (state.workspaceId !== workspaceId) return;
  if (event.kind === "workspace_deleted") {
    closeWorkspaceSockets();
    await loadWorkspaces().catch(() => {});
    renderHome();
    showNotice("このワークスペースは削除されました");
    return;
  }
  if (event.kind === "file_tree_restored" || event.kind.startsWith("file_") || event.kind === "path_deleted" || event.kind === "path_moved" || event.kind === "directory_created") {
    invalidateTypstWorkspace(state.workspaceId, event);
    if (event.kind !== "file_changed" || !event.path?.toLowerCase().endsWith(".typ")) scheduleTypstPreviewRefresh();
  }
  if (event.kind === "file_tree_restored") {
    disposeEditor();
    state.editorLayout = createEditorGroup();
    state.focusedGroupId = state.editorLayout.id;
    editorLayoutChanged();
    state.currentDir = "";
    await renderWorkspace();
    showNotice("チェックポイントからファイルを復元しました");
    return;
  }
  if (event.kind === "members_changed") {
    await loadWorkspaces().catch(() => {});
    const current = state.workspaces.find((workspace) => workspace.id === state.workspaceId);
    if (!current) {
      closeWorkspaceSockets();
      renderHome();
      showNotice("このワークスペースへのアクセス権がありません");
      return;
    }
    state.workspaceRole = current.role;
    disposeEditor();
    await renderWorkspace();
    return;
  }
  if (event.kind === "path_deleted") {
    removeOpenPaths(event.path);
    await refreshFiles();
    await renderFiles(document.querySelector("#tabContent"));
  } else if (event.kind === "file_replaced") {
    const file = state.openFiles.get(event.path);
    if (file) {
      const requestId = (file.replacementRequestId || 0) + 1;
      file.replacementRequestId = requestId;
      const previewEnabled = file.previewEnabled;
      disposeFile(file);
      try {
        const replacement = await loadFileData(event.path, workspaceId);
        if (state.workspaceId !== workspaceId
          || state.openFiles.get(event.path) !== file
          || file.replacementRequestId !== requestId) {
          disposeFile(replacement);
          return;
        }
        replacement.previewEnabled = previewEnabled;
        state.openFiles.set(event.path, replacement);
        const target = document.querySelector("#tabContent");
        if (target) await renderFiles(target);
        showNotice("アップロードされたファイルを再読み込みしました");
      } catch (error) {
        removeOpenPaths(event.path);
        showNotice(`ファイルを再読み込みできません: ${error.message}`);
      }
    } else {
      await refreshFiles();
    }
  } else if (event.kind === "path_moved") {
    if (state.currentDir === event.path || state.currentDir.startsWith(`${event.path}/`)) {
      state.currentDir = `${event.destination}${state.currentDir.slice(event.path.length)}`;
    }
    renameOpenPaths(event.path, event.destination);
    await refreshFiles();
    await renderFiles(document.querySelector("#tabContent"));
  } else if (event.kind.startsWith("file_") || event.kind === "directory_created" || event.kind === "path_moved") {
    await refreshFiles();
  } else if (event.kind === "checkpoint_created") {
    await refreshWorkspacePanel("history");
  }
}

async function refreshWorkspacePanel(view) {
  const tabId = workspaceViewTab(view);
  const page = [...document.querySelectorAll(".workspace-panel-page")].find((item) => item.dataset.filePath === tabId);
  const content = page?.querySelector(".workspace-panel-content");
  if (!content) return;
  page.dataset.panelRendered = "false";
  await attachWorkspacePanels(page.closest("#tabContent"));
}

function showNotice(message) {
  state.notice = message;
  const notice = document.querySelector("#workspaceNotice");
  if (notice) {
    notice.textContent = message;
    notice.hidden = false;
    clearTimeout(showNotice.timer);
    showNotice.timer = setTimeout(() => { notice.hidden = true; }, 5000);
  } else {
    const node = document.createElement("div");
    node.className = "toast-notice";
    node.setAttribute("role", "status");
    node.textContent = message;
    document.body.append(node);
    setTimeout(() => node.remove(), 3500);
  }
}

function fileGlyph(name) {
  const type = extension(name);
  if (["md", "markdown", "txt", "csv"].includes(type)) return tablerIcon("file-text");
  if (["typ", "rs", "js", "ts", "tsx", "jsx", "py", "go", "html", "css", "scss", "json", "toml", "yaml", "yml", "svg"].includes(type)) return tablerIcon("file-code");
  return tablerIcon("file");
}

function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

document.addEventListener("keydown", (event) => {
  if (!(event.ctrlKey || event.metaKey) || !state.workspace || document.querySelector("dialog[open]")) return;
  if (event.key.toLowerCase() === "p" && !event.shiftKey) {
    event.preventDefault();
    showQuickOpen();
  } else if (event.key.toLowerCase() === "w" && !event.shiftKey) {
    event.preventDefault();
    const group = findEditorGroup(state.editorLayout, state.focusedGroupId);
    if (group?.activePath) closeEditorTab(group.id, group.activePath);
  } else if (event.key === "\\") {
    event.preventDefault();
    splitEditor(state.focusedGroupId, event.shiftKey ? "vertical" : "horizontal");
  }
});

boot();
