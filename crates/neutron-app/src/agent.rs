//! The AI agents: Claude Code and the Gemini CLI, run in a terminal tab in
//! the folder being looked at, with Neutron's search given to them as tools.
//!
//! # Why the CLIs, in a terminal
//!
//! Each CLI is the whole agent — its model, its tools, its permission prompts,
//! the user's own subscription sign-in — and keeps improving on its own. Run
//! in Neutron's terminal it is all there, in the folder, with what is selected
//! described to it. Neutron adds what it knows how to do quickly: find files
//! anywhere on the disk by name or by what is in them, through Spotlight,
//! which no CLI's grep can match across a whole disk.
//!
//! # The search tools
//!
//! `neutron --mcp` is a Model Context Protocol server on stdin and stdout: one
//! JSON-RPC message per line. Each CLI is told to start it — Claude Code with
//! `--mcp-config`, the Gemini CLI through a defaults file of the lowest
//! priority, so a user's own settings and servers still win.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Which agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Gemini,
}

impl Agent {
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Gemini => "Gemini",
        }
    }
}

/// How an agent is started in `dir`, told about `selected`: through a login
/// shell, so it is found on the PATH the user's profile sets up — started
/// from the Dock, Neutron has none of it.
pub fn program(agent: Agent, dir: &Path, selected: &[PathBuf]) -> crate::term::Program {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let me = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "neutron".into());
    let server = json!({ "command": me, "args": ["--mcp"] });
    let mut env = Vec::new();
    let mut argv = vec![shell, "-l".into(), "-c".into(), "exec \"$0\" \"$@\"".into()];
    match agent {
        Agent::Claude => {
            argv.push("claude".into());
            // Reads and edits files without asking; commands still ask.
            argv.extend(["--permission-mode".into(), "acceptEdits".into()]);
            argv.extend(["--mcp-config".into(), json!({ "mcpServers": { "neutron": server } }).to_string()]);
            argv.extend(["--append-system-prompt".into(), context(dir, selected)]);
        }
        Agent::Gemini => {
            argv.push("gemini".into());
            argv.extend(["--approval-mode".into(), "auto_edit".into()]);
            if let Some(defaults) = gemini_defaults(&server) {
                env.push(("GEMINI_CLI_SYSTEM_DEFAULTS_PATH".into(), defaults.display().to_string()));
            }
        }
    }
    crate::term::Program { command: Some(argv), env }
}

/// What the agent is told about where it is.
fn context(dir: &Path, selected: &[PathBuf]) -> String {
    let mut text = format!(
        "You are running inside Neutron, the user's file manager, in a terminal tab opened on the folder {}. \
         Neutron gives you fast search tools backed by Spotlight: neutron_find_files finds files and folders by \
         name anywhere on the disk, neutron_search_contents finds files by the text inside them, and \
         neutron_fuzzy_find matches paths under a folder the way a fuzzy finder does. Prefer them to walking \
         or grepping large trees.",
        dir.display()
    );
    if !selected.is_empty() {
        text.push_str(" The user had these items selected in Neutron when they opened you:");
        for path in selected.iter().take(50) {
            text.push_str(&format!("\n- {}", path.display()));
        }
        if selected.len() > 50 {
            text.push_str(&format!("\n…and {} more.", selected.len() - 50));
        }
    }
    text
}

/// Writes the Gemini CLI's defaults — Neutron's server — to a file of
/// Neutron's own, and returns where. Only the server: the CLI has no way to be
/// handed extra instructions that does not replace its own or send a prompt,
/// so the folder it is in is all it is told of.
fn gemini_defaults(server: &Value) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/Neutron");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("gemini-defaults.json");
    let settings = json!({ "mcpServers": { "neutron": server } });
    std::fs::write(&path, serde_json::to_vec_pretty(&settings).ok()?).ok()?;
    Some(path)
}

// --- the MCP server ----------------------------------------------------------

/// Most results a tool returns.
const LIMIT: usize = 200;

/// Serves the search tools on stdin and stdout until stdin closes.
pub fn serve_mcp() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = answer(&line) {
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
        }
    }
}

/// The reply to one JSON-RPC message, or `None` for a notification.
fn answer(line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(m) => m,
        Err(e) => return Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } })),
    };
    let id = message.get("id").cloned()?;
    let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2024-11-05"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "neutron", "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            match call(name, &args) {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                Err(why) => json!({ "content": [{ "type": "text", "text": why }], "isError": true }),
            }
        }
        _ => return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("no method {method}") } })),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tools() -> Value {
    let folder = json!({ "type": "string", "description": "Only look under this folder (an absolute path). Optional." });
    json!([
        {
            "name": "neutron_find_files",
            "description": "Find files and folders by name anywhere on this Mac, through Spotlight. Case-insensitive; matches anywhere in the name. Best matches first.",
            "inputSchema": { "type": "object", "properties": {
                "name": { "type": "string", "description": "Part of the name to look for." },
                "folder": folder,
            }, "required": ["name"] },
        },
        {
            "name": "neutron_search_contents",
            "description": "Find files containing some text, through Spotlight's index of file contents. Much faster than grep across large trees; finds documents and PDFs as well as code.",
            "inputSchema": { "type": "object", "properties": {
                "text": { "type": "string", "description": "Words or a phrase to look for." },
                "folder": folder,
            }, "required": ["text"] },
        },
        {
            "name": "neutron_fuzzy_find",
            "description": "Fuzzy-match paths under a folder, as a fuzzy finder does: 'srcmain' finds src/main.rs. Walks up to 100,000 entries.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string" },
                "folder": { "type": "string", "description": "The folder to look under (an absolute path)." },
            }, "required": ["query", "folder"] },
        },
    ])
}

