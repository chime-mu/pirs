//! pirs: a Rust port of the pi coding agent.

mod agent_session;
mod modes;
mod session;
mod settings;
mod system_prompt;
mod tools;

use agent_session::{resolve_startup_model, AgentSession, SessionOptions, UiBackend};
use clap::Parser;
use pi_agent::ToolExecutionMode;
use pi_ai::{ModelRegistry, ThinkingLevel};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "pirs", version, about = "pi coding agent, in Rust", long_about = None)]
struct Args {
    /// Prompt text. In interactive mode it is sent as the first message.
    prompt: Vec<String>,
    /// Print mode: run the prompt(s) and exit.
    #[arg(short = 'p', long)]
    print: bool,
    /// Output mode for print mode: text or json.
    #[arg(long, default_value = "text")]
    mode: String,
    /// Model: provider/id, id, or a substring. Default: settings or first provider with credentials.
    #[arg(short = 'm', long)]
    model: Option<String>,
    /// Thinking level: off, minimal, low, medium, high, xhigh, max.
    #[arg(long)]
    thinking: Option<String>,
    /// Extension file or directory to load (repeatable).
    #[arg(short = 'e', long = "extension")]
    extensions: Vec<PathBuf>,
    /// Skip auto-discovered extensions (~/.pi/agent/extensions, .pi/extensions, settings).
    #[arg(long)]
    no_extensions: bool,
    /// Continue the most recent session for this directory.
    #[arg(short = 'c', long = "continue")]
    continue_session: bool,
    /// Resume a specific session file.
    #[arg(short = 'r', long)]
    resume: Option<PathBuf>,
    /// Do not persist the session.
    #[arg(long)]
    no_session: bool,
    /// Directory for session files (default: ~/.pi/agent/sessions/<cwd>).
    #[arg(long)]
    session_dir: Option<PathBuf>,
    /// Replace the default system prompt.
    #[arg(long)]
    system_prompt: Option<String>,
    /// Append text to the system prompt.
    #[arg(long)]
    append_system_prompt: Option<String>,
    /// Comma-separated built-in tools to enable (default: read,bash,edit,write).
    #[arg(long)]
    tools: Option<String>,
    /// Execute tool calls one at a time.
    #[arg(long)]
    sequential_tools: bool,
    /// Working directory.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// List models and exit.
    #[arg(long)]
    list_models: bool,
    /// Load extensions, print what they registered, and exit.
    #[arg(long)]
    list_extensions: bool,
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let code = rt.block_on(async_main());
    std::process::exit(code);
}

