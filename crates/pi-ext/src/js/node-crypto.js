export function randomUUID() { return globalThis.__pirs.host.sync("crypto.randomUUID"); }
export function randomBytes(n) { return { toString: (enc) => globalThis.__pirs.host.sync("crypto.randomBytes", n, enc || "hex") }; }
export function createHash(alg) { let data = ""; return { update(d) { data += String(d); return this; }, digest(enc) { return globalThis.__pirs.host.sync("crypto.hash", alg, data, enc || "hex"); } }; }
export default { randomUUID, randomBytes, createHash };
