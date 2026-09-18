export function fileURLToPath(u) { u = String(u); return decodeURIComponent(u.startsWith("file://") ? u.slice(7) : u); }
export function pathToFileURL(p) { return { href: "file://" + p, toString: () => "file://" + p }; }
export const URL = globalThis.URL;
export const URLSearchParams = globalThis.URLSearchParams;
export default { fileURLToPath, pathToFileURL, URL, URLSearchParams };
