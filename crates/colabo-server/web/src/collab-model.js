export function idKey(id) {
  return id ? String(id.actor) + ":" + String(id.clock) : "";
}

function equalId(a, b) {
  return !!a && !!b && Number(a.actor) === Number(b.actor) && Number(a.clock) === Number(b.clock);
}

function compareIds(a, b) {
  var actorDiff = Number(a.actor) - Number(b.actor);
  return actorDiff || (Number(a.clock) - Number(b.clock));
}

export function inList(list, id) {
  return (list || []).some(function (entry) { return equalId(entry, id); });
}

export function item(actorId, clock) {
  return { actor: Number(actorId), clock: Number(clock) };
}

export function visibleModel(docModel) {
  var nodes = new Map();
  var children = new Map();
  var groups = new Map();
  var rootHasChildren = false;
  var inactive = new Set((docModel.inactive_transactions || []).map(idKey));
  var deleted = new Map();
  (docModel.deletion_tags || []).forEach(function (tag) {
    var key = idKey(tag.target);
    if (!deleted.has(key)) deleted.set(key, []);
    deleted.get(key).push(tag.transaction);
  });
  function parentKey(value) { return value ? idKey(value) : ""; }
  function addChild(parent, child) {
    var key = parent || "";
    if (key) {
      var parentNode = nodes.get(key);
      if (parentNode) parentNode.hasChildren = true;
    }
    else rootHasChildren = true;
    if (!children.has(key)) children.set(key, []);
    children.get(key).push(child);
  }
  function groupKey(left, right) { return parentKey(left) + "|" + parentKey(right); }
  (docModel.chunks || []).forEach(function (chunk) {
    var parent = chunk.anchor.left ? idKey(chunk.anchor.left) : "";
    Array.from(chunk.text).forEach(function (value, offset) {
      var id = item(chunk.start.actor, Number(chunk.start.clock) + offset);
      var key = idKey(id);
      nodes.set(key, { id: id, value: value, parent: parent, createdBy: chunk.transaction });
      addChild(parent, key);
      parent = key;
    });
    var key = groupKey(chunk.anchor.left, chunk.anchor.right);
    if (!groups.has(key)) groups.set(key, { parent: chunk.anchor.left ? idKey(chunk.anchor.left) : "", roots: [] });
    groups.get(key).roots.push(idKey(chunk.start));
  });
  var edges = new Map();
  function edge(parent, from, to) {
    if (!edges.has(parent)) edges.set(parent, new Map());
    var source = edges.get(parent);
    if (!source.has(from)) source.set(from, new Set());
    source.get(from).add(to);
  }
  function topChild(parent, before) {
    if (!before) return "";
    var current = idKey(before);
    while (current) {
      var node = nodes.get(current);
      if (!node) return "";
      if (node.parent === (parent ? idKey(parent) : "")) return current;
      current = node.parent;
    }
    return "";
  }
  (docModel.chunks || []).forEach(function (chunk) {
    var target = topChild(chunk.anchor.left, chunk.anchor.right);
    var parent = chunk.anchor.left ? idKey(chunk.anchor.left) : "";
    var start = idKey(chunk.start);
    if (target && target !== start) edge(parent, start, target);
  });
  groups.forEach(function (group) {
    var roots = group.roots.slice().sort(function (a, b) { return compareIds(nodes.get(a).id, nodes.get(b).id); });
    roots = roots.filter(function (value, index) { return index === 0 || value !== roots[index - 1]; });
    for (var index = 1; index < roots.length; index += 1) edge(group.parent, roots[index - 1], roots[index]);
  });
  function orderSiblings(siblings, parent) {
    var members = Array.from(new Set(siblings));
    var degree = new Map();
    members.forEach(function (key) { degree.set(key, 0); });
    var bySource = edges.get(parent);
    if (bySource) bySource.forEach(function (targets, from) {
      if (!degree.has(from)) return;
      targets.forEach(function (to) {
        if (degree.has(to)) degree.set(to, degree.get(to) + 1);
      });
    });
    var ready = members.filter(function (key) { return degree.get(key) === 0; });
    ready.sort(function (a, b) { return compareIds(nodes.get(a).id, nodes.get(b).id); });
    var ordered = [];
    while (ready.length) {
      var current = ready.shift();
      ordered.push(current);
      var targets = bySource && bySource.get(current);
      if (!targets) continue;
      targets.forEach(function (target) {
        if (!degree.has(target)) return;
        degree.set(target, degree.get(target) - 1);
        if (degree.get(target) === 0) {
          ready.push(target);
          ready.sort(function (a, b) { return compareIds(nodes.get(a).id, nodes.get(b).id); });
        }
      });
    }
    if (ordered.length !== members.length) {
      ordered = members.slice().sort(function (a, b) { return compareIds(nodes.get(a).id, nodes.get(b).id); });
    }
    return ordered;
  }
  children.forEach(function (siblings, parent) {
    children.set(parent, orderSiblings(siblings, parent));
  });
  var visible = [];
  var ids = [];
  var textParts = [];
  var visibleCount = 0;
  var allCount = 0;
  var textOffset = 0;
  var hasHidden = false;
  var stack = (children.get("") || []).slice().reverse();
  while (stack.length) {
    var key = stack.pop();
    var node = nodes.get(key);
    if (!node) continue;
    var inserted = !inactive.has(idKey(node.createdBy));
    var tags = deleted.get(key) || [];
    var isDeleted = tags.some(function (tagTx) { return !inactive.has(idKey(tagTx)); });
    node.visible = inserted && !isDeleted;
    node.visibleOffset = visibleCount;
    node.allOffset = allCount;
    allCount += 1;
    if (node.visible) {
      visible.push({ id: node.id, value: node.value, visible: true, textOffset: textOffset, node: node });
      ids.push(node.id);
      textParts.push(node.value);
      visibleCount += 1;
      textOffset += node.value.length;
    } else {
      hasHidden = true;
    }
    var descendants = children.get(key) || [];
    for (var index = descendants.length - 1; index >= 0; index -= 1) stack.push(descendants[index]);
  }
  return {
    visible: visible,
    ids: ids,
    text: textParts.join(""),
    nodes: nodes,
    groups: groups,
    rootHasChildren: rootHasChildren,
    allCount: allCount,
    hasHidden: hasHidden
  };
}

