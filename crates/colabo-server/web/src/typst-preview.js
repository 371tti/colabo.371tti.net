import compilerWasm from "@myriaddreamin/typst-ts-web-compiler/pkg/typst_ts_web_compiler_bg.wasm?url";
import rendererWasm from "@myriaddreamin/typst-ts-renderer/pkg/typst_ts_renderer_bg.wasm?url";
import { MemoryAccessModel } from "@myriaddreamin/typst.ts";
import { CompileFormatEnum } from "@myriaddreamin/typst.ts/compiler";
import { TypstSnippet } from "@myriaddreamin/typst.ts/contrib/snippet";

const MAX_WORKSPACE_BYTES = 256 * 1024 * 1024;
const MAX_WORKSPACE_FILES = 5000;
const FONT_EXTENSIONS = new Set(["ttf", "otf", "ttc", "otc"]);
const TYPOGRAPHY_FONT_BASE = "https://cdn.jsdelivr.net/gh/typst/typst-dev-assets@v0.15.0/files/fonts/";
const PREVIEW_FONT_URLS = [
  `${TYPOGRAPHY_FONT_BASE}InriaSerif-Bold.ttf`,
  `${TYPOGRAPHY_FONT_BASE}InriaSerif-BoldItalic.ttf`,
  `${TYPOGRAPHY_FONT_BASE}InriaSerif-Italic.ttf`,
  `${TYPOGRAPHY_FONT_BASE}InriaSerif-Regular.ttf`,
  `${TYPOGRAPHY_FONT_BASE}Roboto-Regular.ttf`,
  "https://cdn.jsdelivr.net/gh/notofonts/noto-cjk@main/Sans/OTF/Japanese/NotoSansCJKjp-Regular.otf",
  "https://cdn.jsdelivr.net/gh/notofonts/noto-fonts@main/hinted/ttf/NotoSans/NotoSans-Regular.ttf",
  "https://cdn.jsdelivr.net/gh/cormullion/juliamono@v0.062/JuliaMono-Regular.ttf",
];
const encoder = new TextEncoder();
const accessModel = new MemoryAccessModel();
let compiler = createCompiler();

function createCompiler(fonts = []) {
  const instance = new TypstSnippet();
  instance.setCompilerInitOptions({ getModule: () => ({ module_or_path: compilerWasm }) });
  instance.setRendererInitOptions({ getModule: () => ({ module_or_path: rendererWasm }) });
  const providers = [
    TypstSnippet.withAccessModel(accessModel),
    TypstSnippet.fetchPackageRegistry(accessModel),
    TypstSnippet.preloadFontAssets({ assets: ["text"] }),
  ];
  providers.splice(2, 0, TypstSnippet.preloadFonts([...PREVIEW_FONT_URLS, ...fonts]));
  instance.use(...providers);
  return instance;
}

let syncedWorkspaceId = "";
let workspaceIsSynced = false;
let workspaceNeedsFullSync = false;
let workspaceMutationVersion = 0;
let workspaceFontsNeedReload = true;
let compilerNeedsReset = false;
const managedWorkspaceFiles = new Map();
const changedWorkspaceFiles = new Set();
const deletedWorkspacePaths = new Set();
let workspaceTotalBytes = 0;
let compileQueue = Promise.resolve();

export function invalidateTypstWorkspace(workspaceId, event) {
  if (workspaceId !== syncedWorkspaceId || !event) return;
  workspaceMutationVersion += 1;
  if (event.kind === "file_changed" && event.path) {
    changedWorkspaceFiles.add(event.path);
    if (isWorkspaceFont(event.path)) {
      compilerNeedsReset = true;
      workspaceFontsNeedReload = true;
    }
  } else if (event.kind === "path_deleted" && event.path) {
    deletedWorkspacePaths.add(event.path);
    if (isWorkspaceFont(event.path) || pathContainsFonts(event.path)) {
      compilerNeedsReset = true;
      workspaceFontsNeedReload = true;
    }
  } else if (event.kind === "path_moved" || event.kind === "file_tree_restored") {
    workspaceNeedsFullSync = true;
    compilerNeedsReset = true;
    workspaceFontsNeedReload = true;
  }
}

