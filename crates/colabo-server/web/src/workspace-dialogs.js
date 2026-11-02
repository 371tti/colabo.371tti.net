import { tablerIcon } from "./icons.js";

const MAX_WORKSPACE_IMPORT_BYTES = 190_000_000;

export function workspaceCreationDialog() {
  return new Promise((resolve) => {
    const dialog = document.createElement("dialog");
    dialog.className = "app-dialog workspace-creation-dialog";
    dialog.innerHTML = `
      <form class="app-dialog-card">
        <header class="app-dialog-heading"><div><p class="eyebrow">NEW WORKSPACE</p><h2>ワークスペースを作成</h2></div><button class="icon-button" type="button" data-dialog-close aria-label="閉じる">${tablerIcon("x")}</button></header>
        <label class="app-dialog-field"><span>ワークスペース名</span><input data-workspace-name maxlength="80" placeholder="例: プロジェクト名" autocomplete="off" spellcheck="false"></label>
        <section class="workspace-folder-choice">
          <button class="button button-secondary folder-picker-button" type="button" data-folder-picker>${tablerIcon("folder-open")}<span>フォルダーを選択</span></button>
          <input type="file" data-folder-input webkitdirectory directory multiple hidden>
          <p class="app-dialog-message" data-folder-status>選択したフォルダーの中身と階層を取り込みます。合計190 MBまで。</p>
        </section>
        <p class="app-dialog-error" data-dialog-error role="alert" hidden></p>
        <footer class="app-dialog-actions workspace-creation-actions">
          <button class="button button-secondary" type="button" data-dialog-cancel>キャンセル</button>
          <button class="button button-secondary" type="submit" data-kind="empty">空で作成</button>
          <button class="button button-primary" type="submit" data-kind="folder" disabled>フォルダーから作成</button>
        </footer>
      </form>`;
    const form = dialog.querySelector("form");
    const nameInput = dialog.querySelector("[data-workspace-name]");
    const folderInput = dialog.querySelector("[data-folder-input]");
    const folderStatus = dialog.querySelector("[data-folder-status]");
    const folderButton = dialog.querySelector('[data-kind="folder"]');
    const error = dialog.querySelector("[data-dialog-error]");
    let selectedFiles = [];
    let folderName = "";
    let nameEdited = false;
    let settled = false;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      dialog.close();
      dialog.remove();
      resolve(result);
    };
    nameInput.addEventListener("input", () => { nameEdited = true; });
    dialog.querySelector("[data-folder-picker]").addEventListener("click", () => folderInput.click());
    folderInput.addEventListener("change", () => {
      selectedFiles = [...folderInput.files || []];
      folderName = selectedFiles[0]?.webkitRelativePath?.split("/")[0] || "";
      const totalBytes = selectedFiles.reduce((total, file) => total + file.size, 0);
      const tooLarge = totalBytes > MAX_WORKSPACE_IMPORT_BYTES;
      folderButton.disabled = !selectedFiles.length || tooLarge;
      if (!nameEdited && folderName) nameInput.value = folderName;
      if (!selectedFiles.length) {
        folderStatus.textContent = "選択したフォルダーの中身と階層を取り込みます。合計190 MBまで。";
      } else if (tooLarge) {
        folderStatus.textContent = `${folderName} · ${selectedFiles.length} 個 · ${formatBytes(totalBytes)}。合計サイズが190 MBを超えています。`;
      } else {
        folderStatus.textContent = `${folderName} · ${selectedFiles.length} 個 · ${formatBytes(totalBytes)}`;
      }
      error.hidden = true;
      folderInput.value = "";
    });
    dialog.addEventListener("cancel", (event) => { event.preventDefault(); finish(null); });
    dialog.addEventListener("click", (event) => { if (event.target === dialog) finish(null); });
    dialog.querySelector("[data-dialog-close]").addEventListener("click", () => finish(null));
    dialog.querySelector("[data-dialog-cancel]").addEventListener("click", () => finish(null));
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const name = nameInput.value.trim();
      const kind = event.submitter?.dataset.kind;
      if (!name) {
        error.textContent = "ワークスペース名を入力してください。";
        error.hidden = false;
        nameInput.focus();
        return;
      }
      if (kind === "folder" && !selectedFiles.length) return;
      finish({ kind, name, folderName, files: selectedFiles });
    });
    document.body.append(dialog);
    dialog.showModal();
    nameInput.focus();
  });
}

function formatBytes(bytes) {
  if (bytes < 1_000_000) return `${(bytes / 1000).toFixed(1)} kB`;
  return `${(bytes / 1_000_000).toFixed(1)} MB`;
}

