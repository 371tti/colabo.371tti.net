import { applyTransaction, canRebase, idKey, inList, item, maxClockInDocument, maxClockInTransaction, visibleModel } from "./collab-model.js";
function collaboratorColorIndex(identityId) {
  var hash = 0;
  for (var index = 0; index < identityId.length; index += 1) {
    hash = (Math.imul(hash, 31) + identityId.charCodeAt(index)) >>> 0;
  }
  return hash % 6;
}

export function copySessionData(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("session data must be a JSON object");
  }
  const copy = JSON.parse(JSON.stringify(value));
  if (!copy || typeof copy !== "object" || Array.isArray(copy)) {
    throw new TypeError("session data must be a JSON object");
  }
  return copy;
}

export function startCollab(monaco, editor, root, options) {
"use strict";
      options = options || {};
      var editorHost = root.querySelector('[data-role="editor"]');
      var expiredNotice = root.querySelector('[data-role="session-expired"]');
      var toast = root.querySelector('[data-role="toast"]');
      var toastTimer = 0;
      var disposed = false;
      var sessionDataListeners = new Set();

      function validActor(value) {
        var number = Number(value);
        return Number.isSafeInteger(number) && number > 0 && number <= 4503599627370496;
      }
      function makeActor() {
        var bytes = new Uint32Array(2);
        window.crypto.getRandomValues(bytes);
        return String((bytes[0] & 1048575) * 4294967296 + bytes[1] + 1);
      }
      var params = new URLSearchParams(window.location.search);
      var documentId = options.sessionId;
      var actor = params.get("actor");
      actor = validActor(options.actorId) ? Number(options.actorId) : validActor(actor) ? Number(actor) : Number(makeActor());
      var editorIdentity = {
        id: options.userId,
        name: options.userName
      };
      var defaultUndoHistoryLimit = 100;
      root.dataset.localColor = String(collaboratorColorIndex(editorIdentity.id));

      function emptyDocument() {
        return { chunks: [], deletion_tags: [], inactive_transactions: [], transactions: [], edit_transactions: [] };
      }
      var state = {
        actor: actor,
        presenceOrder: 1,
        localActivityOrder: 1,
        nextClock: 1,
        lastSeq: 0,
        confirmed: emptyDocument(),
        visible: emptyDocument(),
        pending: [],
        undo: [],
        redo: [],
        undoHistoryLimit: defaultUndoHistoryLimit,
        applicationData: options.sessionData || {},
        peers: new Map(),
        cursorWidgets: new Map(),
        editorDecorations: [],
        socket: null,
        everWelcomed: false,
        composing: false,
        rendering: false,
        lastStableSelection: null,
        selectionHistory: new Map(),
        selectionRestoreRevision: 0,
        compositionBase: null,
        reconnectTimer: 0,
        presenceTimer: 0,
        localPresenceFrame: 0,
        reconnectDelay: 900,
        sessionExpired: false
      };
      var cachedVisibleDocument = null;
      var cachedVisibleModel = null;
      var editorText = editor.getValue();

      function clone(value) {
        return JSON.parse(JSON.stringify(value));
      }
      function currentVisibleModel() {
        if (cachedVisibleDocument !== state.visible) {
          cachedVisibleDocument = state.visible;
          cachedVisibleModel = visibleModel(state.visible);
        }
        return cachedVisibleModel;
      }
      function invalidateVisibleModel() {
        cachedVisibleDocument = null;
        cachedVisibleModel = null;
      }
      function anchorAt(ids, index) {
        if (index < 0 || index > ids.length) return null;
        return {
          left: index > 0 ? ids[index - 1] : null,
          right: index < ids.length ? ids[index] : null
        };
      }
      function scalarOffset(model, offset) {
        if (offset <= 0 || !model.visible.length) return 0;
        if (offset >= model.text.length) return model.visible.length;
        var low = 0;
        var high = model.visible.length;
        while (low < high) {
          var middle = (low + high) >> 1;
          if (model.visible[middle].textOffset <= offset) low = middle + 1;
          else high = middle;
        }
        return Math.max(0, low - 1);
      }
      function scalarOffsetInText(text, offset) {
        var count = 0;
        for (var index = 0; index < offset;) {
          var codePoint = text.codePointAt(index);
          index += codePoint > 0xffff ? 2 : 1;
          count += 1;
        }
        return count;
      }
      function scalarLength(text) {
        return scalarOffsetInText(text, text.length);
      }
      function utf16Offset(model, scalarIndex) {
        scalarIndex = Math.max(0, Math.min(scalarIndex, model.visible.length));
        return scalarIndex === model.visible.length
          ? model.text.length
          : model.visible[scalarIndex].textOffset;
      }
      function allocateId() {
        if (!Number.isSafeInteger(state.nextClock) || state.nextClock >= Number.MAX_SAFE_INTEGER) {
          showToast("この参加者の編集 ID を使い切りました。ページを再読み込みしてください。");
          return null;
        }
        var id = { actor: state.actor, clock: state.nextClock };
        state.nextClock += 1;
        return id;
      }
      function historyAction(transaction) {
        var active = (transaction.ops || []).find(function (op) { return !!op.SetActive; });
        return active ? { target: active.SetActive.target, active: active.SetActive.active } : null;
      }
      function rememberSelectionHistory(transaction) {
        if (!transaction || historyAction(transaction) || Number(transaction.actor) !== state.actor) return;
        if (!Object.prototype.hasOwnProperty.call(transaction, "selection_before") &&
            !Object.prototype.hasOwnProperty.call(transaction, "selection_after")) return;
        state.selectionHistory.set(idKey(transaction.id), {
          before: transaction.selection_before ? clone(transaction.selection_before) : null,
          after: transaction.selection_after ? clone(transaction.selection_after) : null
        });
      }
      function trimUndoHistory() {
        var excess = state.undo.length + state.redo.length - state.undoHistoryLimit;
        if (excess <= 0) return;
        var expiredUndo = state.undo.splice(0, Math.min(excess, state.undo.length));
        expiredUndo.forEach(function (id) { state.selectionHistory.delete(idKey(id)); });
        excess -= expiredUndo.length;
        if (excess > 0) {
          var expiredRedo = state.redo.splice(0, excess);
          expiredRedo.forEach(function (id) { state.selectionHistory.delete(idKey(id)); });
        }
      }
      function pruneSelectionHistory() {
        var retained = new Set();
        state.undo.concat(state.redo).forEach(function (id) { retained.add(idKey(id)); });
        state.selectionHistory.forEach(function (_, id) {
          if (!retained.has(id)) state.selectionHistory.delete(id);
        });
      }
      function applyOptimisticHistory(transaction) {
        var action = historyAction(transaction);
        if (!action) {
          rememberSelectionHistory(transaction);
          state.undo.push(transaction.id);
          state.redo.forEach(function (id) { state.selectionHistory.delete(idKey(id)); });
          state.redo = [];
          trimUndoHistory();
        } else if (action.active) {
          if (idKey(state.redo[state.redo.length - 1]) === idKey(action.target)) state.redo.pop();
          state.undo.push(action.target);
        } else {
          if (idKey(state.undo[state.undo.length - 1]) === idKey(action.target)) state.undo.pop();
          state.redo.push(action.target);
        }
      }
      function rollbackHistory(transaction) {
        var action = historyAction(transaction);
        if (!action) {
          state.undo = state.undo.filter(function (id) { return idKey(id) !== idKey(transaction.id); });
          state.redo = state.redo.filter(function (id) { return idKey(id) !== idKey(transaction.id); });
          state.selectionHistory.delete(idKey(transaction.id));
        } else if (!action.active) {
          state.redo = state.redo.filter(function (id) { return idKey(id) !== idKey(action.target); });
          if (!inList(state.undo, action.target)) state.undo.push(action.target);
        } else {
          state.undo = state.undo.filter(function (id) { return idKey(id) !== idKey(action.target); });
          if (!inList(state.redo, action.target)) state.redo.push(action.target);
        }
      }
      function applyCommittedHistory(transaction) {
        var action = historyAction(transaction);
        if (!action) {
          state.undo.push(transaction.id);
          state.redo.forEach(function (id) { state.selectionHistory.delete(idKey(id)); });
          state.redo = [];
          trimUndoHistory();
        } else if (!action.active) {
          if (idKey(state.undo[state.undo.length - 1]) !== idKey(action.target)) {
            showToast("Undo 履歴が同期されていません。再接続しています。");
            return;
          }
          state.undo.pop();
          state.redo.push(action.target);
        } else {
          if (idKey(state.redo[state.redo.length - 1]) !== idKey(action.target)) {
            showToast("Redo 履歴が同期されていません。再接続しています。");
            return;
          }
          state.redo.pop();
          state.undo.push(action.target);
        }
      }
      function rebuildVisible() {
        var rebuilt = clone(state.confirmed);
        var kept = [];
        state.pending.forEach(function (transaction) {
          if (canRebase(rebuilt, transaction)) {
            applyTransaction(rebuilt, transaction);
            kept.push(transaction);
          } else {
            rollbackHistory(transaction);
          }
        });
        state.pending = kept;
        state.visible = rebuilt;
      }
      function patchVisibleModel(model, edits, transaction) {
        if (edits.length !== 1) return false;
        var edit = edits[0];
        var removeCount = edit.end - edit.start;
        var insertCount = edit.insertCount;
        if (removeCount && insertCount) return false;
        var hadHidden = model.hasHidden;
        var insertionIndex = model.allCount;
        var parentKey = edit.anchor && edit.anchor.left ? idKey(edit.anchor.left) : "";
        if (insertCount > 512) return false;
        if (insertCount) {
          var anchorGroup = parentKey + "|" + idKey(edit.anchor.right);
          if (model.groups.has(anchorGroup)) return false;
          if (edit.anchor.right) {
            var node = model.nodes.get(idKey(edit.anchor.right));
            var target = null;
            while (node) {
              if (node.parent === parentKey) { target = node; break; }
              node = node.parent ? model.nodes.get(node.parent) : null;
            }
            if (!target || target.visibleOffset !== edit.start) return false;
            insertionIndex = target.allOffset;
          } else {
            var parentNode = parentKey ? model.nodes.get(parentKey) : null;
            if (parentNode ? parentNode.hasChildren : model.rootHasChildren) return false;
          }
        }
        if (insertCount && model.hasHidden && insertionIndex < model.allCount) {
          var hiddenBeforeTarget = false;
          model.nodes.forEach(function (node) {
            if (!node.visible && node.visibleOffset === edit.start && node.allOffset < insertionIndex) {
              hiddenBeforeTarget = true;
            }
          });
          if (hiddenBeforeTarget) return false;
        }
        var textStart = edit.start < model.visible.length
          ? model.visible[edit.start].textOffset
          : model.text.length;
        var textEnd = edit.end < model.visible.length
          ? model.visible[edit.end].textOffset
          : model.text.length;
        var removedEntries = model.visible.slice(edit.start, edit.end);
        if (removeCount) {
          model.hasHidden = true;
          if (hadHidden) {
            model.nodes.forEach(function (node) {
              if (node.visibleOffset >= edit.end) node.visibleOffset -= removeCount;
              else if (node.visibleOffset > edit.start) node.visibleOffset = edit.start;
            });
          }
          removedEntries.forEach(function (entry) {
            entry.node.visible = false;
            entry.node.visibleOffset = edit.start;
          });
        }
        if (insertCount && hadHidden && insertionIndex < model.allCount) {
          model.nodes.forEach(function (node) {
            if (node.visible ? node.visibleOffset >= edit.start : node.allOffset >= insertionIndex) {
              node.visibleOffset += insertCount;
            }
            if (node.allOffset >= insertionIndex) node.allOffset += insertCount;
          });
        }
        if (insertCount) {
          var parentNode = parentKey ? model.nodes.get(parentKey) : null;
          if (parentNode) parentNode.hasChildren = true;
          else model.rootHasChildren = true;
          model.groups.set(parentKey + "|" + idKey(edit.anchor.right), null);
        }
        model.ids.splice(edit.start, removeCount);
        model.visible.splice(edit.start, removeCount);
        var insertedIds = [];
        var insertedEntries = [];
        var previousKey = parentKey;
        Array.from(edit.text).forEach(function (value, index) {
          var id = item(edit.actor || state.actor, edit.startClock + index);
          var key = idKey(id);
          model.nodes.set(key, {
            id: id,
            value: value,
            parent: previousKey,
            createdBy: transaction.id,
            hasChildren: index + 1 < insertCount,
            visible: true,
            visibleOffset: edit.start + index,
            allOffset: insertionIndex + index
          });
          insertedIds.push(id);
          insertedEntries.push({
            id: id,
            value: value,
            visible: true,
            textOffset: 0,
            node: model.nodes.get(key)
          });
          previousKey = key;
        });
        model.ids.splice(edit.start, 0, ...insertedIds);
        model.visible.splice(edit.start, 0, ...insertedEntries);
        model.text = model.text.slice(0, textStart) + edit.text + model.text.slice(textEnd);
        model.allCount += insertCount;
        var textOffset = textStart;
        for (var visibleIndex = edit.start; visibleIndex < model.visible.length; visibleIndex += 1) {
          var entry = model.visible[visibleIndex];
          entry.textOffset = textOffset;
          if (!hadHidden) {
            entry.node.visibleOffset = visibleIndex;
            if (insertCount) entry.node.allOffset = visibleIndex;
          }
          textOffset += entry.value.length;
        }
        return true;
      }
      function enqueue(transaction, patch) {
        applyTransaction(state.visible, transaction);
        var patched = !!(patch && patchVisibleModel(patch.model, patch.edits, transaction));
        if (patched) {
          cachedVisibleDocument = state.visible;
          cachedVisibleModel = patch.model;
        } else {
          invalidateVisibleModel();
        }
        state.pending.push(transaction);
        applyOptimisticHistory(transaction);
        send({ type: "submit", data: { transaction: transaction } });
        return patched;
      }
      function patchFromTransaction(model, transaction) {
        var insert = null;
        var targets = [];
        var valid = true;
        (transaction.ops || []).forEach(function (op) {
          if (op.TextInsert && !insert) insert = op.TextInsert;
          else if (op.TextDelete && !targets.length) targets = op.TextDelete.targets || [];
          else valid = false;
        });
        if (!valid || (!insert && !targets.length)) return null;
        var start = insert ? positionFromAnchor(insert.anchor, model) : 0;
        var end = start;
        if (targets.length) {
          var positions = targets.map(function (id) {
            var node = model.nodes.get(idKey(id));
            return node && node.visible ? node.visibleOffset : -1;
          }).sort(function (left, right) { return left - right; });
          if (positions[0] < 0 || positions.some(function (position, index) {
            return position !== positions[0] + index;
          })) return null;
          start = positions[0];
          end = start + positions.length;
          var targetKeys = new Set(targets.map(idKey));
          if (model.ids.slice(start, end).some(function (id) { return !targetKeys.has(idKey(id)); })) return null;
          if (insert && positionFromAnchor(insert.anchor, model) !== start) return null;
        }
        var startOffset = utf16Offset(model, start);
        var endOffset = utf16Offset(model, end);
        var edit = {
          start: start,
          end: end,
          startOffset: startOffset,
          endOffset: endOffset,
          text: insert ? insert.text : "",
          insertCount: insert ? scalarLength(insert.text) : 0,
          actor: insert ? Number(insert.start.actor) : state.actor,
          startClock: insert ? Number(insert.start.clock) : 0,
          anchor: insert && insert.anchor
        };
        return {
          model: model,
          edits: [edit],
          previousText: model.text,
          textChanges: [{ rangeOffset: startOffset, rangeLength: endOffset - startOffset, text: edit.text }]
        };
      }
      function textChangesBetween(oldModel, nextText) {
        var before = Array.from(oldModel.text);
        var after = Array.from(nextText);
        var prefix = 0;
        while (prefix < before.length && prefix < after.length && before[prefix] === after[prefix]) prefix += 1;
        var suffix = 0;
        while (suffix < before.length - prefix && suffix < after.length - prefix &&
               before[before.length - 1 - suffix] === after[after.length - 1 - suffix]) suffix += 1;
        var removeEnd = before.length - suffix;
        var insertEnd = after.length - suffix;
        if (prefix === removeEnd && prefix === insertEnd) return [];
        var startOffset = utf16Offset(oldModel, prefix);
        var endOffset = utf16Offset(oldModel, removeEnd);
        return [{
          rangeOffset: startOffset,
          rangeLength: endOffset - startOffset,
          text: after.slice(prefix, insertEnd).join("")
        }];
      }
      function normalizeChanges(model, changes) {
        return (changes || []).map(function (change) {
          var startOffset = Math.max(0, Math.min(model.text.length, Number(change.rangeOffset) || 0));
          var rangeLength = Math.max(0, Number(change.rangeLength) || 0);
          var endOffset = Math.min(model.text.length, startOffset + rangeLength);
          return {
            startOffset: startOffset,
            endOffset: endOffset,
            start: scalarOffset(model, startOffset),
            end: scalarOffset(model, endOffset),
            text: String(change.text || "")
          };
        }).sort(function (left, right) { return left.startOffset - right.startOffset; });
      }
      function anchorAfterChanges(model, edits, offset) {
        var delta = 0;
        for (var index = 0; index < edits.length; index += 1) {
          var edit = edits[index];
          var newStart = edit.startOffset + delta;
          var newEnd = newStart + edit.text.length;
          if (offset < newStart) break;
          if (offset <= newEnd) {
            var insertedOffset = scalarOffsetInText(edit.text, Math.max(0, offset - newStart));
            var left = insertedOffset
              ? item(state.actor, edit.startClock + insertedOffset - 1)
              : (edit.start > 0 ? model.ids[edit.start - 1] : null);
            var right = insertedOffset < edit.insertCount
              ? item(state.actor, edit.startClock + insertedOffset)
              : (edit.end < model.ids.length ? model.ids[edit.end] : null);
            return { left: left, right: right };
          }
          delta += edit.text.length - (edit.endOffset - edit.startOffset);
        }
        return anchorAt(model.ids, scalarOffset(model, offset - delta));
      }
      function captureSelectionAfterChanges(model, edits) {
        var editorModel = editor.getModel();
        var selection = editor.getSelection();
        if (!editorModel || !selection) return null;
        var startOffset = editorModel.getOffsetAt({
          lineNumber: selection.selectionStartLineNumber,
          column: selection.selectionStartColumn
        });
        var endOffset = editorModel.getOffsetAt({
          lineNumber: selection.positionLineNumber,
          column: selection.positionColumn
        });
        return {
          anchor: anchorAfterChanges(model, edits, startOffset),
          head: anchorAfterChanges(model, edits, endOffset)
        };
      }
      function newTextTransaction(oldModel, changes) {
        var edits = normalizeChanges(oldModel, changes);
        if (edits.some(function (edit, index) {
          return index > 0 && edit.startOffset < edits[index - 1].endOffset;
        })) {
          edits = normalizeChanges(oldModel, textChangesBetween(oldModel, editor.getValue()));
        }
        edits.forEach(function (edit) { edit.insertCount = scalarLength(edit.text); });
        var insertCount = edits.reduce(function (count, edit) { return count + edit.insertCount; }, 0);
        if (!edits.some(function (edit) { return edit.start !== edit.end || edit.insertCount; })) return null;
        if (insertCount && state.nextClock > Number.MAX_SAFE_INTEGER - insertCount - 1) {
          showToast("編集 ID の範囲を使い切りました。ページを再読み込みしてください。");
          return null;
        }
        var id = allocateId();
        if (!id) return null;
        var ops = [];
        edits.forEach(function (edit) {
          if (edit.end > edit.start) {
            ops.push({ TextDelete: { targets: oldModel.ids.slice(edit.start, edit.end) } });
          }
          if (edit.insertCount) {
            edit.actor = state.actor;
            edit.startClock = state.nextClock;
            state.nextClock += edit.insertCount;
            edit.anchor = anchorAt(oldModel.ids, edit.start);
            ops.push({
              TextInsert: {
                start: item(state.actor, edit.startClock),
                anchor: edit.anchor,
                text: edit.text
              }
            });
          }
        });
        var transaction = { id: id, actor: state.actor, ops: ops };
        transaction.selection_before = state.lastStableSelection
          ? clone(state.lastStableSelection)
          : null;
        transaction.selection_after = captureSelectionAfterChanges(oldModel, edits);
        return { transaction: transaction, model: oldModel, edits: edits };
      }
      function syncFromInput(baseModel, changes) {
        if (state.composing || !state.everWelcomed) return;
        var current = baseModel || currentVisibleModel();
        var edits = changes && changes.length ? changes : textChangesBetween(current, editor.getValue());
        var patch = newTextTransaction(current, edits);
        if (!patch) {
          if (editorText !== current.text) render();
          return;
        }
        var transaction = patch.transaction;
        var patched = enqueue(transaction, patch);
        if (patched) editorText = patch.model.text;
        if (transaction.selection_after) {
          state.lastStableSelection = clone(transaction.selection_after);
        }
        if (!patched) render();
        scheduleLocalPresenceRender();
        sendSessionDataSoon();
      }
      function makeHistoryTransaction(active) {
        var stack = active ? state.redo : state.undo;
        var target = stack[stack.length - 1];
        if (!target) return null;
        var id = allocateId();
        if (!id) return null;
        var transaction = {
          id: id,
          actor: state.actor,
          ops: [{ SetActive: { target: target, active: active } }]
        };
        var selection = state.selectionHistory.get(idKey(target));
        if (selection) transaction.restore_selection = clone(active ? selection.after : selection.before);
        return transaction;
      }
      function undo(redo) {
        var selection = captureSelection(currentVisibleModel());
        var transaction = makeHistoryTransaction(!!redo);
        if (!transaction) return;
        enqueue(transaction);
        render({ selection: transaction.restore_selection || selection });
        sendSessionDataSoon();
      }
      function send(message) {
        if (state.socket && state.socket.readyState === WebSocket.OPEN) {
          state.socket.send(JSON.stringify(message));
        }
      }
      function setConnection(status, text) {
        if (typeof options.onConnectionChange === "function") options.onConnectionChange(status, text);
      }
      function connect() {
        if (disposed || state.sessionExpired) return;
        if (state.socket && (state.socket.readyState === WebSocket.OPEN || state.socket.readyState === WebSocket.CONNECTING)) return;
        setConnection("connecting", state.everWelcomed ? "再接続しています" : "接続しています");
        var socketUrl;
        if (options.websocketPath) {
          var endpoint = new URL(options.websocketPath, window.location.href);
          endpoint.protocol = endpoint.protocol === "https:" ? "wss:" : "ws:";
          socketUrl = endpoint.toString();
        } else if (options.websocketUrl) {
          socketUrl = new URL(options.websocketUrl, window.location.href).toString();
        } else {
          var serverUrl = new URL(options.serverUrl || window.location.origin);
          serverUrl.protocol = serverUrl.protocol === "https:" ? "wss:" : "ws:";
          serverUrl.pathname = "/ws/" + encodeURIComponent(documentId);
          serverUrl.search = "";
          serverUrl.hash = "";
          socketUrl = serverUrl.toString();
        }
        var socket = new WebSocket(socketUrl);
        state.socket = socket;
        socket.addEventListener("open", function () {
          socket.send(JSON.stringify({ type: "hello", data: { actor: state.actor, identity: editorIdentity } }));
        });
        socket.addEventListener("message", function (event) {
          var message;
          try { message = JSON.parse(event.data); } catch (_) { showToast("サーバーから読めないメッセージを受信しました。"); return; }
          handleMessage(message);
        });
        socket.addEventListener("error", function () {
          try { socket.close(); } catch (_) {}
        });
        socket.addEventListener("close", function () {
          if (state.socket === socket) state.socket = null;
          if (disposed || state.sessionExpired) return;
          if (state.reconnectTimer) window.clearTimeout(state.reconnectTimer);
          setConnection("connecting", state.everWelcomed ? "再接続中" : "接続中");
          state.reconnectTimer = window.setTimeout(connect, state.reconnectDelay);
          state.reconnectDelay = Math.min(state.reconnectDelay * 1.5, 5000);
        });
      }
      function handleWelcome(data) {
        if (state.sessionExpired) return;
        var selection = captureSelection(currentVisibleModel());
        var previousPending = state.pending.slice();
        state.peers.clear();
        state.confirmed = clone(data.snapshot.document);
        state.visible = clone(state.confirmed);
        (state.confirmed.transactions || []).forEach(rememberSelectionHistory);
        state.lastSeq = Number(data.snapshot.seq);
        var configuredUndoLimit = Number(data.max_undo_transactions_per_actor);
        state.undoHistoryLimit = Number.isSafeInteger(configuredUndoLimit) && configuredUndoLimit > 0
          ? configuredUndoLimit
          : defaultUndoHistoryLimit;
        state.undo = (data.undo_stack || []).slice();
        state.redo = (data.redo_stack || []).slice();
        trimUndoHistory();
        (data.session_data || []).forEach(function (entry) {
          var peerActor = Number(entry.actor);
          var peerIdentity = entry.identity || { id: String(entry.actor), name: "参加者 " + entry.actor };
          state.peers.set(peerActor, {
            identity: peerIdentity,
            data: entry.data || {},
            activityOrder: ++state.presenceOrder
          });
        });
        notifySessionData({ type: "snapshot" });
        state.pending = [];
        var committedIds = new Set((state.confirmed.transactions || []).map(idKey));
        previousPending.forEach(function (transaction) {
          if (committedIds.has(idKey(transaction.id))) return;
          if (!canRebase(state.visible, transaction)) return;
          applyTransaction(state.visible, transaction);
          state.pending.push(transaction);
          applyOptimisticHistory(transaction);
        });
        pruneSelectionHistory();
        var maxClock = maxClockInDocument(state.confirmed, state.actor);
        state.pending.forEach(function (transaction) { maxClock = Math.max(maxClock, maxClockInTransaction(transaction, state.actor)); });
        state.nextClock = maxClock + 1;
        state.everWelcomed = true;
        state.reconnectDelay = 900;
        setConnection("connected", "接続済み");
        editor.updateOptions({ readOnly: false });
        editorHost.setAttribute("aria-busy", "false");
        render({ selection: selection });
        state.pending.forEach(function (transaction) {
          send({ type: "submit", data: { transaction: transaction } });
        });
        sendSessionDataSoon();
      }
      function receiveCommitted(committed) {
        var seq = Number(committed.seq);
        if (seq <= state.lastSeq) return;
        if (seq !== state.lastSeq + 1) {
          if (state.socket) state.socket.close();
          return;
        }
        var transaction = committed.transaction;
        var localPending = state.pending.some(function (pending) { return idKey(pending.id) === idKey(transaction.id); });
        applyTransaction(state.confirmed, transaction);
        if (localPending) {
          state.pending = state.pending.filter(function (pending) { return idKey(pending.id) !== idKey(transaction.id); });
          state.lastSeq = seq;
          return;
        }
        var model = currentVisibleModel();
        var selection = captureSelection(model);
        var patch = patchFromTransaction(model, transaction);
        applyTransaction(state.visible, transaction);
        if (patch && patchVisibleModel(model, patch.edits, transaction)) {
          cachedVisibleDocument = state.visible;
          cachedVisibleModel = model;
        } else {
          invalidateVisibleModel();
        }
        if (Number(transaction.actor) === state.actor) applyCommittedHistory(transaction);
        state.lastSeq = seq;
        render({
          selection: Number(transaction.actor) === state.actor && transaction.restore_selection || selection,
          previousText: patch && cachedVisibleModel === model ? patch.previousText : null,
          textChanges: patch && cachedVisibleModel === model ? patch.textChanges : null
        });
      }
      function handleMessage(message) {
        if (state.sessionExpired) return;
        var data = message.data || {};
        if (message.type === "session_expired") {
          expireSession();
        } else if (message.type === "welcome") {
          handleWelcome(data);
        } else if (message.type === "committed") {
          receiveCommitted(data.committed);
        } else if (message.type === "rejected") {
          rejectTransaction(data.tx_id, data.reason);
        } else if (message.type === "session_data_update") {
          var peerActor = Number(data.actor);
          var peerIdentity = data.identity || { id: String(peerActor), name: "参加者 " + peerActor };
          var peerData = data.data || {};
          state.presenceOrder += 1;
          state.peers.set(peerActor, {
            identity: peerIdentity,
            data: peerData,
            activityOrder: state.presenceOrder
          });
          notifySessionData({ type: "update", actor: peerActor, identity: peerIdentity, data: peerData });
          renderRemotePresence(currentVisibleModel());
        } else if (message.type === "session_data_left") {
          var leftActor = Number(data.actor);
          var leftPeer = state.peers.get(leftActor);
          state.peers.delete(leftActor);
          notifySessionData({
            type: "left",
            actor: leftActor,
            identity: data.identity || (leftPeer && leftPeer.identity) || { id: String(leftActor), name: "参加者 " + leftActor }
          });
          renderRemotePresence(currentVisibleModel());
        } else if (message.type === "protocol_error") {
          showToast(data.message || "通信エラーが発生しました。");
        }
      }
      function expireSession() {
        state.sessionExpired = true;
        state.pending = [];
        state.peers.clear();
        if (state.reconnectTimer) window.clearTimeout(state.reconnectTimer);
        if (state.presenceTimer) window.clearTimeout(state.presenceTimer);
        if (state.localPresenceFrame) window.cancelAnimationFrame(state.localPresenceFrame);
        state.reconnectTimer = 0;
        state.presenceTimer = 0;
        state.localPresenceFrame = 0;
        editor.updateOptions({ readOnly: true });
        editorHost.setAttribute("aria-busy", "false");
        editorHost.hidden = true;
        expiredNotice.hidden = false;
        setConnection("error", "セッションは終了しました");
        if (state.socket) {
          var socket = state.socket;
          state.socket = null;
          socket.close(1000, "session expired");
        }
      }
      function rejectTransaction(id, reason) {
        var selection = captureSelection(currentVisibleModel());
        var key = idKey(id);
        var rejected = state.pending.find(function (transaction) { return idKey(transaction.id) === key; });
        if (!rejected) return;
        state.pending = state.pending.filter(function (transaction) { return idKey(transaction.id) !== key; });
        rollbackHistory(rejected);
        rebuildVisible();
        render({ selection: selection });
        showToast("編集を反映できませんでした: " + String(reason || "transaction rejected"));
      }
      function captureSelection(model) {
        if (state.composing) return null;
        var editorModel = editor.getModel();
        var selection = editor.getSelection();
        if (!editorModel || !selection) return null;
        var startOffset = editorModel.getOffsetAt({
          lineNumber: selection.selectionStartLineNumber,
          column: selection.selectionStartColumn
        });
        var endOffset = editorModel.getOffsetAt({
          lineNumber: selection.positionLineNumber,
          column: selection.positionColumn
        });
        var anchor = scalarOffset(model, startOffset);
        var head = scalarOffset(model, endOffset);
        return {
          anchor: anchorAt(model.ids, Math.min(anchor, model.ids.length)),
          head: anchorAt(model.ids, Math.min(head, model.ids.length))
        };
      }
      function rememberCurrentSelection(model) {
        if (state.composing || editorText !== model.text) return;
        var selection = captureSelection(model);
        if (selection) state.lastStableSelection = selection;
      }
      function restoreEditorSelection(selection, model) {
        if (!selection) return;
        var start = Math.min(positionFromAnchor(selection.anchor, model), model.ids.length);
        var end = Math.min(positionFromAnchor(selection.head, model), model.ids.length);
        var editorModel = editor.getModel();
        if (!editorModel) return;
        var startPosition = editorModel.getPositionAt(utf16Offset(model, start));
        var endPosition = editorModel.getPositionAt(utf16Offset(model, end));
        editor.setSelection(new monaco.Selection(
          startPosition.lineNumber,
          startPosition.column,
          endPosition.lineNumber,
          endPosition.column
        ));
        editor.revealRangeInCenterIfOutsideViewport(new monaco.Range(
          startPosition.lineNumber,
          startPosition.column,
          endPosition.lineNumber,
          endPosition.column
        ));
      }
      function positionFromAnchor(anchor, model) {
        if (!anchor) return model.ids.length;
        var left = anchor.left && model.nodes.get(idKey(anchor.left));
        if (left) return left.visibleOffset + Number(left.visible);
        var right = anchor.right && model.nodes.get(idKey(anchor.right));
        return right ? right.visibleOffset : model.ids.length;
      }
      function render(options) {
        options = options || {};
        var model = currentVisibleModel();
        var selection = options.selection || captureSelection(model);
        var textChanged = editorText !== model.text;
        if (!state.composing && (textChanged || !!options.selection)) {
          var restoreRevision = ++state.selectionRestoreRevision;
          state.rendering = true;
          if (textChanged) {
            var editorModel = editor.getModel();
            var edits = options.textChanges && options.previousText === editorText && editorModel
              ? options.textChanges.map(function (change) {
                var start = editorModel.getPositionAt(change.rangeOffset);
                var end = editorModel.getPositionAt(change.rangeOffset + change.rangeLength);
                return {
                  range: new monaco.Range(start.lineNumber, start.column, end.lineNumber, end.column),
                  text: change.text,
                  forceMoveMarkers: true
                };
              })
              : null;
            if (!edits || !editor.executeEdits("collab.remote", edits)) {
              editor.setValue(model.text);
            }
            editorText = model.text;
          }
          if (selection) {
            restoreEditorSelection(selection, model);
            if (options.selection) {
              // Reapply after Monaco finishes laying out a replaced model. A real
              // mouse/keyboard move cancels this so it cannot pull the caret back.
              window.requestAnimationFrame(function () {
                if (disposed || state.composing || restoreRevision !== state.selectionRestoreRevision) return;
                var latestModel = currentVisibleModel();
                state.rendering = true;
                try {
                  restoreEditorSelection(selection, latestModel);
                  rememberCurrentSelection(latestModel);
                } finally {
                  state.rendering = false;
                }
              });
            }
          }
          state.rendering = false;
        }
        renderRemotePresence(model);
      }
      function sendSessionDataSoon() {
        if (state.presenceTimer) window.clearTimeout(state.presenceTimer);
        state.presenceTimer = window.setTimeout(sendSessionData, 80);
      }
      function scheduleLocalPresenceRender() {
        if (state.localPresenceFrame) return;
        state.localPresenceFrame = window.requestAnimationFrame(function () {
          state.localPresenceFrame = 0;
          if (!disposed) renderRemotePresence(currentVisibleModel(), true);
        });
      }
      function sendSessionData() {
        if (!state.everWelcomed) return;
        var model = currentVisibleModel();
        var currentSelection = captureSelection(model);
        if (!currentSelection) return;
        var start = positionFromAnchor(currentSelection.anchor, model);
        var end = positionFromAnchor(currentSelection.head, model);
        var sessionData = Object.assign(Object.create(null), state.applicationData, {
          cursor: currentSelection.head,
          selection: start === end ? null : currentSelection
        });
        send({
          type: "session_data",
          data: { data: sessionData }
        });
      }
      function notifySessionData(event) {
        sessionDataListeners.forEach(function (listener) {
          try { listener(event); } catch (error) { console.error("session data listener failed", error); }
        });
      }
      function setSessionData(value) {
        state.applicationData = copySessionData(value);
        sendSessionDataSoon();
      }
      function onSessionDataChange(listener) {
        if (typeof listener !== "function") throw new TypeError("listener must be a function");
        sessionDataListeners.add(listener);
        state.peers.forEach(function (peer, peerActor) {
          try {
            listener({ type: "update", actor: peerActor, identity: peer.identity, data: peer.data });
          } catch (error) {
            console.error("session data listener failed", error);
          }
        });
        return function unsubscribe() { sessionDataListeners.delete(listener); };
      }
      function getGutterPresence() {
        var model = currentVisibleModel();
        var lines = new Map();
        function addCursor(identity, presence, actorId, activityOrder) {
          if (!presence || !presence.cursor) return;
          var position = positionAtScalarOffset(model, positionFromAnchor(presence.cursor, model));
          var line = lines.get(position.lineNumber);
          if (!line) {
            line = [];
            lines.set(position.lineNumber, line);
          }
          line.push({
            id: identity.id || String(actorId),
            name: identity.name || identity.id || "参加者",
            avatarUrl: typeof presence.avatarUrl === "string" ? presence.avatarUrl : "",
            activityOrder: activityOrder,
            actor: String(actorId)
          });
        }
        state.peers.forEach(function (peer, peerActor) {
          addCursor(peer.identity || { id: String(peerActor) }, peer.data, peerActor, peer.activityOrder);
        });
        var localSelection = captureSelection(model);
        if (localSelection) {
          addCursor(
            editorIdentity,
            { cursor: localSelection.head, avatarUrl: state.applicationData.avatarUrl },
            state.actor,
            state.localActivityOrder
          );
        }
        return Array.from(lines, function (entry) {
          return {
            lineNumber: entry[0],
            editors: entry[1].sort(function (left, right) {
              return right.activityOrder - left.activityOrder || left.actor.localeCompare(right.actor);
            })
          };
        });
      }
      function positionAtScalarOffset(model, scalarPosition) {
        var editorModel = editor.getModel();
        return editorModel.getPositionAt(utf16Offset(model, scalarPosition));
      }
      function renderRemotePresence(model, localOnly) {
        var decorations = [];
        var activeCursorActors = new Set();
        state.peers.forEach(function (peer, peerActor) {
          var presence = peer.data || {};
          var identity = peer.identity || { id: String(peerActor), name: "参加者 " + peerActor };
          var displayName = identity.name || identity.id;
          var index = collaboratorColorIndex(identity.id || String(peerActor));
          if (!localOnly && presence.selection) {
            var selectionStart = positionFromAnchor(presence.selection.anchor, model);
            var selectionEnd = positionFromAnchor(presence.selection.head, model);
            if (selectionStart !== selectionEnd) {
              var startPosition = positionAtScalarOffset(model, Math.min(selectionStart, selectionEnd));
              var endPosition = positionAtScalarOffset(model, Math.max(selectionStart, selectionEnd));
              decorations.push({
                range: new monaco.Range(
                  startPosition.lineNumber,
                  startPosition.column,
                  endPosition.lineNumber,
                  endPosition.column
                ),
                options: { inlineClassName: "collab-selection-" + index }
              });
            }
          }
          if (presence.cursor) {
            var cursorPosition = positionAtScalarOffset(
              model,
              positionFromAnchor(presence.cursor, model)
            );
            if (localOnly) return;
            activeCursorActors.add(peerActor);
            var widgetId = "collab-cursor-" + peerActor;
            var widget = state.cursorWidgets.get(peerActor);
            if (!widget) {
              var node = document.createElement("span");
              widget = {
                position: cursorPosition,
                getId: function () { return widgetId; },
                getDomNode: function () { return node; },
                getPosition: function () {
                  return {
                    position: widget.position,
                    preference: [monaco.editor.ContentWidgetPositionPreference.EXACT]
                  };
                }
              };
              widget.node = node;
              editor.addContentWidget(widget);
              state.cursorWidgets.set(peerActor, widget);
              widget.colorIndex = index;
              widget.lineHeight = editor.getOption(monaco.editor.EditorOption.lineHeight);
              widget.node.className = "collab-cursor collab-cursor--" + index;
              widget.node.style.setProperty("--collab-cursor-height", widget.lineHeight + "px");
            } else if (widget.position.lineNumber !== cursorPosition.lineNumber ||
                       widget.position.column !== cursorPosition.column) {
              widget.position = cursorPosition;
              editor.layoutContentWidget(widget);
            }
            if (widget.colorIndex !== index) {
              widget.colorIndex = index;
              widget.node.className = "collab-cursor collab-cursor--" + index;
            }
            widget.node.title = displayName || identity.id || "参加者";
            var lineHeight = editor.getOption(monaco.editor.EditorOption.lineHeight);
            if (widget.lineHeight !== lineHeight) {
              widget.lineHeight = lineHeight;
              // Monaco's line height can be overridden per embed, so scope the measured size to each cursor.
              widget.node.style.setProperty("--collab-cursor-height", lineHeight + "px");
            }
          }
        });
        if (!localOnly) {
          state.cursorWidgets.forEach(function (widget, actor) {
            if (!activeCursorActors.has(actor)) {
              editor.removeContentWidget(widget);
              state.cursorWidgets.delete(actor);
            }
          });
          state.editorDecorations = editor.deltaDecorations(state.editorDecorations, decorations);
        }
        if (!localOnly) rememberCurrentSelection(model);
      }
      function showToast(message) {
        toast.textContent = message;
        toast.classList.add("visible");
        window.clearTimeout(toastTimer);
        toastTimer = window.setTimeout(function () { toast.classList.remove("visible"); }, 3800);
      }

      editor.onDidChangeModelContent(function (event) {
        if (!state.rendering) syncFromInput(null, event.changes);
      });
      editor.onDidChangeCursorSelection(function (event) {
        if (state.rendering) return;
        if (event.source === "mouse" || event.source === "keyboard") {
          state.selectionRestoreRevision += 1;
        }
        state.presenceOrder += 1;
        state.localActivityOrder = state.presenceOrder;
        rememberCurrentSelection(currentVisibleModel());
        sendSessionDataSoon();
        scheduleLocalPresenceRender();
      });
      editor.onDidFocusEditorText(sendSessionDataSoon);
      editor.onDidBlurEditorText(sendSessionDataSoon);
      editor.getDomNode().addEventListener("compositionstart", function () {
        state.compositionBase = currentVisibleModel();
        rememberCurrentSelection(state.compositionBase);
        state.composing = true;
      });
      editor.getDomNode().addEventListener("compositionend", function () {
        var baseModel = state.compositionBase;
        state.compositionBase = null;
        state.composing = false;
        syncFromInput(baseModel);
      });
      editor.addAction({
        id: "collab.selectiveUndo",
        label: "自分の編集を元に戻す",
        keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyZ],
        run: function () { undo(false); }
      });
      editor.addAction({
        id: "collab.selectiveRedo",
        label: "自分の編集をやり直す",
        keybindings: [
          monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyZ,
          monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyY
        ],
        run: function () { undo(true); }
      });
      var unloadHandler = function () {
        if (state.socket) state.socket.close();
      };
      window.addEventListener("beforeunload", unloadHandler);
      connect();
      return {
        setSessionData: setSessionData,
        onSessionDataChange: onSessionDataChange,
        getGutterPresence: getGutterPresence,
        dispose: function () {
          disposed = true;
          window.removeEventListener("beforeunload", unloadHandler);
          if (state.reconnectTimer) window.clearTimeout(state.reconnectTimer);
          if (state.presenceTimer) window.clearTimeout(state.presenceTimer);
          if (state.localPresenceFrame) window.cancelAnimationFrame(state.localPresenceFrame);
          if (toastTimer) window.clearTimeout(toastTimer);
          if (state.socket) state.socket.close();
          state.cursorWidgets.forEach(function (widget) { editor.removeContentWidget(widget); });
          sessionDataListeners.clear();
        }
      };
}
