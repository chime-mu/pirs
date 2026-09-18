const h = () => globalThis.__pirs.host;
export function execSync(command, opts) {
  const r = h().sync("bash.execSync", String(command), opts?.cwd, opts?.timeout);
  if (r.code !== 0) { const e = new Error(`Command failed: ${command}\n${r.stderr}`); e.status = r.code; e.stdout = r.stdout; e.stderr = r.stderr; throw e; }
  return opts?.encoding ? r.stdout : r.stdout;
}
export function execFileSync(file, args, opts) { return execSync([file, ...(args || [])].map((a) => `'${String(a).replace(/'/g, "'\\''")}'`).join(" "), opts); }
export function spawnSync(file, args, opts) { const r = h().sync("bash.execSync", [file, ...(args || [])].map((a) => `'${String(a).replace(/'/g, "'\\''")}'`).join(" "), opts?.cwd, opts?.timeout); return { status: r.code, stdout: r.stdout, stderr: r.stderr, pid: 0 }; }
export function exec(command, opts, cb) {
  if (typeof opts === "function") { cb = opts; opts = undefined; }
  h().async("bash.exec", String(command), opts?.cwd, { timeout: opts?.timeout }).then((r) => cb?.(r.exitCode === 0 ? null : Object.assign(new Error(r.output), { code: r.exitCode }), r.output, "")).catch((e) => cb?.(e, "", ""));
}
export function spawn() { throw new Error("child_process.spawn is not supported in pirs extensions; use pi.exec()"); }
export default { execSync, execFileSync, spawnSync, exec, spawn };