fn call(name: &str, args: &Value) -> Result<String, String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_owned).filter(|s| !s.trim().is_empty());
    let folder = text("folder").map(PathBuf::from);
    let found = match name {
        "neutron_find_files" => {
            let needle = text("name").ok_or("name is required")?;
            let escaped = escape(&needle);
            let mut found = spotlight(&format!("kMDItemFSName == \"*{escaped}*\"cd"), folder.as_deref())?;
            rank_by_name(&mut found, &needle);
            found
        }
        "neutron_search_contents" => {
            let needle = text("text").ok_or("text is required")?;
            let escaped = escape(&needle);
            spotlight(&format!("kMDItemTextContent == \"*{escaped}*\"cd"), folder.as_deref())?
        }
        "neutron_fuzzy_find" => {
            let needle = text("query").ok_or("query is required")?;
            let folder = folder.ok_or("folder is required")?;
            fuzzy(&folder, &needle)
        }
        _ => return Err(format!("no tool {name}")),
    };
    if found.is_empty() {
        return Ok("Nothing found.".into());
    }
    let shown = found.len().min(LIMIT);
    let mut out: String = found.iter().take(LIMIT).map(|p| format!("{}\n", p.display())).collect();
    if found.len() > LIMIT {
        out.push_str(&format!("…{} more not shown; narrow the search or give a folder.\n", found.len() - shown));
    }
    Ok(out)
}

/// Spotlight's query syntax, escaped.
fn escape(text: &str) -> String {
    text.chars().flat_map(|c| if matches!(c, '\\' | '"' | '*' | '?') { vec!['\\', c] } else { vec![c] }).collect()
}

/// Paths matching a Spotlight `predicate`, under `folder` if given.
fn spotlight(predicate: &str, folder: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    let mut cmd = Command::new("/usr/bin/mdfind");
    cmd.arg("-0");
    if let Some(folder) = folder {
        cmd.arg("-onlyin").arg(folder);
    }
    cmd.arg(predicate).stdout(Stdio::piped()).stderr(Stdio::null());
    let output = cmd.output().map_err(|e| format!("Spotlight could not be asked: {e}"))?;
    if !output.status.success() {
        return Err("Spotlight refused the search".into());
    }
    use std::os::unix::ffi::OsStrExt;
    Ok(output
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .take(20_000)
        .map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)))
        .collect())
}

/// Exact names first, then names that start with the needle, then shorter
/// names: the order the search overlay uses.
fn rank_by_name(paths: &mut [PathBuf], needle: &str) {
    let needle = needle.to_lowercase();
    paths.sort_by_cached_key(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let class = if name == needle { 0 } else if name.starts_with(&needle) { 1 } else { 2 };
        (class, name.len(), p.clone())
    });
}

/// Paths under `root` fuzzily matching `needle`, best first.
fn fuzzy(root: &Path, needle: &str) -> Vec<PathBuf> {
    let mut matcher = neutron_fuzzy::FuzzyMatcher::new();
    let mut scored = Vec::new();
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    let mut seen = 0;
    while let Some(dir) = queue.pop_front() {
        let Ok(listing) = std::fs::read_dir(&dir) else { continue };
        for entry in listing.flatten() {
            seen += 1;
            if seen > 100_000 {
                break;
            }
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                queue.push_back(path.clone());
            }
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            if let Some(found) = matcher.score(&relative, needle) {
                scored.push((found.score, path));
            }
        }
    }
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    scored.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_introduces_itself_and_lists_its_tools() {
        let init = answer(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(answer(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none(), "notifications get no reply");
        let list = answer(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["neutron_find_files", "neutron_search_contents", "neutron_fuzzy_find"]);
        let unknown = answer(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn fuzzy_find_matches_paths_under_a_folder() {
        let dir = std::env::temp_dir().join(format!("neutron-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        let reply = answer(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"neutron_fuzzy_find","arguments":{"query":"srcmain","folder":dir}}}).to_string()).unwrap();
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.lines().next().unwrap().ends_with("src/main.rs"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_agent_starts_through_a_login_shell_with_neutrons_tools() {
        let claude = program(Agent::Claude, Path::new("/tmp"), &[PathBuf::from("/tmp/a.txt")]);
        let argv = claude.command.unwrap();
        assert_eq!(argv[1..4], ["-l", "-c", "exec \"$0\" \"$@\""]);
        assert_eq!(argv[4], "claude");
        let config = &argv[argv.iter().position(|a| a == "--mcp-config").unwrap() + 1];
        assert!(config.contains("\"--mcp\""));
        assert!(argv.last().unwrap().contains("/tmp/a.txt"), "told what was selected");
    }
}