function containsItem(docModel, id) {
  if (!id) return false;
  return (docModel.chunks || []).some(function (chunk) {
    if (Number(chunk.start.actor) !== Number(id.actor)) return false;
    var start = Number(chunk.start.clock);
    var end = start + Array.from(chunk.text).length;
    return Number(id.clock) >= start && Number(id.clock) < end;
  });
}

export function canRebase(docModel, transaction) {
  return transaction.ops.every(function (op) {
    if (op.TextInsert) {
      var anchor = op.TextInsert.anchor;
      return (!anchor.left || containsItem(docModel, anchor.left)) &&
        (!anchor.right || containsItem(docModel, anchor.right));
    }
    if (op.TextDelete) return op.TextDelete.targets.every(function (id) { return containsItem(docModel, id); });
    return op.SetActive && inList(docModel.edit_transactions, op.SetActive.target);
  });
}

export function applyTransaction(docModel, transaction) {
  var operations = transaction.ops || [];
  for (var index = 0; index < operations.length; index += 1) {
    var op = operations[index];
    if (op.TextInsert) {
      var value = op.TextInsert;
      docModel.chunks.push({ start: value.start, anchor: value.anchor, text: value.text, transaction: transaction.id });
    } else if (op.TextDelete) {
      op.TextDelete.targets.forEach(function (target) {
        var duplicate = docModel.deletion_tags.some(function (tag) {
          return equalId(tag.target, target) && equalId(tag.transaction, transaction.id);
        });
        if (!duplicate) docModel.deletion_tags.push({ target: target, transaction: transaction.id });
      });
    } else if (op.SetActive) {
      var targetKey = idKey(op.SetActive.target);
      var found = inList(docModel.inactive_transactions, op.SetActive.target);
      if (op.SetActive.active && found) {
        docModel.inactive_transactions = docModel.inactive_transactions.filter(function (id) { return idKey(id) !== targetKey; });
      } else if (!op.SetActive.active && !found) {
        docModel.inactive_transactions.push(op.SetActive.target);
      }
    }
  }
  docModel.transactions.push(transaction.id);
  if (!(operations.length === 1 && operations[0].SetActive)) docModel.edit_transactions.push(transaction.id);
}

export function maxClockInDocument(docModel, actor) {
  var maximum = 0;
  (docModel.transactions || []).forEach(function (id) {
    if (Number(id.actor) === actor) maximum = Math.max(maximum, Number(id.clock));
  });
  (docModel.chunks || []).forEach(function (chunk) {
    if (Number(chunk.start.actor) === actor) {
      maximum = Math.max(maximum, Number(chunk.start.clock) + Array.from(chunk.text).length - 1);
    }
  });
  return maximum;
}

export function maxClockInTransaction(transaction, actor) {
  var maximum = Number(transaction.id.clock);
  (transaction.ops || []).forEach(function (op) {
    if (op.TextInsert && Number(op.TextInsert.start.actor) === actor) {
      maximum = Math.max(maximum, Number(op.TextInsert.start.clock) + Array.from(op.TextInsert.text).length - 1);
    }
  });
  return maximum;
}