async fn async_main() -> i32 {
    let args = Args::parse();
    let cwd = match &args.cwd {
        Some(c) => c.clone(),
        None => std::env::current_dir().expect("cwd"),
    };
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let settings = settings::load_settings(&cwd);
    let registry = ModelRegistry::with_builtins();
    registry.set_agent_dir(settings::agent_dir());
    settings::load_models_json(&cwd, &registry);

    if args.list_models {
        for m in registry.models() {
            let auth = registry.credential_source(&m.provider).map(|s| format!("credentials: {s}")).unwrap_or_else(|| "credentials: none".into());
            println!("{:<40} {:<22} ctx={:<8} {}", m.key(), m.api, m.context_window, auth);
        }
        return 0;
    }

    let model = match resolve_startup_model(&registry, args.model.as_deref(), &settings) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let thinking = args
        .thinking
        .as_deref()
        .or(settings.default_thinking_level.as_deref())
        .and_then(ThinkingLevel::parse)
        .unwrap_or(if model.reasoning { ThinkingLevel::Off } else { ThinkingLevel::Off });

    let cwd_str = cwd.to_string_lossy().to_string();
    let session_dir = args.session_dir.as_deref();
    let session_result = if args.no_session {
        session::SessionManager::in_memory(&cwd_str)
    } else if let Some(path) = &args.resume {
        session::SessionManager::open_with(path, session_dir, None)
    } else if args.continue_session {
        session::SessionManager::continue_recent(&cwd_str, session_dir)
    } else {
        session::SessionManager::create(&cwd_str, session_dir)
    };
    let session_manager = match session_result {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not open session: {e}");
            return 1;
        }
    };

    let selected_tools: Vec<String> = match &args.tools {
        Some(t) => t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        None => tools::DEFAULT_TOOL_NAMES.iter().map(|s| s.to_string()).collect(),
    };

    let print_mode = args.print || args.list_extensions;
    let json = args.mode == "json";
    let print_ui = Arc::new(modes::print::PrintUi::new(json));
    let tui = Arc::new(modes::interactive::TuiBackend::new());
    let ui: Arc<dyn UiBackend> = if print_mode { print_ui.clone() } else { tui.clone() };

    let session = match AgentSession::new(SessionOptions {
        cwd: cwd.clone(),
        settings: settings.clone(),
        registry: registry.clone(),
        model: model.clone(),
        thinking_level: thinking,
        session: session_manager,
        ui,
        custom_prompt: args.system_prompt.clone(),
        append_system_prompt: args.append_system_prompt.clone(),
        selected_tools,
        tool_execution: if args.sequential_tools { ToolExecutionMode::Sequential } else { ToolExecutionMode::Parallel },
    })
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    // Extensions: auto-discovered roots, settings, then -e flags.
    let mut ext_paths: Vec<PathBuf> = Vec::new();
    if !args.no_extensions {
        ext_paths.extend(pi_ext::discover_extensions(&settings::extension_roots(&cwd)));
        ext_paths.extend(settings::settings_extension_paths(&settings, &cwd));
    }
    for e in &args.extensions {
        let p = if e.is_absolute() { e.clone() } else { cwd.join(e) };
        if p.is_dir() {
            // A directory is one extension (`index.ts`); otherwise treat it as a root of extensions.
            match ["index.ts", "index.js", "index.mjs"].iter().map(|i| p.join(i)).find(|f| f.is_file()) {
                Some(idx) => ext_paths.push(idx),
                None => ext_paths.extend(pi_ext::discover_extensions(&[p.clone()])),
            }
        } else {
            ext_paths.push(p);
        }
    }
    ext_paths.dedup();
    let mut load_failures = Vec::new();
    let mut loaded = Vec::new();
    if !ext_paths.is_empty() {
        for (path, r) in session.load_extensions(&ext_paths).await {
            match r {
                Ok(ext) => loaded.push(ext),
                Err(e) => load_failures.push((path, e)),
            }
        }
    }
    if args.list_extensions {
        for ext in &loaded {
            println!("{}", ext.path);
            for t in &ext.tools {
                println!("  tool     {}: {}", t.name, t.description.lines().next().unwrap_or(""));
            }
            for c in &ext.commands {
                println!("  command  /{}: {}", c.invocation.clone().unwrap_or(c.name.clone()), c.description);
            }
            for f in &ext.flags {
                println!("  flag     --{} ({})", f.name, f.kind);
            }
            if !ext.events.is_empty() {
                println!("  events   {}", ext.events.join(", "));
            }
        }
        for (p, e) in &load_failures {
            println!("FAILED {}: {}", p.display(), e.lines().next().unwrap_or(""));
        }
        return if load_failures.is_empty() { 0 } else { 1 };
    }
    for (p, e) in &load_failures {
        eprintln!("warning: failed to load extension {}: {}", p.display(), e.lines().next().unwrap_or(""));
    }

    session.emit_session_start("startup").await;

    let code = if print_mode {
        let prompts = if args.prompt.is_empty() { vec![] } else { vec![args.prompt.join(" ")] };
        if prompts.is_empty() {
            eprintln!("error: print mode requires a prompt");
            1
        } else {
            modes::print::run(&session, &print_ui, prompts).await
        }
    } else {
        let initial = if args.prompt.is_empty() { None } else { Some(args.prompt.join(" ")) };
        modes::interactive::run(session.clone(), tui, initial, &loaded, &load_failures).await
    };
    session.shutdown("quit").await;
    code
}
