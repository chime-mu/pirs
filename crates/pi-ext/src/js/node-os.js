const h = () => globalThis.__pirs.host;
export function homedir() { return h().sync("os.homedir"); }
export function tmpdir() { return h().sync("os.tmpdir"); }
export function platform() { return h().sync("os.platform"); }
export function hostname() { return h().sync("os.hostname"); }
export function type() { return platform() === "darwin" ? "Darwin" : platform() === "win32" ? "Windows_NT" : "Linux"; }
export function release() { return ""; }
export function arch() { return h().sync("os.arch"); }
export function cpus() { return []; }
export function totalmem() { return 0; }
export function freemem() { return 0; }
export const EOL = "\n";
export default { homedir, tmpdir, platform, hostname, type, release, arch, cpus, totalmem, freemem, EOL };
