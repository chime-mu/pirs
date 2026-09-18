const h = () => globalThis.__pirs.host;
function stats(s) {
  return { ...s, isFile: () => s.isFile, isDirectory: () => s.isDirectory, isSymbolicLink: () => !!s.isSymbolicLink, mtime: new Date(s.mtimeMs), size: s.size };
}
export function existsSync(p) { return h().sync("fs.exists", String(p)); }
export function readFileSync(p, opts) {
  const enc = typeof opts === "string" ? opts : opts?.encoding;
  const r = h().sync("fs.readFile", String(p), enc ? "utf8" : "base64");
  if (enc) return r;
  return { toString: (e) => (e && e !== "utf8" && e !== "utf-8") ? globalThis.__pirs.base64ToLatin1(r) : globalThis.__pirs.base64ToUtf8(r), base64: r, length: r.length, __pirsBase64: true };
}
export function writeFileSync(p, data, opts) { h().sync("fs.writeFile", String(p), typeof data === "string" ? data : String(data), opts?.flag === "a"); }
export function appendFileSync(p, data) { h().sync("fs.writeFile", String(p), String(data), true); }
export function mkdirSync(p, opts) { h().sync("fs.mkdir", String(p), !!opts?.recursive); }
export function readdirSync(p, opts) { const entries = h().sync("fs.readdir", String(p)); return opts?.withFileTypes ? entries.map((e) => ({ name: e.name, isFile: () => e.isFile, isDirectory: () => e.isDirectory })) : entries.map((e) => e.name); }
export function statSync(p, opts) { const s = h().sync("fs.stat", String(p)); if (!s) { if (opts?.throwIfNoEntry === false) return undefined; throw new Error(`ENOENT: no such file or directory, stat '${p}'`); } return stats(s); }
export const lstatSync = statSync;
export function unlinkSync(p) { h().sync("fs.remove", String(p), false); }
export function rmSync(p, opts) { h().sync("fs.remove", String(p), !!opts?.recursive); }
export function rmdirSync(p, opts) { h().sync("fs.remove", String(p), !!opts?.recursive); }
export function renameSync(a, b) { h().sync("fs.rename", String(a), String(b)); }
export function copyFileSync(a, b) { h().sync("fs.copy", String(a), String(b)); }
export function realpathSync(p) { return h().sync("fs.realpath", String(p)); }
export function accessSync(p) { if (!existsSync(p)) throw new Error(`ENOENT: no such file or directory, access '${p}'`); }
export function chmodSync() {}
export function mkdtempSync(prefix) { return h().sync("fs.mkdtemp", String(prefix)); }
export function watch() { return { close() {} }; }
export const constants = { F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1 };
export const promises = {
  readFile: async (p, o) => readFileSync(p, o), writeFile: async (p, d, o) => writeFileSync(p, d, o), appendFile: async (p, d) => appendFileSync(p, d),
  mkdir: async (p, o) => mkdirSync(p, o), readdir: async (p, o) => readdirSync(p, o), stat: async (p) => statSync(p), lstat: async (p) => statSync(p),
  unlink: async (p) => unlinkSync(p), rm: async (p, o) => rmSync(p, o), rename: async (a, b) => renameSync(a, b), copyFile: async (a, b) => copyFileSync(a, b),
  access: async (p) => accessSync(p), realpath: async (p) => realpathSync(p), mkdtemp: async (p) => mkdtempSync(p),
};
export function readFile(p, opts, cb) { if (typeof opts === "function") { cb = opts; opts = undefined; } try { cb(null, readFileSync(p, opts)); } catch (e) { cb(e); } }
export function writeFile(p, d, opts, cb) { if (typeof opts === "function") { cb = opts; opts = undefined; } try { writeFileSync(p, d, opts); cb?.(null); } catch (e) { cb?.(e); } }
export default { existsSync, mkdtempSync, readFileSync, writeFileSync, appendFileSync, mkdirSync, readdirSync, statSync, lstatSync, unlinkSync, rmSync, rmdirSync, renameSync, copyFileSync, realpathSync, accessSync, chmodSync, watch, constants, promises, readFile, writeFile };
