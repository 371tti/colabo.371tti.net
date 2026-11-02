const MAX_LAYOUT_DEPTH = 8;
const MAX_EDITOR_GROUPS = 12;

export function createEditorGroup(id = "editor-1") {
  return { type: "group", id, tabs: [], activePath: null };
}

export function editorGroups(layout, groups = []) {
  if (layout?.type === "group") groups.push(layout);
  else if (layout?.type === "split") layout.children.forEach((child) => editorGroups(child, groups));
  return groups;
}

export function findEditorGroup(layout, id) {
  if (layout?.type === "group") return layout.id === id ? layout : null;
  if (layout?.type === "split") {
    return findEditorGroup(layout.children[0], id) || findEditorGroup(layout.children[1], id);
  }
  return null;
}

export function editorGroupForPath(layout, path) {
  return editorGroups(layout).find((group) => group.tabs.includes(path)) || null;
}

export function splitEditorGroup(layout, groupId, direction, side, newGroup) {
  let inserted = false;
  function visit(node) {
    if (node?.type === "group" && node.id === groupId) {
      inserted = true;
      const before = side === "left" || side === "top";
      return {
        type: "split",
        id: `split-${newGroup.id}`,
        direction,
        ratio: 0.5,
        children: before ? [newGroup, node] : [node, newGroup],
      };
    }
    if (node?.type !== "split") return node;
    return { ...node, children: node.children.map(visit) };
  }
  const next = visit(layout);
  return inserted ? next : layout;
}

export function removeEditorGroup(layout, groupId) {
  function visit(node) {
    if (node?.type === "group") return node.id === groupId ? null : node;
    if (node?.type !== "split") return null;
    const [first, second] = node.children.map(visit);
    if (!first) return second;
    if (!second) return first;
    return { ...node, children: [first, second] };
  }
  return visit(layout) || createEditorGroup();
}

export function loadEditorLayout(storageKey) {
  const fallback = { layout: createEditorGroup(), focusedGroupId: "editor-1" };
  if (!storageKey) return fallback;
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey) || "null");
    if (!saved || typeof saved !== "object") return fallback;
    const seenPaths = new Set();
    const layout = normalizeNode(saved.layout, seenPaths, 0, { groups: 0 });
    const groups = editorGroups(layout);
    const focusedGroupId = groups.some((group) => group.id === saved.focusedGroupId)
      ? saved.focusedGroupId
      : groups[0]?.id || "editor-1";
    return { layout, focusedGroupId };
  } catch (_) {
    return fallback;
  }
}

export function saveEditorLayout(storageKey, layout, focusedGroupId) {
  if (!storageKey) return;
  try {
    localStorage.setItem(storageKey, JSON.stringify({ layout, focusedGroupId }));
  } catch (_) {}
}

function normalizeNode(node, seenPaths, depth, counts) {
  if (depth > MAX_LAYOUT_DEPTH || counts.groups >= MAX_EDITOR_GROUPS) return createEditorGroup();
  if (node?.type === "split" && Array.isArray(node.children) && node.children.length === 2) {
    const direction = node.direction === "vertical" ? "vertical" : "horizontal";
    const first = normalizeNode(node.children[0], seenPaths, depth + 1, counts);
    const second = normalizeNode(node.children[1], seenPaths, depth + 1, counts);
    const ratio = Number(node.ratio);
    return {
      type: "split",
      id: typeof node.id === "string" && /^[\w-]{1,100}$/.test(node.id) ? node.id : `split-${counts.groups + 1}`,
      direction,
      ratio: Number.isFinite(ratio) ? Math.min(0.8, Math.max(0.2, ratio)) : 0.5,
      children: [first, second],
    };
  }
  counts.groups += 1;
  const id = typeof node?.id === "string" && /^[\w-]{1,80}$/.test(node.id)
    ? node.id
    : `editor-${counts.groups}`;
  const tabs = [];
  if (Array.isArray(node?.tabs)) {
    for (const path of node.tabs) {
      if (typeof path !== "string" || !path.length || path.length > 512 || seenPaths.has(path)) continue;
      seenPaths.add(path);
      tabs.push(path);
      if (tabs.length >= 100) break;
    }
  }
  return {
    type: "group",
    id,
    tabs,
    activePath: tabs.includes(node?.activePath) ? node.activePath : tabs[0] || null,
  };
}
