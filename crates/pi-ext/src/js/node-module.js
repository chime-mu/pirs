export function createRequire() { return (id) => { throw new Error(`require("${id}") is not supported in pirs extensions; use ESM imports`); }; }
export const builtinModules = ["fs", "path", "os", "child_process", "util", "url", "crypto", "process", "events", "readline", "module"];
export default { createRequire, builtinModules };
