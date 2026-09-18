// Minimal pi-tui shim: components render to arrays of plain-text lines.
const ANSI_RE = /\x1b\[[0-9;]*m/g;
export function visibleWidth(s) { return String(s).replace(ANSI_RE, "").length; }
export function truncateToWidth(s, width, ellipsis = "…") {
  s = String(s);
  if (visibleWidth(s) <= width) return s;
  const plain = s.replace(ANSI_RE, "");
  return plain.slice(0, Math.max(0, width - ellipsis.length)) + ellipsis;
}
export function wrapTextWithAnsi(text, width) {
  const out = [];
  for (const line of String(text).split("\n")) {
    if (line.length <= width) { out.push(line); continue; }
    let rest = line;
    while (rest.length > width) { out.push(rest.slice(0, width)); rest = rest.slice(width); }
    out.push(rest);
  }
  return out;
}
export function matchesKey(data, key) {
  const map = { escape: "\x1b", enter: "\r", return: "\r", tab: "\t", space: " ", backspace: "\x7f", "ctrl+c": "\x03", "ctrl+d": "\x04", up: "\x1b[A", down: "\x1b[B", left: "\x1b[D", right: "\x1b[C" };
  if (key in map) return data === map[key];
  const m = /^ctrl\+([a-z])$/.exec(key);
  if (m) return data === String.fromCharCode(m[1].charCodeAt(0) - 96);
  return data === key;
}
export class Text {
  constructor(text = "", paddingX = 0, paddingY = 0) { this.text = String(text); this.paddingX = paddingX; this.paddingY = paddingY; }
  setText(t) { this.text = String(t); }
  render(width) {
    const pad = " ".repeat(this.paddingX);
    const lines = wrapTextWithAnsi(this.text, Math.max(1, width - this.paddingX * 2)).map((l) => pad + l + pad);
    const v = Array(this.paddingY).fill("");
    return [...v, ...lines, ...v];
  }
  invalidate() {}
}
export class Spacer { constructor(n = 1) { this.n = n; } render() { return Array(this.n).fill(""); } invalidate() {} }
export class Container {
  constructor() { this.children = []; }
  addChild(c) { this.children.push(c); return c; }
  removeChild(c) { this.children = this.children.filter((x) => x !== c); }
  clear() { this.children = []; }
  render(width) { return this.children.flatMap((c) => c.render(width)); }
  invalidate() { for (const c of this.children) c.invalidate?.(); }
}
export class Box extends Container {
  constructor(paddingX = 0, paddingY = 0) { super(); this.paddingX = paddingX; this.paddingY = paddingY; }
  render(width) {
    const pad = " ".repeat(this.paddingX);
    const inner = super.render(Math.max(1, width - this.paddingX * 2)).map((l) => pad + l + pad);
    const v = Array(this.paddingY).fill("");
    return [...v, ...inner, ...v];
  }
}
export class Markdown extends Text {}
export class TruncatedText extends Text {}
export class Input { constructor() { this.value = ""; } getValue() { return this.value; } setValue(v) { this.value = v; } render() { return [this.value]; } invalidate() {} handleInput() {} }
export class Editor extends Input { getText() { return this.value; } setText(v) { this.value = v; } }
export class SelectList { constructor(items) { this.items = items || []; } render(width) { return this.items.map((i) => truncateToWidth(String(i.label ?? i.value ?? i), width)); } invalidate() {} handleInput() {} }
export class SettingsList { constructor(items) { this.items = items || []; } render(width) { return this.items.map((i) => truncateToWidth(`${i.label}: ${i.currentValue}`, width)); } invalidate() {} handleInput() {} }
export class Loader extends Text {}
export class Spinner extends Text {}
export class Overlay extends Container {}
export class TUI { requestRender() {} setFocus() {} }
export const CURSOR_MARKER = "";
export function isKeyRelease() { return false; }
export function fuzzyFilter(items, query, key) {
  const q = String(query || "").toLowerCase();
  const text = (i) => String(typeof key === "function" ? key(i) : key ? i[key] : (i.label ?? i.value ?? i)).toLowerCase();
  if (!q) return [...items];
  const scored = [];
  for (const it of items) {
    const t = text(it);
    let qi = 0, score = 0;
    for (let i = 0; i < t.length && qi < q.length; i++) if (t[i] === q[qi]) { qi++; score += t[i - 1] === q[qi - 2] ? 2 : 1; }
    if (qi === q.length) scored.push({ it, score: score + (t.startsWith(q) ? 100 : 0) });
  }
  return scored.sort((a, b) => b.score - a.score).map((s) => s.it);
}
const KEY_NAMES = ["escape", "enter", "return", "tab", "space", "backspace", "delete", "up", "down", "left", "right", "home", "end", "pageUp", "pageDown", "insert", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12"];
export const Key = new Proxy({}, {
  get: (_t, prop) => {
    if (typeof prop !== "string") return undefined;
    if (KEY_NAMES.includes(prop)) return prop;
    // Modifier helpers: Key.ctrl("p"), Key.ctrlShift("u"), Key.ctrlAlt("p"), Key.shiftAlt(...)
    const mods = prop.match(/^(ctrl|alt|shift|meta)((?:Ctrl|Alt|Shift|Meta)*)$/);
    if (mods) {
      const all = [mods[1], ...(mods[2].match(/Ctrl|Alt|Shift|Meta/g) || []).map((m) => m.toLowerCase())];
      return (k) => `${all.join("+")}+${k}`;
    }
    return prop;
  },
});
export function parseKey(s) { return String(s); }
export function keyToString(k) { return String(k); }
export function getEditorKeybindings() { return {}; }
export class Image extends Text {}
export class Slider extends Text {}
export default { Text, Container, Box, Spacer, matchesKey, truncateToWidth, visibleWidth, wrapTextWithAnsi };
