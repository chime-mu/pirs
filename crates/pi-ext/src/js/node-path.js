export const sep = "/";
export const delimiter = ":";
export function isAbsolute(p) { return String(p).startsWith("/"); }
export function normalize(p) {
  p = String(p);
  const abs = p.startsWith("/");
  const parts = [];
  for (const seg of p.split("/")) {
    if (!seg || seg === ".") continue;
    if (seg === "..") { if (parts.length && parts[parts.length - 1] !== "..") parts.pop(); else if (!abs) parts.push(".."); continue; }
    parts.push(seg);
  }
  const out = (abs ? "/" : "") + parts.join("/");
  return out || (abs ? "/" : ".");
}
export function join(...parts) { return normalize(parts.filter((p) => p !== undefined && p !== null && p !== "").join("/")); }
export function resolve(...parts) {
  let out = "";
  for (let i = parts.length - 1; i >= 0; i--) {
    const p = String(parts[i]);
    if (!p) continue;
    out = out ? p + "/" + out : p;
    if (p.startsWith("/")) break;
  }
  if (!out.startsWith("/")) out = globalThis.__pirs.host.sync("process.cwd") + "/" + out;
  return normalize(out);
}
export function dirname(p) { p = String(p); const i = p.lastIndexOf("/"); if (i < 0) return "."; if (i === 0) return "/"; return p.slice(0, i); }
export function basename(p, ext) { p = String(p).replace(/\/+$/, ""); let b = p.slice(p.lastIndexOf("/") + 1); if (ext && b.endsWith(ext)) b = b.slice(0, -ext.length); return b; }
export function extname(p) { const b = basename(p); const i = b.lastIndexOf("."); return i <= 0 ? "" : b.slice(i); }
export function relative(from, to) {
  const f = resolve(from).split("/").filter(Boolean), t = resolve(to).split("/").filter(Boolean);
  let i = 0; while (i < f.length && i < t.length && f[i] === t[i]) i++;
  return [...Array(f.length - i).fill(".."), ...t.slice(i)].join("/");
}
export function parse(p) { const base = basename(p); const ext = extname(p); return { root: isAbsolute(p) ? "/" : "", dir: dirname(p), base, ext, name: ext ? base.slice(0, -ext.length) : base }; }
export function format(o) { return join(o.dir || o.root || "", o.base || (o.name || "") + (o.ext || "")); }
export function toNamespacedPath(p) { return p; }
export const posix = { sep, delimiter, isAbsolute, normalize, join, resolve, dirname, basename, extname, relative, parse, format };
export default posix;
