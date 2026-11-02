const sessionIdPattern = /^[A-Za-z0-9_-]{1,64}$/;

export function isValidSessionId(value) {
  return typeof value === "string" && sessionIdPattern.test(value);
}

export function isValidEditorIdentity(id, name) {
  return [[id, 64], [name, 80]].every(([value, max]) =>
    typeof value === "string" && value.trim() !== "" && Array.from(value).length <= max &&
    !/[\u0000-\u001f\u007f-\u009f]/.test(value)
  );
}

export function randomEditorId() {
  if (crypto.randomUUID) return `editor-${crypto.randomUUID()}`;
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return `editor-${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}