export function renderTypstWorkspace({ workspaceId, path, source, openSources, listDirectory, readFile, isCurrent }) {
  const render = compileQueue.then(async () => {
    if (isCurrent && !isCurrent()) return null;
    if (workspaceId !== syncedWorkspaceId) {
      removeManagedWorkspaceFiles();
      syncedWorkspaceId = workspaceId;
      workspaceIsSynced = false;
      workspaceNeedsFullSync = false;
      compilerNeedsReset = true;
      workspaceFontsNeedReload = true;
    }
    if (!workspaceIsSynced || workspaceNeedsFullSync) {
      const syncVersion = workspaceMutationVersion;
      removeManagedWorkspaceFiles();
      await syncWorkspace(workspaceId, listDirectory, readFile);
      workspaceIsSynced = true;
      workspaceFontsNeedReload = true;
      if (syncVersion === workspaceMutationVersion) {
        workspaceNeedsFullSync = false;
        changedWorkspaceFiles.clear();
        deletedWorkspacePaths.clear();
      }
    } else {
      await syncWorkspaceChanges(workspaceId, readFile);
    }
    if (isCurrent && !isCurrent()) return null;

    const { fonts: workspaceFonts, warnings: fontWarnings } = workspaceFontsNeedReload
      ? await loadWorkspaceFonts()
      : { fonts: [], warnings: [] };
    if (compilerNeedsReset || workspaceFontsNeedReload) {
      compiler = createCompiler(workspaceFonts);
      compilerNeedsReset = false;
      workspaceFontsNeedReload = false;
    }

    const root = `/@memory/${workspaceId}`;
    for (const file of openSources) {
      insertManagedFile(root, file.path, encoder.encode(file.source));
    }
    const mainFilePath = `${root}/${path}`;
    if (typeof source === "string") {
      insertManagedFile(root, path, encoder.encode(source));
    } else if (!managedWorkspaceFiles.has(path)) {
      throw new Error("Typstソースをワークスペースから読み込めませんでした。");
    }

    const core = await compiler.getCompiler();
    await core.reset();
    const result = await core.compile({
      mainFilePath,
      root,
      format: CompileFormatEnum.vector,
      diagnostics: "full",
    });
    if (isCurrent && !isCurrent()) return null;
    const diagnostics = result.diagnostics || [];
    const errors = diagnostics.filter((diagnostic) => diagnostic.severity === "error");
    if (!result.result || errors.length) {
      throw new Error(formatDiagnostics(errors) || "Typst のベクター成果物を生成できませんでした。ソースと参照ファイルを確認してください。");
    }
    const renderer = await compiler.getRenderer();
    const renderedSvg = await renderer.renderSvg({
      format: "vector",
      artifactContent: result.result,
    });
    if (!renderedSvg) throw new Error("Typst SVG レンダラーが空の結果を返しました。");
    // Typst 文書由来の生の & が混ざる場合に備え、SVG の XML を整える。
    const svg = renderedSvg.replace(/&(?!(?:amp|lt|gt|quot|apos|#\d+|#x[0-9a-fA-F]+);)/g, "&amp;");
    const compileWarnings = diagnostics
      .filter((diagnostic) => diagnostic.severity === "warning")
      .map(formatDiagnostic);
    return { svg, warnings: [...fontWarnings, ...compileWarnings] };
  });
  compileQueue = render.catch(() => {});
  return render;
}

async function loadWorkspaceFonts() {
  const paths = [...managedWorkspaceFiles.keys()].filter(isWorkspaceFont);
  if (!paths.length) return { fonts: [], warnings: [] };
  const warnings = [];
  const fonts = [];
  const probe = createCompiler();
  const resolver = await probe.getFontResolver();
  for (const path of paths) {
    const bytes = accessModel.readAll(`/@memory/${syncedWorkspaceId}/${path}`);
    if (!bytes) continue;
    try {
      await resolver.addFontData(bytes);
      fonts.push(bytes);
    } catch (error) {
      warnings.push(`${path}: ${errorMessage(error)}`);
    }
  }
  await resolver.build(async () => {});
  return { fonts, warnings };
}

function formatDiagnostics(diagnostics) {
  return diagnostics.map(formatDiagnostic).join("\n").slice(0, 6000);
}

function formatDiagnostic(diagnostic) {
  const location = [diagnostic.package, diagnostic.path].filter(Boolean).join(":");
  const range = diagnostic.range ? `:${diagnostic.range}` : "";
  const heading = [location, range].filter(Boolean).join("");
  return `${heading ? `${heading}: ` : ""}${diagnostic.message || "Typst のコンパイルに失敗しました。"}`;
}

function errorMessage(error) {
  if (typeof error === "string") return error;
  if (error && typeof error.message === "string") return error.message;
  try { return JSON.stringify(error); } catch (_) { return "フォントを読み込めませんでした。"; }
}

function isWorkspaceFont(path) {
  return FONT_EXTENSIONS.has(path.split(".").pop().toLowerCase());
}

function pathContainsFonts(path) {
  return managedWorkspaceFiles.has(path)
    ? isWorkspaceFont(path)
    : [...managedWorkspaceFiles.keys()].some((file) => file.startsWith(`${path}/`) && isWorkspaceFont(file));
}

function insertManagedFile(root, path, bytes) {
  const previousSize = managedWorkspaceFiles.get(path) || 0;
  const nextTotalBytes = workspaceTotalBytes + bytes.byteLength - previousSize;
  const nextFileCount = managedWorkspaceFiles.size + (managedWorkspaceFiles.has(path) ? 0 : 1);
  if (nextFileCount > MAX_WORKSPACE_FILES || nextTotalBytes > MAX_WORKSPACE_BYTES) {
    throw new Error("Typst プレビューに同期できる上限を超えています (5,000ファイル / 256 MiB)。");
  }
  workspaceTotalBytes = nextTotalBytes;
  managedWorkspaceFiles.set(path, bytes.byteLength);
  accessModel.insertFile(`${root}/${path}`, bytes, new Date());
}

function removeManagedWorkspaceFiles() {
  if (syncedWorkspaceId) {
    const root = `/@memory/${syncedWorkspaceId}`;
    for (const path of managedWorkspaceFiles.keys()) accessModel.removeFile(`${root}/${path}`);
  }
  managedWorkspaceFiles.clear();
  changedWorkspaceFiles.clear();
  deletedWorkspacePaths.clear();
  workspaceTotalBytes = 0;
}

async function syncWorkspaceChanges(workspaceId, readFile) {
  const root = `/@memory/${workspaceId}`;
  for (const deletedPath of deletedWorkspacePaths) {
    for (const [path, size] of managedWorkspaceFiles) {
      if (path === deletedPath || path.startsWith(`${deletedPath}/`)) {
        accessModel.removeFile(`${root}/${path}`);
        managedWorkspaceFiles.delete(path);
        workspaceTotalBytes -= size;
      }
    }
  }
  deletedWorkspacePaths.clear();

  for (const path of changedWorkspaceFiles) {
    const bytes = await readFile(path);
    insertManagedFile(root, path, bytes);
    if (isWorkspaceFont(path)) {
      compilerNeedsReset = true;
      workspaceFontsNeedReload = true;
    }
  }
  changedWorkspaceFiles.clear();
}

async function syncWorkspace(workspaceId, listDirectory, readFile) {
  const directories = [""];
  const files = [];
  let totalBytes = 0;
  for (let index = 0; index < directories.length; index += 1) {
    const entries = await listDirectory(directories[index]);
    for (const entry of entries) {
      if (entry.directory) {
        directories.push(entry.path);
      } else {
        totalBytes += entry.size;
        if (files.length >= MAX_WORKSPACE_FILES || totalBytes > MAX_WORKSPACE_BYTES) {
          throw new Error("Typst プレビューに同期できる上限を超えています (5,000ファイル / 256 MiB)。");
        }
        files.push(entry);
      }
    }
  }

  const root = `/@memory/${workspaceId}`;
  for (let index = 0; index < files.length; index += 4) {
    await Promise.all(files.slice(index, index + 4).map(async (file) => {
      const bytes = await readFile(file.path);
      insertManagedFile(root, file.path, bytes);
    }));
  }
}
