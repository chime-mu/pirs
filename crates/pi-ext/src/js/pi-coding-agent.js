export function defineTool(t) { return t; }
export function isToolCallEventType(name, event) { return event.toolName === name; }
export function isBashToolResult(e) { return e.toolName === "bash"; }
export function isReadToolResult(e) { return e.toolName === "read"; }
export function isEditToolResult(e) { return e.toolName === "edit"; }
export function isWriteToolResult(e) { return e.toolName === "write"; }
export function isGrepToolResult(e) { return e.toolName === "grep"; }
export function isFindToolResult(e) { return e.toolName === "find"; }
export function isLsToolResult(e) { return e.toolName === "ls"; }
export const CONFIG_DIR_NAME = ".pi";
export function getAgentDir() { return globalThis.__pirs.host.sync("paths.agentDir"); }
export function getSessionsDir() { return globalThis.__pirs.host.sync("paths.sessionsDir"); }
export function keyHint(_id, description) { return description; }
export function keyText(id) { return id; }
export function rawKeyHint(key, description) { return `${key} ${description}`; }
export function getSettingsListTheme() { return {}; }
export function createLocalBashOperations() {
  return { exec: (command, cwd, options) => globalThis.__pirs.host.async("bash.exec", command, cwd, options || {}) };
}
export function execCommand(command, args, cwd, options) { return globalThis.__pirs.host.async("exec", command, args || [], { ...(options || {}), cwd }); }
export function truncateHead(text, opts) { return globalThis.__pirs.host.sync("truncate.head", text, opts || {}); }
export function truncateTail(text, opts) { return globalThis.__pirs.host.sync("truncate.tail", text, opts || {}); }
export function formatSize(bytes) { return bytes < 1024 ? `${bytes}B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)}KB` : `${(bytes / 1024 / 1024).toFixed(1)}MB`; }
export const DEFAULT_MAX_LINES = 2000;
export const DEFAULT_MAX_BYTES = 50 * 1024;
export class SessionManager {
  static list(cwd) { return globalThis.__pirs.host.async("sessions.list", cwd); }
  static listAll() { return globalThis.__pirs.host.async("sessions.list", null); }
}
export function buildSystemPrompt(opts) { return globalThis.__pirs.host.sync("systemPrompt.build", opts || {}); }
export function loadSettings() { return globalThis.__pirs.host.sync("settings"); }
export function normalizeToolArgs(a) { return a; }
export const VERSION = "0.1.0-pirs";
export function serializeConversation(messages, options) {
  const out = [];
  for (const m of messages || []) {
    const text = (c) => (typeof c === "string" ? c : (c || []).map((b) => (b.type === "text" ? b.text : b.type === "toolCall" ? `[tool call ${b.name}(${JSON.stringify(b.arguments)})]` : b.type === "thinking" ? "" : `[${b.type}]`)).join(""));
    if (m.role === "user") out.push(`User: ${text(m.content)}`);
    else if (m.role === "assistant") out.push(`Assistant: ${text(m.content)}`);
    else if (m.role === "toolResult") out.push(`Tool result (${m.toolName}): ${text(m.content)}`);
    else if (m.role === "custom") out.push(`${m.customType}: ${text(m.content)}`);
  }
  return out.join("\n\n");
}
export function withFileMutationQueue(_path, fn) { return fn(); }
export function parseFrontmatter(content) {
  const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/.exec(String(content));
  if (!m) return { frontmatter: {}, body: String(content) };
  const frontmatter = {};
  for (const line of m[1].split(/\r?\n/)) {
    const kv = /^([A-Za-z0-9_-]+):\s*(.*)$/.exec(line);
    if (!kv) continue;
    let v = kv[2].trim();
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) v = v.slice(1, -1);
    else if (v === "true") v = true; else if (v === "false") v = false; else if (v !== "" && !isNaN(Number(v))) v = Number(v);
    else if (v.startsWith("[") && v.endsWith("]")) v = v.slice(1, -1).split(",").map((x) => x.trim().replace(/^["']|["']$/g, "")).filter(Boolean);
    frontmatter[kv[1]] = v;
  }
  return { frontmatter, body: m[2] };
}
// Built-in tool factories. The returned tools run the host's native implementations;
// `options.operations` / spawn hooks are only honoured for bash (`operations.exec`).
function builtinTool(name, cwd, options) {
  const info = globalThis.__pirs.host.sync("tools.info", name) || { name, description: "", parameters: { type: "object", properties: {} } };
  return {
    ...info,
    label: info.label || name,
    async execute(toolCallId, params, signal, onUpdate, ctx) {
      if (name === "bash" && options?.operations?.exec) {
        const r = await options.operations.exec(params.command, options.cwd || cwd, { timeout: params.timeout ? params.timeout * 1000 : undefined, signal, onData: (d) => onUpdate?.({ content: [{ type: "text", text: String(d) }] }) });
        const out = r?.output ?? "";
        if (r?.exitCode && r.exitCode !== 0) throw new Error(`${out}\n\nCommand exited with code ${r.exitCode}`);
        return { content: [{ type: "text", text: out }], details: { cancelled: !!r?.cancelled, truncated: !!r?.truncated } };
      }
      const execId = "tool_" + Math.random().toString(36).slice(2);
      signal?.addEventListener("abort", () => globalThis.__pirs.host.sync("exec.kill", execId), { once: true });
      const r = await globalThis.__pirs.host.async("tools.execute", execId, name, toolCallId, params, cwd);
      if (r?.error) throw new Error(r.error);
      return r;
    },
  };
}
export function createBashTool(cwd, options) { return builtinTool("bash", cwd, options); }
export function createReadTool(cwd, options) { return builtinTool("read", cwd, options); }
export function createEditTool(cwd, options) { return builtinTool("edit", cwd, options); }
export function createWriteTool(cwd, options) { return builtinTool("write", cwd, options); }
export function createGrepTool(cwd, options) { return builtinTool("grep", cwd, options); }
export function createFindTool(cwd, options) { return builtinTool("find", cwd, options); }
export function createLsTool(cwd, options) { return builtinTool("ls", cwd, options); }
export function createBashToolDefinition(cwd, options) { return createBashTool(cwd, options); }
export function createReadToolDefinition(cwd, options) { return createReadTool(cwd, options); }
export function createEditToolDefinition(cwd, options) { return createEditTool(cwd, options); }
export function createWriteToolDefinition(cwd, options) { return createWriteTool(cwd, options); }
export function wrapToolDefinition(t) { return t; }
export function createCodingTools(cwd, options) { return [createReadTool(cwd, options), createBashTool(cwd, options), createEditTool(cwd, options), createWriteTool(cwd, options)]; }
export function createAllTools(cwd, options) { return [...createCodingTools(cwd, options), createGrepTool(cwd, options), createFindTool(cwd, options), createLsTool(cwd, options)]; }
export const codingTools = [];
export const readOnlyTools = [];
// TUI-only classes: present so modules load; rendering is not supported in pirs.
class UnsupportedComponent { constructor() { this.children = []; } render() { return []; } invalidate() {} addChild(c) { this.children.push(c); } setText() {} handleInput() {} dispose() {} }
export class CustomEditor extends UnsupportedComponent { getText() { return ""; } setText() {} }
export class DynamicBorder extends UnsupportedComponent {}
export class BorderedLoader extends UnsupportedComponent { start() {} stop() {} }
export class ThinkingBlock extends UnsupportedComponent {}
export function getMarkdownTheme() { return {}; }
export function getSelectListTheme() { return {}; }
export function getEditorTheme() { return {}; }
export function getInputTheme() { return {}; }
export function convertToLlm(messages) { return messages; }
export function estimateTokens(text) { return Math.ceil((typeof text === "string" ? text : JSON.stringify(text ?? "")).length / 4); }
export function isImageFile(p) { return /\.(png|jpe?g|gif|webp|bmp)$/i.test(String(p)); }
export default {};
