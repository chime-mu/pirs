// pirs extension runtime. Evaluated once per host as a script. Defines the
// `pi` ExtensionAPI, event dispatch with pi's chaining semantics, the per-event
// ExtensionContext, and Node-like globals. Everything that needs the outside
// world goes through two native functions: __hostSync(name, argsJson) and
// __hostAsync(name, argsJson) -> Promise, both returning JSON strings.
(function () {
  const hostSync = (name, ...args) => {
    const r = __hostSync(name, JSON.stringify(args));
    return r === undefined || r === null ? undefined : JSON.parse(r);
  };
  const hostAsync = async (name, ...args) => {
    const r = await __hostAsync(name, JSON.stringify(args));
    return r === undefined || r === null ? undefined : JSON.parse(r);
  };
  const host = { sync: hostSync, async: hostAsync };

  // ------------------------------------------------------------------------
  // Globals: console, timers, AbortController, fetch, process, misc
  // ------------------------------------------------------------------------
  const fmt = (v) => {
    if (typeof v === "string") return v;
    if (v instanceof Error) return v.stack || v.message;
    try { return JSON.stringify(v); } catch { return String(v); }
  };
  globalThis.console = {
    log: (...a) => hostSync("console", "log", a.map(fmt).join(" ")),
    info: (...a) => hostSync("console", "info", a.map(fmt).join(" ")),
    warn: (...a) => hostSync("console", "warn", a.map(fmt).join(" ")),
    error: (...a) => hostSync("console", "error", a.map(fmt).join(" ")),
    debug: (...a) => hostSync("console", "debug", a.map(fmt).join(" ")),
    trace: (...a) => hostSync("console", "error", a.map(fmt).join(" ")),
  };

  let timerSeq = 1;
  const timers = new Map();
  globalThis.setTimeout = (fn, ms = 0, ...args) => {
    const id = timerSeq++;
    timers.set(id, true);
    hostAsync("sleep", Math.max(0, ms | 0)).then(() => {
      if (!timers.has(id)) return;
      timers.delete(id);
      try { fn(...args); } catch (e) { console.error("uncaught in setTimeout:", fmt(e)); }
    });
    return id;
  };
  globalThis.clearTimeout = (id) => { timers.delete(id); };
  globalThis.setInterval = (fn, ms = 0, ...args) => {
    const id = timerSeq++;
    timers.set(id, true);
    const tick = async () => {
      while (timers.has(id)) {
        await hostAsync("sleep", Math.max(1, ms | 0));
        if (!timers.has(id)) break;
        try { fn(...args); } catch (e) { console.error("uncaught in setInterval:", fmt(e)); }
      }
    };
    tick();
    return id;
  };
  globalThis.clearInterval = globalThis.clearTimeout;
  globalThis.setImmediate = (fn, ...args) => globalThis.setTimeout(fn, 0, ...args);
  globalThis.clearImmediate = globalThis.clearTimeout;
  globalThis.queueMicrotask = (fn) => { Promise.resolve().then(fn); };
  globalThis.structuredClone = (v) => (v === undefined ? undefined : JSON.parse(JSON.stringify(v)));

  class AbortSignal {
    constructor() { this.aborted = false; this.reason = undefined; this._listeners = []; this.onabort = null; }
    addEventListener(type, fn, opts) { if (type === "abort") { if (this.aborted && !opts?.once) fn({ type: "abort" }); else this._listeners.push(fn); } }
    removeEventListener(type, fn) { if (type === "abort") this._listeners = this._listeners.filter((f) => f !== fn); }
    throwIfAborted() { if (this.aborted) throw this.reason ?? new Error("The operation was aborted"); }
    static timeout(ms) { const c = new AbortController(); setTimeout(() => c.abort(new Error("TimeoutError")), ms); return c.signal; }
    static any(signals) { const c = new AbortController(); for (const s of signals) { if (s?.aborted) { c.abort(s.reason); break; } s?.addEventListener("abort", () => c.abort(s.reason)); } return c.signal; }
  }
  class AbortController {
    constructor() { this.signal = new AbortSignal(); }
    abort(reason) {
      const s = this.signal;
      if (s.aborted) return;
      s.aborted = true;
      s.reason = reason ?? Object.assign(new Error("This operation was aborted"), { name: "AbortError" });
      const ev = { type: "abort", target: s };
      try { s.onabort?.(ev); } catch {}
      for (const fn of [...s._listeners]) { try { fn(ev); } catch {} }
    }
  }
  globalThis.AbortController = AbortController;
  globalThis.AbortSignal = AbortSignal;

  class Headers {
    constructor(init) { this._m = new Map(); if (init) { const entries = init instanceof Headers ? [...init._m] : Array.isArray(init) ? init : Object.entries(init); for (const [k, v] of entries) this.set(k, v); } }
    get(k) { return this._m.get(String(k).toLowerCase()) ?? null; }
    set(k, v) { this._m.set(String(k).toLowerCase(), String(v)); }
    has(k) { return this._m.has(String(k).toLowerCase()); }
    delete(k) { this._m.delete(String(k).toLowerCase()); }
    forEach(fn) { for (const [k, v] of this._m) fn(v, k, this); }
    entries() { return this._m.entries(); }
    keys() { return this._m.keys(); }
    [Symbol.iterator]() { return this._m.entries(); }
    toJSON() { return Object.fromEntries(this._m); }
  }
  globalThis.Headers = Headers;
  class Response {
    constructor(r) { this.status = r.status; this.ok = r.status >= 200 && r.status < 300; this.statusText = r.statusText || ""; this.headers = new Headers(r.headers); this._body = r.body; this.url = r.url; this.bodyUsed = false; }
    async text() { this.bodyUsed = true; return this._body; }
    async json() { this.bodyUsed = true; return JSON.parse(this._body); }
    async arrayBuffer() { this.bodyUsed = true; return __pirs.utf8Encode(this._body).buffer; }
    clone() { return new Response({ status: this.status, statusText: this.statusText, headers: this.headers.toJSON(), body: this._body, url: this.url }); }
  }
  globalThis.Response = Response;
  globalThis.fetch = async (url, init = {}) => {
    const headers = init.headers instanceof Headers ? init.headers.toJSON() : init.headers || {};
    let body = init.body;
    if (body !== undefined && typeof body !== "string") body = typeof body === "object" && body !== null && !(body instanceof ArrayBuffer) ? JSON.stringify(body) : String(body);
    const id = "fetch_" + timerSeq++;
    init.signal?.addEventListener("abort", () => hostSync("fetch.abort", id), { once: true });
    const r = await hostAsync("fetch", id, String(url), init.method || "GET", headers, body ?? null);
    if (r.error) { const e = new Error(r.error); e.name = init.signal?.aborted ? "AbortError" : "TypeError"; throw e; }
    return new Response({ ...r, url: String(url) });
  };

  // Live view of the process environment (reads go through the host).
  const envProxy = new Proxy({}, {
    get: (_t, key) => (typeof key === "string" ? hostSync("process.env.get", key) ?? undefined : undefined),
    has: (_t, key) => typeof key === "string" && hostSync("process.env.get", key) !== undefined,
    set: (_t, key, value) => { hostSync("process.env.set", key, String(value)); return true; },
    deleteProperty: (_t, key) => { hostSync("process.env.set", key, null); return true; },
    ownKeys: () => Object.keys(hostSync("process.env") || {}),
    getOwnPropertyDescriptor: (_t, key) => { const v = hostSync("process.env.get", key); return v === undefined ? undefined : { value: v, enumerable: true, configurable: true, writable: true }; },
  });
  globalThis.process = {
    env: envProxy,
    argv: ["pirs"],
    platform: hostSync("os.platform"),
    arch: hostSync("os.arch"),
    version: "v22.0.0-pirs",
    versions: { node: "22.0.0", pirs: "0.1.0" },
    pid: 0,
    cwd: () => hostSync("process.cwd"),
    exit: (code) => { throw new Error(`process.exit(${code ?? 0}) is not allowed inside pirs extensions`); },
    on: () => globalThis.process,
    once: () => globalThis.process,
    off: () => globalThis.process,
    removeListener: () => globalThis.process,
    nextTick: (fn, ...a) => queueMicrotask(() => fn(...a)),
    hrtime: Object.assign((prev) => { const ms = hostSync("time.now"); const s = Math.floor(ms / 1000), ns = Math.floor((ms % 1000) * 1e6); return prev ? [s - prev[0], ns - prev[1]] : [s, ns]; }, { bigint: () => BigInt(Math.floor(hostSync("time.now") * 1e6)) }),
    memoryUsage: () => ({ rss: 0, heapUsed: 0, heapTotal: 0 }),
    stdout: { write: (s) => { hostSync("console", "log", String(s).replace(/\n$/, "")); return true; }, isTTY: false, columns: 80 },
    stderr: { write: (s) => { hostSync("console", "error", String(s).replace(/\n$/, "")); return true; }, isTTY: false },
    stdin: { isTTY: false, on() {}, setRawMode() {} },
    getBuiltinModule: () => undefined,
    features: {},
  };
  globalThis.performance = { now: () => hostSync("time.now") };
  globalThis.crypto = globalThis.crypto || {};
  globalThis.crypto.randomUUID = () => hostSync("crypto.randomUUID");
  globalThis.crypto.getRandomValues = (arr) => { for (let i = 0; i < arr.length; i++) arr[i] = Math.floor(Math.random() * 256); return arr; };
  globalThis.TextEncoder = class { encode(s) { return __pirs.utf8Encode(String(s)); } };
  globalThis.TextDecoder = class { decode(b) { return __pirs.utf8Decode(b); } };
  globalThis.btoa = (s) => hostSync("base64.encode", String(s));
  globalThis.atob = (s) => hostSync("base64.decode", String(s));

  // ------------------------------------------------------------------------
  // Registry
  // ------------------------------------------------------------------------
  const registry = { extensions: [], flagValues: {}, state: "loading" };

  class ExtensionRecord {
    constructor(path) {
      this.path = path;
      this.handlers = new Map(); // event -> handler[]
      this.tools = new Map();
      this.commands = new Map();
      this.shortcuts = new Map();
      this.flags = new Map();
      this.messageRenderers = new Map();
      this.entryRenderers = new Map();
      this.markdownTransformer = undefined;
      this.active = true;
    }
  }

  const eventBus = (() => {
    const listeners = new Map();
    return {
      on(name, fn) { (listeners.get(name) ?? listeners.set(name, []).get(name)).push(fn); return () => this.off(name, fn); },
      off(name, fn) { listeners.set(name, (listeners.get(name) || []).filter((f) => f !== fn)); },
      emit(name, data) { for (const fn of [...(listeners.get(name) || [])]) { try { fn(data); } catch (e) { console.error("event bus handler failed:", fmt(e)); } } },
      once(name, fn) { const w = (d) => { this.off(name, w); fn(d); }; return this.on(name, w); },
    };
  })();

  function stripFunctions(v) {
    return v === undefined ? undefined : JSON.parse(JSON.stringify(v, (_k, val) => (typeof val === "function" ? undefined : val)));
  }

  function createApi(ext) {
    const assertActive = () => { if (!ext.active) throw new Error(`Extension ${ext.path} is no longer active (session was replaced or reloaded)`); };
    const api = {
      on(event, handler) {
        assertActive();
        if (typeof handler !== "function") throw new Error(`pi.on("${event}") requires a function handler`);
        const list = ext.handlers.get(event) ?? ext.handlers.set(event, []).get(event);
        list.push(handler);
        return () => { const l = ext.handlers.get(event); if (l) ext.handlers.set(event, l.filter((h) => h !== handler)); };
      },
      registerTool(tool) {
        assertActive();
        if (!tool || typeof tool.name !== "string") throw new Error("registerTool requires a tool with a name");
        if (typeof tool.execute !== "function") throw new Error(`Tool "${tool.name}" must define execute()`);
        if (!tool.parameters || tool.parameters.type !== "object") throw new Error(`Tool "${tool.name}" registered by extension "${ext.path}" must define an object parameter schema.`);
        ext.tools.set(tool.name, tool);
        if (registry.state === "loaded") hostSync("tools.changed");
      },
      registerCommand(name, options) {
        assertActive();
        if (typeof options?.handler !== "function") throw new Error(`Command "${name}" must define a handler`);
        ext.commands.set(name, { name, ...options });
      },
      registerShortcut(shortcut, options) { assertActive(); ext.shortcuts.set(shortcut, { shortcut, ...options }); },
      registerFlag(name, options) {
        assertActive();
        if (options.default !== undefined && typeof options.default !== options.type) throw new Error(`Invalid default for flag "${name}": expected ${options.type}, got ${typeof options.default}`);
        ext.flags.set(name, { name, ...options });
        if (options.default !== undefined && !(name in registry.flagValues)) registry.flagValues[name] = options.default;
      },
      getFlag(name) { assertActive(); if (!ext.flags.has(name)) return undefined; return registry.flagValues[name]; },
      registerMessageRenderer(customType, renderer) { assertActive(); ext.messageRenderers.set(customType, renderer); },
      registerMarkdownTransformer(t) { assertActive(); ext.markdownTransformer = t; },
      registerEntryRenderer(customType, renderer) { assertActive(); ext.entryRenderers.set(customType, renderer); },
      sendMessage(message, options) { assertActive(); hostSync("sendMessage", stripFunctions(message), options || {}); },
      sendUserMessage(content, options) { assertActive(); hostSync("sendUserMessage", content, options || {}); },
      appendEntry(customType, data) { assertActive(); hostSync("appendEntry", customType, data === undefined ? null : stripFunctions(data)); },
      setSessionName(name) { assertActive(); hostSync("setSessionName", name); },
      getSessionName() { assertActive(); return hostSync("getSessionName") ?? undefined; },
      setLabel(entryId, label) { assertActive(); hostSync("setLabel", entryId, label ?? null); },
      exec(command, args, options) {
        assertActive();
        const id = "exec_" + timerSeq++;
        options?.signal?.addEventListener("abort", () => hostSync("exec.kill", id), { once: true });
        return hostAsync("exec", id, command, args || [], { timeout: options?.timeout, cwd: options?.cwd });
      },
      getActiveTools() { assertActive(); return hostSync("getActiveTools"); },
      getAllTools() { assertActive(); return hostSync("getAllTools"); },
      setActiveTools(names) { assertActive(); hostSync("setActiveTools", names); },
      getCommands() { assertActive(); return hostSync("getCommands"); },
      setModel(model) { assertActive(); return hostAsync("setModel", stripFunctions(model)); },
      getThinkingLevel() { assertActive(); return hostSync("getThinkingLevel"); },
      setThinkingLevel(level) { assertActive(); hostSync("setThinkingLevel", level); },
      registerProvider(nameOrProvider, config) {
        assertActive();
        if (typeof nameOrProvider === "string") {
          if (typeof config?.streamSimple === "function") console.warn(`registerProvider("${nameOrProvider}"): custom streamSimple functions are not supported in pirs; models will use the "api" field instead`);
          hostSync("registerProvider", nameOrProvider, stripFunctions(config));
        } else {
          hostSync("registerProvider", nameOrProvider?.id ?? nameOrProvider?.name ?? "custom", stripFunctions(nameOrProvider));
        }
      },
      unregisterProvider(name) { assertActive(); hostSync("unregisterProvider", name); },
      events: eventBus,
    };
    return api;
  }

  // ------------------------------------------------------------------------
  // Contexts
  // ------------------------------------------------------------------------
  // Plain-text theme: colour functions return their input unchanged.
  const theme = new Proxy({}, { get: (_t, prop) => (prop === "fg" || prop === "bg" ? (_c, text) => String(text) : (text) => String(text)) });
  const cleanOpts = (o) => (o ? { timeout: o.timeout, placeholder: o.placeholder } : undefined);
  const withSignal = (o, signalId) => ({ ...(cleanOpts(o) || {}), signalId });

  function createContext(extra = {}) {
    const info = hostSync("context.info") || {};
    const signal = extra.signal;
    const dialog = async (kind, ...args) => {
      const opts = args[args.length - 1];
      let id;
      if (opts?.signal) { id = "dialog_" + timerSeq++; opts.signal.addEventListener("abort", () => hostSync("ui.dismiss", id), { once: true }); }
      return hostAsync(`ui.${kind}`, ...args.slice(0, -1), withSignal(opts, id));
    };
    const ui = {
      select: async (title, options, opts) => (await dialog("select", title, options, opts)) ?? undefined,
      confirm: async (title, message, opts) => !!(await dialog("confirm", title, message, opts)),
      input: async (title, placeholder, opts) => (await dialog("input", title, placeholder ?? "", opts)) ?? undefined,
      editor: async (title, prefill) => (await hostAsync("ui.editor", title, prefill ?? "")) ?? undefined,
      notify: (message, type) => hostSync("ui.notify", String(message), type || "info"),
      setStatus: (key, text) => hostSync("ui.setStatus", key, text ?? null),
      setWorkingMessage: (m) => hostSync("ui.setWorkingMessage", m ?? null),
      setWorkingVisible: () => {},
      setWorkingIndicator: () => {},
      setHiddenThinkingLabel: () => {},
      setWidget: (key, content, options) => hostSync("ui.setWidget", key, typeof content === "function" ? null : content ?? null, options?.placement ?? null),
      setFooter: () => {},
      setHeader: () => {},
      setTitle: (t) => hostSync("ui.setTitle", String(t)),
      pasteToEditor: (t) => hostSync("ui.setEditorText", (hostSync("ui.getEditorText") || "") + String(t)),
      setEditorText: (t) => hostSync("ui.setEditorText", String(t)),
      getEditorText: () => hostSync("ui.getEditorText") || "",
      addAutocompleteProvider: () => {},
      setEditorComponent: () => {},
      getEditorComponent: () => undefined,
      getAllThemes: () => [{ name: "dark", path: undefined }],
      getTheme: () => undefined,
      setTheme: () => ({ success: false, error: "themes are not supported in pirs" }),
      getToolsExpanded: () => false,
      setToolsExpanded: () => {},
      custom: async () => { throw new Error("ctx.ui.custom() (custom TUI components) is not supported in pirs"); },
      theme,
      onTerminalInput: () => () => {},
    };
    const sessionManager = {
      getEntries: () => hostSync("session.entries") || [],
      getBranch: () => hostSync("session.branch") || [],
      buildContextEntries: () => hostSync("session.branch") || [],
      getLeafId: () => hostSync("session.leafId") ?? null,
      getSessionFile: () => info.sessionFile ?? undefined,
      getSessionId: () => info.sessionId,
      getCwd: () => info.cwd,
      getSessionName: () => hostSync("getSessionName") ?? undefined,
      getLabel: (id) => hostSync("session.label", id) ?? undefined,
      getEntry: (id) => (hostSync("session.entries") || []).find((e) => e.id === id),
      getChildren: (id) => (hostSync("session.entries") || []).filter((e) => e.parentId === id),
      getHeader: () => ({ id: info.sessionId, cwd: info.cwd }),
    };
    const modelRegistry = {
      getAvailable: () => hostSync("models.available") || [],
      getAll: () => hostSync("models.all") || [],
      find: (spec) => hostSync("models.find", spec) ?? undefined,
      getModel: (provider, id) => hostSync("models.get", provider, id) ?? undefined,
      getProviderAuth: (provider) => hostSync("models.providerAuth", provider),
      getProvider: (provider) => ({ id: provider }),
      streamSimple: (model, context, options) => {
        const events = [];
        const p = hostAsync("models.complete", stripFunctions(model), stripFunctions(context), stripFunctions(options) || {});
        const stream = {
          [Symbol.asyncIterator]: async function* () { const m = await p; yield { type: "done", reason: m.stopReason, message: m }; },
          result: () => p,
        };
        void events;
        return stream;
      },
      complete: (model, context, options) => hostAsync("models.complete", stripFunctions(model), stripFunctions(context), stripFunctions(options) || {}),
    };
    modelRegistry.stream = modelRegistry.streamSimple;
    return {
      ui,
      mode: info.mode,
      hasUI: !!info.hasUI,
      cwd: info.cwd,
      sessionManager,
      modelRegistry,
      model: info.model ?? undefined,
      thinkingLevel: info.thinkingLevel,
      scopedModels: [],
      signal,
      isIdle: () => !!(hostSync("context.info") || {}).isIdle,
      isProjectTrusted: () => true,
      abort: () => hostSync("abort"),
      hasPendingMessages: () => !!hostSync("hasPendingMessages"),
      shutdown: () => hostSync("shutdown"),
      getContextUsage: () => hostSync("contextUsage") ?? undefined,
      compact: (options) => hostSync("compact", stripFunctions(options) || {}),
      getSystemPrompt: () => hostSync("systemPrompt") || "",
    };
  }

  function createCommandContext(extra) {
    const ctx = createContext(extra);
    ctx.getSystemPromptOptions = () => hostSync("systemPromptOptions") || {};
    ctx.waitForIdle = () => hostAsync("waitForIdle");
    ctx.newSession = (options) => hostAsync("newSession", { parentSession: options?.parentSession });
    ctx.fork = (entryId, options) => hostAsync("fork", entryId, { position: options?.position });
    ctx.navigateTree = (targetId, options) => hostAsync("navigateTree", targetId, stripFunctions(options) || {});
    ctx.switchSession = (path) => hostAsync("switchSession", path);
    ctx.reload = () => hostAsync("reload");
    return ctx;
  }

  // ------------------------------------------------------------------------
  // Signals for cancellation from Rust
  // ------------------------------------------------------------------------
  const controllers = new Map();
  function makeSignal(id) {
    if (!id) return undefined;
    const c = new AbortController();
    controllers.set(id, c);
    return c.signal;
  }
  function releaseSignal(id) { if (id) controllers.delete(id); }
  globalThis.__abortSignal = (id) => { controllers.get(id)?.abort(); };

  // ------------------------------------------------------------------------
  // Errors
  // ------------------------------------------------------------------------
  function reportError(ext, event, err) {
    hostSync("reportError", { extensionPath: ext.path, event, error: err instanceof Error ? err.message : String(err), stack: err instanceof Error ? err.stack : undefined });
  }
  const errorText = (e) => (e instanceof Error ? `${e.message}${e.stack ? "\n" + e.stack : ""}` : String(e));

  // ------------------------------------------------------------------------
  // Loading
  // ------------------------------------------------------------------------
  globalThis.__loadExtension = async function (path) {
    const ext = new ExtensionRecord(path);
    const api = createApi(ext);
    let mod;
    try {
      mod = await import(path);
    } catch (e) {
      throw new Error(`Failed to import extension ${path}: ${errorText(e)}`);
    }
    const factory = mod.default;
    if (typeof factory !== "function") throw new Error(`Extension ${path} must export a default function`);
    registry.extensions.push(ext);
    try {
      const r = factory(api);
      if (r && typeof r.then === "function") await r;
    } catch (e) {
      registry.extensions = registry.extensions.filter((x) => x !== ext);
      throw new Error(`Extension ${path} factory failed: ${errorText(e)}`);
    }
    return JSON.stringify(summarizeExtension(ext));
  };

  function toolInfo(ext, tool) {
    return {
      name: tool.name,
      label: tool.label ?? tool.name,
      description: tool.description ?? "",
      parameters: tool.parameters,
      promptSnippet: tool.promptSnippet,
      promptGuidelines: tool.promptGuidelines,
      executionMode: tool.executionMode,
      hasRenderCall: typeof tool.renderCall === "function",
      hasRenderResult: typeof tool.renderResult === "function",
      extensionPath: ext.path,
    };
  }
  function commandInfo(ext, c) {
    return { name: c.name, description: c.description ?? "", hasCompletions: typeof c.getArgumentCompletions === "function", extensionPath: ext.path };
  }
  function summarizeExtension(ext) {
    return {
      path: ext.path,
      tools: [...ext.tools.values()].map((t) => toolInfo(ext, t)),
      commands: [...ext.commands.values()].map((c) => commandInfo(ext, c)),
      shortcuts: [...ext.shortcuts.values()].map((s) => ({ shortcut: s.shortcut, description: s.description ?? "" })),
      flags: [...ext.flags.values()].map((f) => ({ name: f.name, type: f.type, description: f.description ?? "", default: f.default })),
      events: [...ext.handlers.keys()],
      messageRenderers: [...ext.messageRenderers.keys()],
    };
  }
  globalThis.__setLoaded = () => { registry.state = "loaded"; };
  globalThis.__setFlagValues = (json) => { Object.assign(registry.flagValues, JSON.parse(json)); };
  globalThis.__listExtensions = () => JSON.stringify(registry.extensions.map(summarizeExtension));
  globalThis.__listTools = () => {
    const out = new Map();
    for (const ext of registry.extensions) for (const t of ext.tools.values()) out.set(t.name, toolInfo(ext, t));
    return JSON.stringify([...out.values()]);
  };
  globalThis.__listCommands = () => {
    const out = [];
    const counts = new Map();
    for (const ext of registry.extensions) for (const c of ext.commands.values()) {
      const n = (counts.get(c.name) || 0) + 1; counts.set(c.name, n);
      out.push({ ...commandInfo(ext, c), invocation: n === 1 ? c.name : `${c.name}:${n}` });
    }
    return JSON.stringify(out);
  };
  globalThis.__invalidateAll = () => { for (const ext of registry.extensions) ext.active = false; registry.extensions = []; };

  function findTool(name) {
    for (let i = registry.extensions.length - 1; i >= 0; i--) {
      const t = registry.extensions[i].tools.get(name);
      if (t) return { ext: registry.extensions[i], tool: t };
    }
    return undefined;
  }
  function findCommand(name) {
    let n = name, idx = 1;
    const m = /^(.*):(\d+)$/.exec(name);
    if (m) { n = m[1]; idx = Number(m[2]); }
    let seen = 0;
    for (const ext of registry.extensions) {
      const c = ext.commands.get(n);
      if (c) { seen++; if (seen === idx) return { ext, command: c }; }
    }
    return undefined;
  }

  // ------------------------------------------------------------------------
  // Dispatch
  // ------------------------------------------------------------------------
  function snapshotHandlers(event) {
    const out = [];
    for (const ext of registry.extensions) {
      const hs = ext.handlers.get(event);
      if (hs && hs.length) out.push({ ext, handlers: [...hs] });
    }
    return out;
  }

  globalThis.__hasHandlers = (event) => snapshotHandlers(event).length > 0;

  // Returns JSON: { result, event }
  globalThis.__dispatch = async function (name, eventJson, signalId) {
    const event = JSON.parse(eventJson);
    const signal = makeSignal(signalId);
    try {
      const ctx = createContext({ signal });
      const groups = snapshotHandlers(name);
      const run = async (ext, h, ev) => {
        try { return await h(ev, ctx); } catch (e) { reportError(ext, name, e); return undefined; }
      };
      let result;
      switch (name) {
        case "tool_call": {
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, event);
            if (r) { result = r; if (r.block) return finish(result, event); }
          }
          return finish(result, event);
        }
        case "tool_result": {
          let modified = false;
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, event);
            if (!r) continue;
            if (r.content !== undefined) { event.content = r.content; modified = true; }
            if (r.details !== undefined) { event.details = r.details; modified = true; }
            if (r.isError !== undefined) { event.isError = r.isError; modified = true; }
            if (r.usage !== undefined) { event.usage = r.usage; modified = true; }
          }
          return finish(modified ? { content: event.content, details: event.details, isError: event.isError, usage: event.usage } : undefined, event);
        }
        case "input": {
          let text = event.text, images = event.images;
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, { ...event, text, images });
            if (r?.action === "handled") return finish(r, event);
            if (r?.action === "transform") { text = r.text; images = r.images ?? images; }
          }
          return finish(text !== event.text || images !== event.images ? { action: "transform", text, images } : { action: "continue" }, event);
        }
        case "message_end": {
          let message = event.message;
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, { ...event, message });
            if (r?.message && r.message.role === message.role) message = r.message;
          }
          return finish(message !== event.message ? { message } : undefined, event);
        }
        case "before_agent_start": {
          const messages = [];
          let systemPrompt = event.systemPrompt;
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, { ...event, systemPrompt });
            if (r?.message) messages.push(r.message);
            if (typeof r?.systemPrompt === "string") systemPrompt = r.systemPrompt;
          }
          return finish({ messages, systemPrompt: systemPrompt !== event.systemPrompt ? systemPrompt : undefined, systemPromptOptions: event.systemPromptOptions }, event);
        }
        case "context": {
          let messages = event.messages;
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, { ...event, messages });
            if (r?.messages) messages = r.messages;
          }
          return finish(messages !== event.messages ? { messages } : undefined, event);
        }
        case "before_provider_request": {
          let payload = event.payload;
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, { ...event, payload });
            if (r !== undefined) payload = r;
          }
          return finish(payload !== event.payload ? { payload } : undefined, event);
        }
        case "before_provider_headers": {
          for (const { ext, handlers } of groups) for (const h of handlers) await run(ext, h, event);
          return finish({ headers: event.headers }, event);
        }
        case "session_before_switch":
        case "session_before_fork":
        case "session_before_compact":
        case "session_before_tree": {
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, event);
            if (r?.cancel) return finish(r, event);
            if (r) result = r;
          }
          return finish(result, event);
        }
        case "project_trust": {
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, event);
            if (r && (r.trusted === "yes" || r.trusted === "no")) return finish(r, event);
          }
          return finish(undefined, event);
        }
        case "resources_discover": {
          const agg = { skillPaths: [], promptPaths: [], themePaths: [] };
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, event);
            if (r?.skillPaths) agg.skillPaths.push(...r.skillPaths);
            if (r?.promptPaths) agg.promptPaths.push(...r.promptPaths);
            if (r?.themePaths) agg.themePaths.push(...r.themePaths);
          }
          return finish(agg, event);
        }
        case "user_bash": {
          for (const { ext, handlers } of groups) for (const h of handlers) {
            const r = await run(ext, h, event);
            if (r?.result) return finish({ result: r.result }, event);
            if (r?.operations) {
              const res = await r.operations.exec(event.command, event.cwd, { timeout: undefined });
              return finish({ result: res }, event);
            }
          }
          return finish(undefined, event);
        }
        default: {
          for (const { ext, handlers } of groups) for (const h of handlers) await run(ext, h, event);
          return finish(undefined, event);
        }
      }
    } finally {
      releaseSignal(signalId);
    }
  };
  const finish = (result, event) => JSON.stringify({ result: result === undefined ? null : result, event });

  // ------------------------------------------------------------------------
  // Tools, commands, shortcuts, renderers
  // ------------------------------------------------------------------------
  globalThis.__executeTool = async function (name, toolCallId, argsJson, signalId, onUpdate) {
    const found = findTool(name);
    if (!found) throw new Error(`Extension tool "${name}" not found`);
    const { ext, tool } = found;
    const params = JSON.parse(argsJson);
    const signal = makeSignal(signalId);
    try {
      const ctx = createContext({ signal });
      const update = (partial) => { try { onUpdate(JSON.stringify(normalizeResult(partial))); } catch (e) { reportError(ext, `tool:${name}:update`, e); } };
      const r = await tool.execute(toolCallId, params, signal, update, ctx);
      return JSON.stringify(normalizeResult(r));
    } finally {
      releaseSignal(signalId);
    }
  };
  function normalizeResult(r) {
    if (r === undefined || r === null) return { content: [], details: null };
    if (typeof r === "string") return { content: [{ type: "text", text: r }], details: null };
    const content = Array.isArray(r.content) ? r.content : r.content ? [r.content] : [];
    const out = { content: content.map((c) => (typeof c === "string" ? { type: "text", text: c } : c)), details: r.details === undefined ? null : stripFunctions(r.details) };
    if (r.usage) out.usage = r.usage;
    if (r.terminate) out.terminate = true;
    return out;
  }
  globalThis.__prepareToolArguments = function (name, argsJson) {
    const found = findTool(name);
    if (!found || typeof found.tool.prepareArguments !== "function") return argsJson;
    return JSON.stringify(found.tool.prepareArguments(JSON.parse(argsJson)));
  };

  globalThis.__runCommand = async function (name, args) {
    const found = findCommand(name);
    if (!found) throw new Error(`Extension command "${name}" not found`);
    const ctx = createCommandContext({});
    try {
      await found.command.handler(args ?? "", ctx);
    } catch (e) {
      reportError(found.ext, `command:${name}`, e);
      throw e;
    }
    return "null";
  };
  globalThis.__commandCompletions = function (name, prefix) {
    const found = findCommand(name);
    if (!found || typeof found.command.getArgumentCompletions !== "function") return "null";
    try { return JSON.stringify(found.command.getArgumentCompletions(prefix ?? "") ?? null); } catch { return "null"; }
  };
  globalThis.__runShortcut = async function (shortcut) {
    for (const ext of registry.extensions) {
      const s = ext.shortcuts.get(shortcut);
      if (s) { const ctx = createContext({}); try { await s.handler(ctx); } catch (e) { reportError(ext, `shortcut:${shortcut}`, e); } return "true"; }
    }
    return "false";
  };

  globalThis.__renderToolCall = function (name, argsJson, width) {
    const found = findTool(name);
    if (!found || typeof found.tool.renderCall !== "function") return "null";
    try {
      const c = found.tool.renderCall(JSON.parse(argsJson), theme, { args: JSON.parse(argsJson), toolCallId: "", invalidate() {}, lastComponent: undefined, state: {}, cwd: hostSync("process.cwd"), executionStarted: true, argsComplete: true, isPartial: false, expanded: false, showImages: false, isError: false });
      return JSON.stringify(c?.render ? c.render(width) : null);
    } catch (e) { reportError(found.ext, `renderCall:${name}`, e); return "null"; }
  };
  globalThis.__renderToolResult = function (name, resultJson, expanded, width) {
    const found = findTool(name);
    if (!found || typeof found.tool.renderResult !== "function") return "null";
    try {
      const result = JSON.parse(resultJson);
      const c = found.tool.renderResult(result, { expanded: !!expanded, isPartial: false }, theme, { args: {}, toolCallId: "", invalidate() {}, lastComponent: undefined, state: {}, cwd: hostSync("process.cwd"), executionStarted: true, argsComplete: true, isPartial: false, expanded: !!expanded, showImages: false, isError: false });
      return JSON.stringify(c?.render ? c.render(width) : null);
    } catch (e) { reportError(found.ext, `renderResult:${name}`, e); return "null"; }
  };
  globalThis.__renderMessage = function (customType, messageJson, width) {
    for (let i = registry.extensions.length - 1; i >= 0; i--) {
      const r = registry.extensions[i].messageRenderers.get(customType);
      if (r) { try { const c = r(JSON.parse(messageJson), { expanded: false, outputPad: 1 }, theme); return JSON.stringify(c?.render ? c.render(width) : null); } catch (e) { reportError(registry.extensions[i], `renderMessage:${customType}`, e); return "null"; } }
    }
    return "null";
  };

  // ------------------------------------------------------------------------
  // Utilities exposed to virtual modules
  // ------------------------------------------------------------------------
  globalThis.__pirs = {
    host,
    validate: (schema, v) => hostSync("validate", schema, v) ?? undefined,
    utf8Encode: (s) => { const b = hostSync("utf8.encode", s); return Uint8Array.from(b); },
    utf8Decode: (b) => hostSync("utf8.decode", Array.from(b instanceof ArrayBuffer ? new Uint8Array(b) : b)),
    base64ToUtf8: (b) => hostSync("base64.decode", b),
    base64ToLatin1: (b) => hostSync("base64.decode", b),
  };
})();