export function inputDialog({ title, message = "", label, value = "", placeholder = "", confirmLabel = "決定", maxLength = 512 }) {
  return new Promise((resolve) => {
    const dialog = document.createElement("dialog");
    dialog.className = "app-dialog";
    dialog.innerHTML = `
      <form class="app-dialog-card">
        <header class="app-dialog-heading"><div><p class="eyebrow">COLABO</p><h2>${escapeHtml(title)}</h2></div><button class="icon-button" type="button" data-dialog-close aria-label="閉じる">${tablerIcon("x")}</button></header>
        ${message ? `<p class="app-dialog-message">${escapeHtml(message)}</p>` : ""}
        <label class="app-dialog-field"><span>${escapeHtml(label)}</span><input data-dialog-input maxlength="${maxLength}" value="${escapeHtml(value)}" placeholder="${escapeHtml(placeholder)}" autocomplete="off" spellcheck="false"></label>
        <p class="app-dialog-error" data-dialog-error role="alert" hidden></p>
        <footer class="app-dialog-actions"><button class="button button-secondary" type="button" data-dialog-cancel>キャンセル</button><button class="button button-primary" type="submit">${escapeHtml(confirmLabel)}</button></footer>
      </form>`;
    const form = dialog.querySelector("form");
    const input = dialog.querySelector("[data-dialog-input]");
    const error = dialog.querySelector("[data-dialog-error]");
    let settled = false;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      dialog.close();
      dialog.remove();
      resolve(result);
    };
    dialog.addEventListener("cancel", (event) => { event.preventDefault(); finish(null); });
    dialog.addEventListener("click", (event) => { if (event.target === dialog) finish(null); });
    dialog.querySelector("[data-dialog-close]").addEventListener("click", () => finish(null));
    dialog.querySelector("[data-dialog-cancel]").addEventListener("click", () => finish(null));
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const result = input.value.trim();
      if (!result) {
        error.textContent = "値を入力してください。";
        error.hidden = false;
        input.focus();
        return;
      }
      finish(result);
    });
    document.body.append(dialog);
    dialog.showModal();
    input.focus();
    input.select();
  });
}

export function confirmDialog({ title, message, confirmLabel = "続行", danger = false }) {
  return new Promise((resolve) => {
    const dialog = document.createElement("dialog");
    dialog.className = "app-dialog app-dialog-confirm";
    dialog.innerHTML = `
      <section class="app-dialog-card">
        <header class="app-dialog-heading"><div><p class="eyebrow">確認</p><h2>${escapeHtml(title)}</h2></div><button class="icon-button" type="button" data-dialog-close aria-label="閉じる">${tablerIcon("x")}</button></header>
        <p class="app-dialog-message">${escapeHtml(message)}</p>
        <footer class="app-dialog-actions"><button class="button button-secondary" type="button" data-dialog-cancel>キャンセル</button><button class="button ${danger ? "button-danger" : "button-primary"}" type="button" data-dialog-confirm>${escapeHtml(confirmLabel)}</button></footer>
      </section>`;
    let settled = false;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      dialog.close();
      dialog.remove();
      resolve(result);
    };
    dialog.addEventListener("cancel", (event) => { event.preventDefault(); finish(false); });
    dialog.addEventListener("click", (event) => { if (event.target === dialog) finish(false); });
    dialog.querySelector("[data-dialog-close]").addEventListener("click", () => finish(false));
    dialog.querySelector("[data-dialog-cancel]").addEventListener("click", () => finish(false));
    dialog.querySelector("[data-dialog-confirm]").addEventListener("click", () => finish(true));
    document.body.append(dialog);
    dialog.showModal();
    dialog.querySelector("[data-dialog-confirm]").focus();
  });
}

export function quickOpenDialog(paths) {
  return new Promise((resolve) => {
    const dialog = document.createElement("dialog");
    dialog.className = "app-dialog quick-open-dialog";
    dialog.innerHTML = `<section class="app-dialog-card"><header class="quick-open-heading"><label class="quick-open-search">${tablerIcon("search")}<input type="search" data-quick-search placeholder="ファイル名またはパスで検索" autocomplete="off" spellcheck="false"></label><button class="icon-button" type="button" data-dialog-close aria-label="閉じる">${tablerIcon("x")}</button></header><ul class="quick-open-results" role="listbox"></ul><footer class="quick-open-hint"><span>↑ ↓ で選択</span><span>Enter で開く</span><span>Esc で閉じる</span></footer></section>`;
    const input = dialog.querySelector("[data-quick-search]");
    const list = dialog.querySelector(".quick-open-results");
    let matches = [];
    let activeIndex = 0;
    let settled = false;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      dialog.close();
      dialog.remove();
      resolve(result);
    };
    const render = () => {
      const query = input.value.trim().toLocaleLowerCase();
      matches = paths.filter((path) => !query || path.toLocaleLowerCase().includes(query)).slice(0, 40);
      activeIndex = Math.min(activeIndex, Math.max(matches.length - 1, 0));
      list.replaceChildren();
      if (!matches.length) {
        const empty = document.createElement("li");
        empty.className = "quick-open-empty";
        empty.textContent = "一致するファイルがありません";
        list.append(empty);
        return;
      }
      matches.forEach((path, index) => {
        const item = document.createElement("li");
        const button = document.createElement("button");
        button.type = "button";
        button.className = `quick-open-option${index === activeIndex ? " is-active" : ""}`;
        button.setAttribute("role", "option");
        button.setAttribute("aria-selected", String(index === activeIndex));
        const name = document.createElement("strong");
        name.textContent = path.split("/").pop();
        const directory = document.createElement("span");
        directory.textContent = path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "ワークスペース";
        button.append(name, directory);
        button.addEventListener("click", () => finish(path));
        item.append(button);
        list.append(item);
      });
    };
    input.addEventListener("input", () => { activeIndex = 0; render(); });
    input.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown" && matches.length) {
        event.preventDefault();
        activeIndex = (activeIndex + 1) % matches.length;
        render();
      } else if (event.key === "ArrowUp" && matches.length) {
        event.preventDefault();
        activeIndex = (activeIndex - 1 + matches.length) % matches.length;
        render();
      } else if (event.key === "Enter" && matches[activeIndex]) {
        event.preventDefault();
        finish(matches[activeIndex]);
      }
    });
    dialog.addEventListener("cancel", (event) => { event.preventDefault(); finish(null); });
    dialog.addEventListener("click", (event) => { if (event.target === dialog) finish(null); });
    dialog.querySelector("[data-dialog-close]").addEventListener("click", () => finish(null));
    document.body.append(dialog);
    render();
    dialog.showModal();
    input.focus();
  });
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (character) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[character]);
}
