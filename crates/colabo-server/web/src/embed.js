import * as monaco from "monaco-editor";
import EditorWorker from "monaco-editor/editor/editor.worker.js?worker";
import JsonWorker from "monaco-editor/language/json/json.worker.js?worker";
import CssWorker from "monaco-editor/language/css/css.worker.js?worker";
import HtmlWorker from "monaco-editor/language/html/html.worker.js?worker";
import TypeScriptWorker from "monaco-editor/language/typescript/ts.worker.js?worker";
import "./collab-editor.css";
import { copySessionData, startCollab } from "./collab-app.js";
import { isValidEditorIdentity, isValidSessionId, randomEditorId } from "./identity.js";

const languageWorkers = new Map([
  ["json", JsonWorker],
  ["css", CssWorker], ["scss", CssWorker], ["less", CssWorker],
  ["html", HtmlWorker], ["handlebars", HtmlWorker], ["razor", HtmlWorker],
  ["javascript", TypeScriptWorker], ["typescript", TypeScriptWorker],
]);

monaco.languages.register({ id: "typst", extensions: [".typ"], aliases: ["Typst"] });
monaco.languages.setMonarchTokensProvider("typst", {
  defaultToken: "",
  tokenPostfix: ".typst",
  keywords: ["let", "set", "show", "import", "include", "if", "else", "for", "in", "while", "return", "as", "none", "auto", "true", "false"],
  tokenizer: {
    root: [
      [/\/\*/, "comment", "@comment"],
      [/\/\/.*$/, "comment"],
      [/#[A-Za-z_][\w-]*/, { cases: { "@keywords": "keyword", "@default": "keyword" } }],
      [/@[A-Za-z_][\w-]*/, "variable"],
      [/[A-Za-z_][\w-]*(?=\s*\()/, "function"],
      [/"([^"\\]|\\.)*$/, "string.invalid"],
      [/"/, "string", "@string"],
      [/-?\d+(\.\d+)?(pt|mm|cm|in|em|fr|deg|rad|%)?/, "number"],
      [/[{}()[\]]/, "@brackets"],
      [/[,:;=+*/<>!&|]/, "operator"],
    ],
    comment: [[/[^/*]+/, "comment"], [/\*\//, "comment", "@pop"], [/[\/\\*]/, "comment"]],
    string: [[/[^\\"]+/, "string"], [/\\./, "string.escape"], [/"/, "string", "@pop"]],
  },
});

globalThis.MonacoEnvironment = {
  getWorker(_workerId, label) {
    const Worker = languageWorkers.get(label) || EditorWorker;
    return new Worker();
  },
};

const mounts = new WeakMap();
const editorFontLoads = new Map();
const collaborationTheme = "collab-dark";
const GUTTER_AVATAR_SIZE = 18;

function remeasureEditorFontWhenLoaded(fontSize, fontFamily) {
  if (!document.fonts?.load) {
    monaco.editor.remeasureFonts();
    return;
  }
  const key = `${fontSize}\0${fontFamily}`;
  if (editorFontLoads.has(key)) return;
  const load = Promise.allSettled([
    document.fonts.load(`${fontSize}px ${fontFamily}`),
    document.fonts.load(`700 ${fontSize}px ${fontFamily}`),
  ]).then(() => monaco.editor.remeasureFonts());
  editorFontLoads.set(key, load);
}

function languageForFileName(fileName) {
  const name = String(fileName || "").split(/[\\/]/).pop().toLowerCase();
  let match = "plaintext";
  let longestExtension = 0;
  for (const language of monaco.languages.getLanguages()) {
    if (language.filenames?.some((filename) => filename.toLowerCase() === name)) return language.id;
    for (const extension of language.extensions || []) {
      if (extension.length > longestExtension && name.endsWith(extension.toLowerCase())) {
        match = language.id;
        longestExtension = extension.length;
      }
    }
  }
  return match;
}

monaco.editor.defineTheme(collaborationTheme, {
  base: "vs-dark",
  inherit: true,
  rules: [],
  colors: {
    "editor.background": "#1a1a1a",
    "editor.foreground": "#cccccc",
    "editorCursor.foreground": "#a9b7e6",
    "editorLineNumber.foreground": "#999999",
    "editorLineNumber.activeForeground": "#a9b7e6",
    "editorGutter.background": "#1a1a1a",
    "editor.lineHighlightBackground": "#282828",
    "editor.selectionBackground": "#6f7a954d",
    "editor.inactiveSelectionBackground": "#282828",
    "editorIndentGuide.background1": "#282828",
    "editorWidget.background": "#1a1a1a",
    "editorWidget.border": "#282828",
    "input.background": "#1a1a1a",
    "input.foreground": "#cccccc",
    "input.border": "#282828",
  },
});

function ensureStylesheet() {
  const href = new URL("./collab-editor.css", new URL(import.meta.url)).href;
  if ([...document.querySelectorAll('link[rel="stylesheet"]')].some((link) => link.href === href)) return;
  const link = document.createElement("link");
  link.rel = "stylesheet";
  link.href = href;
  document.head.append(link);
}

export function mountCollaborativeEditor(container, options = {}) {
  if (!(container instanceof HTMLElement)) throw new TypeError("container must be an HTMLElement");
  if (options.sessionData !== undefined) {
    options = { ...options, sessionData: copySessionData(options.sessionData) };
  }
  const sessionId = String(options.sessionId || "");
  if (!isValidSessionId(sessionId)) throw new TypeError("sessionId must contain 1-64 letters, digits, hyphens, or underscores");
  const userId = String(options.userId || randomEditorId());
  const userName = String(options.userName || userId);
  if (!isValidEditorIdentity(userId, userName)) throw new TypeError("userId must be 1-64 characters and userName 1-80 characters");
  ensureStylesheet();
  mounts.get(container)?.dispose();

  const shell = document.createElement("section");
  shell.className = "collab-editor-shell";
  shell.innerHTML = `
    <div class="collab-editor-host" data-role="editor" aria-label="共同編集エディター" aria-busy="true"></div>
    <div class="collab-session-expired" data-role="session-expired" role="alert" hidden>
      <strong>この編集セッションは終了しました。</strong>
      <span>新しい共有セッションを作成してください。</span>
    </div>
    <div class="collab-editor-toast" data-role="toast" role="status" aria-live="polite"></div>
  `;
  container.replaceChildren(shell);

  const editorOptions = {
    value: "",
    language: options.language || languageForFileName(options.fileName),
    theme: options.theme || collaborationTheme,
    readOnly: true,
    automaticLayout: true,
    minimap: { enabled: false },
    lineNumbers: "on",
    lineNumbersMinChars: 5,
    glyphMargin: true,
    scrollBeyondLastLine: false,
    wordWrap: "on",
    tabSize: 2,
    fontSize: 14,
    fontFamily: '"UDEV Gothic HSLG", monospace',
    fontLigatures: true,
    lineHeight: 22,
    letterSpacing: 0,
    padding: { top: 12, bottom: 12 },
    renderLineHighlight: "line",
    guides: { indentation: true, bracketPairs: true },
    bracketPairColorization: { enabled: true },
    matchBrackets: "always",
    folding: true,
    ...(options.editorOptions || {}),
  };
  remeasureEditorFontWhenLoaded(editorOptions.fontSize, editorOptions.fontFamily);
  const editor = monaco.editor.create(shell.querySelector('[data-role="editor"]'), editorOptions);
  const collaboration = startCollab(monaco, editor, shell, { ...options, sessionId, userId, userName });
  let disposed = false;
  let gutterFrame = 0;
  const gutterDisposables = [];
  const editorRoot = editor.getDomNode();
  const margin = editorRoot?.querySelector(".margin") || editorRoot?.querySelector(".margin-view-overlays")?.parentElement;
  const presenceLayer = margin ? document.createElement("div") : null;
  if (presenceLayer) {
    presenceLayer.className = "collab-gutter-presence-layer";
    presenceLayer.setAttribute("role", "group");
    presenceLayer.setAttribute("aria-label", "行ごとの編集者");
    margin.append(presenceLayer);
  }
  function scheduleGutterPresence() {
    if (!presenceLayer || disposed || gutterFrame) return;
    gutterFrame = window.requestAnimationFrame(() => {
      gutterFrame = 0;
      if (disposed) return;
      const layerRect = presenceLayer.getBoundingClientRect();
      const rows = collaboration.getGutterPresence();
      const groups = [];
      for (const row of rows) {
        const lineNumber = Number(row.lineNumber);
        const lineNode = editorRoot.querySelector(`.line-numbers[data-line-number="${lineNumber}"]`)
          || [...editorRoot.querySelectorAll(".line-numbers")].find((node) => node.textContent.trim() === String(lineNumber));
        if (!lineNode) continue;
        const lineRect = lineNode.getBoundingClientRect();
        const glyphRect = editorRoot.querySelector(".glyph-margin")?.getBoundingClientRect();
        const group = document.createElement("span");
        group.className = "ui-avatar-group collab-gutter-avatar-group";
        group.setAttribute("role", "img");
        const names = row.editors.map((item) => item.name).filter(Boolean);
        group.setAttribute("aria-label", names.join(", "));
        group.title = names.join(", ");
        group.style.left = `${Math.max(0, (glyphRect?.width ? glyphRect.left : lineRect.left) - layerRect.left + 1)}px`;
        group.style.top = `${lineRect.top - layerRect.top + (lineRect.height - GUTTER_AVATAR_SIZE) / 2}px`;
        for (const person of row.editors.slice(0, 2)) {
          const avatar = document.createElement("span");
          avatar.className = "ui-avatar ui-avatar--small ui-state--neutral collab-gutter-avatar";
          const imageUrl = safeAvatarUrl(person.avatarUrl);
          if (imageUrl) {
            const image = document.createElement("img");
            image.className = "user-avatar-image";
            image.src = imageUrl;
            image.alt = "";
            image.loading = "lazy";
            image.referrerPolicy = "no-referrer";
            avatar.append(image);
          } else {
            avatar.textContent = Array.from(person.name.trim())[0] || "?";
          }
          group.append(avatar);
        }
        if (row.editors.length > 2) {
          const overflow = document.createElement("span");
          overflow.className = "ui-avatar ui-avatar--small ui-state--neutral collab-gutter-avatar collab-gutter-avatar-overflow";
          overflow.textContent = `+${row.editors.length - 2}`;
          group.append(overflow);
        }
        groups.push(group);
      }
      presenceLayer.replaceChildren(...groups);
    });
  }
  function safeAvatarUrl(value) {
    try {
      const url = new URL(value);
      return ["http:", "https:"].includes(url.protocol) && url.hostname && !url.username && !url.password
        ? url.href
        : "";
    } catch (_) {
      return "";
    }
  }
  if (presenceLayer) {
    gutterDisposables.push(editor.onDidChangeCursorSelection(scheduleGutterPresence));
    gutterDisposables.push(editor.onDidChangeModelContent(scheduleGutterPresence));
    gutterDisposables.push(editor.onDidScrollChange(scheduleGutterPresence));
    gutterDisposables.push(editor.onDidLayoutChange(scheduleGutterPresence));
    gutterDisposables.push(editor.onDidChangeModel(scheduleGutterPresence));
    collaboration.onSessionDataChange(scheduleGutterPresence);
    scheduleGutterPresence();
  }
  const mounted = {
    editor,
    setSessionData(data) {
      collaboration.setSessionData(data);
      scheduleGutterPresence();
    },
    onSessionDataChange(listener) {
      return collaboration.onSessionDataChange(listener);
    },
    getGutterPresence() {
      return collaboration.getGutterPresence();
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      if (gutterFrame) window.cancelAnimationFrame(gutterFrame);
      gutterDisposables.forEach((disposable) => disposable.dispose());
      presenceLayer?.remove();
      collaboration.dispose();
      editor.dispose();
      shell.remove();
      mounts.delete(container);
    },
  };
  mounts.set(container, mounted);
  return mounted;
}

if (typeof window !== "undefined") {
  window.CollabEditor = Object.freeze({ mount: mountCollaborativeEditor, version: "0.1.0" });
}
