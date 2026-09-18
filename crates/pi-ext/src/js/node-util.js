export function promisify(fn) { return (...args) => new Promise((res, rej) => fn(...args, (err, ...out) => (err ? rej(err) : res(out.length > 1 ? out : out[0])))); }
export function inspect(v) { try { return typeof v === "string" ? v : JSON.stringify(v, null, 2); } catch { return String(v); } }
export function format(f, ...args) { let i = 0; return String(f).replace(/%[sdjoO]/g, (m) => (i < args.length ? (m === "%j" ? JSON.stringify(args[i++]) : String(args[i++])) : m)) + (i < args.length ? " " + args.slice(i).map(inspect).join(" ") : ""); }
export function isDeepStrictEqual(a, b) { return JSON.stringify(a) === JSON.stringify(b); }
export const types = { isPromise: (v) => v && typeof v.then === "function" };
export class TextEncoder { encode(s) { return globalThis.__pirs.utf8Encode(String(s)); } }
export class TextDecoder { decode(b) { return globalThis.__pirs.utf8Decode(b); } }
export default { promisify, inspect, format, isDeepStrictEqual, types, TextEncoder, TextDecoder };
