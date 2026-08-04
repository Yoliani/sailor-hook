//! Write sailor-owned hook entries into supported agent config files.
//!
//! Phase 3 implements all eight agents in `config.rs`:
//!
//! - Claude Code / Codex / Gemini / Qwen — JSON settings files sharing the
//!   "nested" shape `{ hooks: { Event: [{ matcher, hooks: [{ type, command,
//!   timeout }] }] } }` (only the event tables and timeouts differ).
//! - Cursor — its own `hooks.json` with a flat shape: each event maps
//!   directly to hook definitions, no nesting.
//! - Kimi — `config.toml` with a flat `[[hooks]]` array.
//! - OpenCode — a TypeScript plugin file in the global plugin directory.
//! - pi — a TypeScript extension in pi's global extensions directory
//!   (`~/.pi/agent/extensions/`; the repo carries the same file at
//!   `.pi/extensions/sailor-hooks.ts`).
//!
//! Every agent's config is shared with the user's own settings and with
//! other tools' hooks, so the rules are the same across formats: never
//! rewrite what we didn't write, mark our entries so `uninstall` can find
//! them again, and stay idempotent.
//!
//! Three of the eight agents (Claude Code, Codex, Qwen) expose a
//! `PermissionRequest` hook whose stdout is read as a decision, so those get
//! the parked-approval wiring (`--wait-secs`). The others are events only:
//! Gemini, Kimi, and pi have no approval event at all, Cursor's gate hook
//! fires on *every* tool call (parking it would block the agent constantly),
//! and OpenCode's `permission.ask` plugin hook is defined in the SDK but
//! never triggered (anomalyco/opencode issues #7006/#9229).

use serde_json::{json, Value};

use crate::config::{self};

/// Every sailor-written hook command ends with this shell comment, and
/// nothing else does. It is what makes install idempotent and uninstall
/// surgical — a sentinel rather than a substring of the command itself,
/// because the binary path is quoted and may live anywhere.
pub const MARKER: &str = "# sailor-hook";

/// How long the shim parks a `PermissionRequest` waiting for the phone, and
/// the hook timeout that has to outlive it. The gap matters: if the agent
/// killed the hook first, the shim would never get to print its fallback and
/// the agent would see a hung hook instead of a normal prompt.
const APPROVAL_WAIT_SECS: u64 = 240;
const APPROVAL_TIMEOUT_SECS: u64 = 300;
const _: () = assert!(APPROVAL_TIMEOUT_SECS > APPROVAL_WAIT_SECS);

/// Everything else is fire-and-forget, so it only needs long enough to write
/// one line to a Unix socket.
const REPORT_TIMEOUT_SECS: u64 = 10;

/// Qwen (and Gemini) measure hook timeouts in *milliseconds*, unlike the
/// seconds-based agents. Same gap, different unit.
const QWEN_APPROVAL_TIMEOUT_MS: u64 = 300_000;
const QWEN_REPORT_TIMEOUT_MS: u64 = 10_000;
const GEMINI_REPORT_TIMEOUT_MS: u64 = 10_000;

/// Claude Code hook events we subscribe to, with the matcher each takes.
/// Tool events match every tool (`*`); the rest carry no matcher.
/// `PermissionRequest` is the only one whose stdout the agent reads as a
/// decision, so it is the only one that waits.
pub const CLAUDE_HOOKS: &[(&str, &str)] = &[
    ("SessionStart", ""),
    ("PreToolUse", "*"),
    ("PostToolUse", "*"),
    ("Notification", ""),
    ("PermissionRequest", "*"),
    ("Stop", ""),
];

/// Codex (OpenAI CLI). Same schema family as Claude Code — same nested shape,
/// same `PermissionRequest` decision JSON. `SubagentStop`/`SessionEnd` round
/// out the lifecycle; `Notification` doesn't exist here.
const CODEX_HOOKS: &[(&str, &str)] = &[
    ("SessionStart", ""),
    ("PreToolUse", "*"),
    ("PostToolUse", "*"),
    ("PermissionRequest", "*"),
    ("SubagentStop", ""),
    ("Stop", ""),
];

/// Gemini CLI. Event names are its own (`BeforeTool`/`AfterTool`); there is
/// no `PermissionRequest` event at all, so nothing here parks.
const GEMINI_HOOKS: &[(&str, &str)] = &[
    ("SessionStart", ""),
    ("BeforeTool", "*"),
    ("AfterTool", "*"),
    ("Notification", ""),
    ("SessionEnd", ""),
];

/// Cursor. Native config uses camelCase event names and a flat definition
/// shape (no nested `hooks` array). No `PermissionRequest`; `preToolUse`
/// could gate but fires on every tool call, so it is observation-only here.
const CURSOR_HOOKS: &[(&str, &str)] = &[
    ("sessionStart", ""),
    ("preToolUse", "*"),
    ("postToolUse", "*"),
    ("postToolUseFailure", "*"),
    ("stop", ""),
    ("sessionEnd", ""),
];

/// Kimi Code. Flat `[[hooks]]` array in TOML; `PermissionRequest` exists but
/// is observation-only (per the docs and moonshotai/kimi-cli#2154), so
/// nothing waits.
const KIMI_HOOKS: &[(&str, &str)] = &[
    ("SessionStart", ""),
    ("PreToolUse", "*"),
    ("PostToolUse", "*"),
    ("PostToolUseFailure", "*"),
    ("Notification", ""),
    ("Stop", ""),
    ("SessionEnd", ""),
];

/// Qwen Code. Same nested JSON shape; `PermissionRequest` decides from the
/// hook's stdout exactly like Claude Code's, so it parks. Timeouts are
/// milliseconds.
const QWEN_HOOKS: &[(&str, &str)] = &[
    ("SessionStart", ""),
    ("PreToolUse", "*"),
    ("PostToolUse", "*"),
    ("PostToolUseFailure", "*"),
    ("PermissionRequest", "*"),
    ("Notification", ""),
    ("Stop", ""),
    ("SessionEnd", ""),
];

fn waits_for_a_decision(agent: &str, event: &str) -> bool {
    event == "PermissionRequest" && matches!(agent, "claude_code" | "codex" | "qwen")
}

pub async fn run(agent: Option<String>) -> anyhow::Result<()> {
    let targets = config::targets_for(agent.as_deref())?;
    if targets.is_empty() {
        println!("no supported agents selected");
        return Ok(());
    }

    let exe = std::env::current_exe()?;
    let exe = exe.display().to_string();

    for t in &targets {
        match t.agent {
            "claude_code" => {
                let mut settings = read_settings(&t.path)?;
                let added = install_claude_hooks(&mut settings, &exe);
                write_settings(&t.path, &settings)?;
                println!(
                    "{}: installed {added} hook{} into {}",
                    t.agent,
                    if added == 1 { "" } else { "s" },
                    t.path.display()
                );
            }
            "codex" => {
                let mut settings = read_settings(&t.path)?;
                let added = install_nested_hooks(
                    &mut settings,
                    &exe,
                    "codex",
                    CODEX_HOOKS,
                    |event| waits_for_a_decision("codex", event),
                    seconds_timeout,
                    none_extra,
                );
                write_settings(&t.path, &settings)?;
                println!(
                    "{}: installed {added} hook{} into {}",
                    t.agent,
                    if added == 1 { "" } else { "s" },
                    t.path.display()
                );
                // Codex refuses to run un-trusted hook definitions; the trust
                // review is interactive (`/hooks`) and can't be automated.
                println!("  note: run `codex` once and trust the sailor-hook entries via `/hooks`");
            }
            "gemini" => {
                let mut settings = read_settings(&t.path)?;
                let added = install_nested_hooks(
                    &mut settings,
                    &exe,
                    "gemini",
                    GEMINI_HOOKS,
                    |event| waits_for_a_decision("gemini", event),
                    ms_timeout(GEMINI_REPORT_TIMEOUT_MS),
                    |_, _| json!({ "name": "sailor-hook" }),
                );
                write_settings(&t.path, &settings)?;
                println!(
                    "{}: installed {added} hook{} into {}",
                    t.agent,
                    if added == 1 { "" } else { "s" },
                    t.path.display()
                );
            }
            "qwen" => {
                let mut settings = read_settings(&t.path)?;
                let added = install_nested_hooks(
                    &mut settings,
                    &exe,
                    "qwen",
                    QWEN_HOOKS,
                    |event| waits_for_a_decision("qwen", event),
                    |waits| {
                        if waits {
                            QWEN_APPROVAL_TIMEOUT_MS
                        } else {
                            QWEN_REPORT_TIMEOUT_MS
                        }
                    },
                    none_extra,
                );
                write_settings(&t.path, &settings)?;
                println!(
                    "{}: installed {added} hook{} into {}",
                    t.agent,
                    if added == 1 { "" } else { "s" },
                    t.path.display()
                );
            }
            "cursor" => {
                let mut settings = read_settings(&t.path)?;
                let added = install_cursor_hooks(&mut settings, &exe);
                write_settings(&t.path, &settings)?;
                println!(
                    "{}: installed {added} hook{} into {}",
                    t.agent,
                    if added == 1 { "" } else { "s" },
                    t.path.display()
                );
            }
            "kimi" => {
                let mut value = kimi::read_toml(&t.path)?;
                let added = kimi::install_hooks(&mut value, &exe);
                kimi::write_toml(&t.path, &value)?;
                println!(
                    "{}: installed {added} hook{} into {}",
                    t.agent,
                    if added == 1 { "" } else { "s" },
                    t.path.display()
                );
            }
            "opencode" => {
                let added = opencode::install_plugin(&t.path)?;
                println!(
                    "{}: wrote {} ({} hook{})",
                    t.agent,
                    t.path.display(),
                    added,
                    if added == 1 { "" } else { "s" }
                );
                println!("  note: restart opencode to load the plugin");
            }
            "pi" => {
                let added = pi::install_extension(&t.path)?;
                println!(
                    "{}: wrote {} ({} event kind{})",
                    t.agent,
                    t.path.display(),
                    added,
                    if added == 1 { "" } else { "s" }
                );
                println!("  note: run `/reload` in pi (or start a new session) to load it");
            }
            other => println!("{other}: not yet implemented, skipped"),
        }
    }
    Ok(())
}

/// Read an agent settings file, treating "missing" and "empty" as `{}` so a
/// first install works on a machine that has never run the agent.
pub fn read_settings(path: &std::path::Path) -> anyhow::Result<Value> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(e.into()),
    };
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", path.display()))?;
    Ok(value)
}

/// Write settings back, keeping a `.bak` of whatever was there before. The
/// file belongs to the user, so a bad merge has to be recoverable.
pub fn write_settings(path: &std::path::Path, settings: &Value) -> anyhow::Result<()> {
    write_with_bak_json(path, settings)
}

fn write_with_bak(path: &std::path::Path, contents: &str, ext: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        std::fs::copy(path, path.with_extension(format!("{ext}.bak")))?;
    }
    std::fs::write(path, contents)?;
    Ok(())
}

fn write_with_bak_json(path: &std::path::Path, settings: &Value) -> anyhow::Result<()> {
    let mut out = serde_json::to_string_pretty(settings)?;
    out.push('\n');
    write_with_bak(path, &out, "json")
}

/// The nested `{ hooks: { Event: [...] } }` shape shared by Claude Code,
/// Codex, Gemini, and Qwen. `timeout_for` returns the hook timeout (seconds
/// or milliseconds per agent); `extra_for` adds per-agent fields (Gemini
/// wants a `name` on each hook).
fn install_nested_hooks(
    settings: &mut Value,
    exe: &str,
    agent: &str,
    events: &[(&str, &str)],
    is_waiting: impl Fn(&str) -> bool,
    timeout_for: impl Fn(bool) -> u64,
    extra_for: impl Fn(&str, bool) -> Value,
) -> usize {
    strip_sailor_hooks(settings);

    let Some(root) = settings.as_object_mut() else {
        return 0;
    };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return 0;
    };

    for (event, matcher) in events {
        let waits = is_waiting(event);
        let mut entry = json!({
            "matcher": matcher,
            "hooks": [{
                "type": "command",
                "command": command_for(exe, agent, waits),
                "timeout": timeout_for(waits),
            }],
        });
        let extra = extra_for(event, waits);
        if let Some(map) = extra.as_object() {
            // Extra fields go on the hook config (the object inside `hooks`),
            // not on the matcher-group entry.
            if let Some(hook) = entry
                .get_mut("hooks")
                .and_then(Value::as_array_mut)
                .and_then(|l| l.get_mut(0))
                .and_then(Value::as_object_mut)
            {
                for (k, v) in map {
                    hook.insert(k.clone(), v.clone());
                }
            }
        }
        match hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
        {
            Some(list) => list.push(entry),
            // Someone hand-wrote a non-array there; replace rather than lose
            // our hook.
            None => {
                hooks.insert((*event).to_string(), json!([entry]));
            }
        }
    }
    events.len()
}

/// Merge sailor's hook entries into `settings`, replacing any it already
/// owns. Returns how many were written.
pub fn install_claude_hooks(settings: &mut Value, exe: &str) -> usize {
    install_nested_hooks(
        settings,
        exe,
        "claude_code",
        CLAUDE_HOOKS,
        |event| waits_for_a_decision("claude_code", event),
        seconds_timeout,
        none_extra,
    )
}

fn seconds_timeout(waits: bool) -> u64 {
    if waits {
        APPROVAL_TIMEOUT_SECS
    } else {
        REPORT_TIMEOUT_SECS
    }
}

fn ms_timeout(report_ms: u64) -> impl Fn(bool) -> u64 {
    move |waits| {
        if waits {
            QWEN_APPROVAL_TIMEOUT_MS
        } else {
            report_ms
        }
    }
}

fn none_extra(_event: &str, _waits: bool) -> Value {
    Value::Null
}

// --- Cursor (flat JSON shape) ----------------------------------------------

fn install_cursor_hooks(settings: &mut Value, exe: &str) -> usize {
    strip_cursor_hooks(settings);

    let Some(root) = settings.as_object_mut() else {
        return 0;
    };
    // Schema marker Cursor expects.
    root.entry("version").or_insert_with(|| json!(1));
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return 0;
    };

    for (event, matcher) in CURSOR_HOOKS {
        // Cursor's shape is flat: the array holds hook definitions directly.
        let mut entry = json!({
            "command": command_for(exe, "cursor", false),
            "timeout": REPORT_TIMEOUT_SECS,
        });
        if !matcher.is_empty() && *matcher != "*" {
            entry["matcher"] = json!(matcher);
        }
        match hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
        {
            Some(list) => list.push(entry),
            None => {
                hooks.insert((*event).to_string(), json!([entry]));
            }
        }
    }
    CURSOR_HOOKS.len()
}

/// Remove every sailor-owned hook from Cursor's flat shape. Returns how many
/// were removed.
pub fn strip_cursor_hooks(settings: &mut Value) -> usize {
    let Some(hooks) = settings
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(Value::as_object_mut)
    else {
        return 0;
    };

    let mut removed = 0;
    let mut empty_events = Vec::new();
    for (event, entries) in hooks.iter_mut() {
        let Some(entries) = entries.as_array_mut() else {
            continue;
        };
        let before = entries.len();
        entries.retain(|e| !is_sailor_hook(e));
        removed += before - entries.len();
        if entries.is_empty() {
            empty_events.push(event.clone());
        }
    }
    for event in empty_events {
        hooks.remove(&event);
    }
    if hooks.is_empty() {
        if let Some(root) = settings.as_object_mut() {
            root.remove("hooks");
        }
    }
    removed
}

/// Remove every sailor-owned hook, pruning entries and event keys that end up
/// empty. Returns how many hook commands were removed. Works on the nested
/// shape (Claude Code, Codex, Gemini, Qwen).
pub fn strip_sailor_hooks(settings: &mut Value) -> usize {
    let Some(hooks) = settings
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(Value::as_object_mut)
    else {
        return 0;
    };

    let mut removed = 0;
    let mut empty_events = Vec::new();
    for (event, entries) in hooks.iter_mut() {
        let Some(entries) = entries.as_array_mut() else {
            continue;
        };
        for entry in entries.iter_mut() {
            let Some(list) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = list.len();
            list.retain(|h| !is_sailor_hook(h));
            removed += before - list.len();
        }
        entries.retain(|e| match e.get("hooks").and_then(Value::as_array) {
            Some(list) => !list.is_empty(),
            None => true,
        });
        if entries.is_empty() {
            empty_events.push(event.clone());
        }
    }
    for event in empty_events {
        hooks.remove(&event);
    }
    if hooks.is_empty() {
        if let Some(root) = settings.as_object_mut() {
            root.remove("hooks");
        }
    }
    removed
}

fn is_sailor_hook(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(MARKER))
}

/// Guarded so an uninstalled or moved binary can't break the agent: if the
/// shim isn't there, swallow the payload the agent wrote to stdin and exit
/// clean, exactly as if no hook were configured.
fn command_for(exe: &str, agent: &str, waits: bool) -> String {
    let args = if waits {
        format!("event --agent {agent} --wait-secs {APPROVAL_WAIT_SECS}")
    } else {
        format!("event --agent {agent}")
    };
    format!("if [ -x '{exe}' ]; then '{exe}' {args}; else cat >/dev/null 2>&1 || :; fi {MARKER}")
}

/// Which agents currently have sailor hooks installed — used by `status`.
pub fn installed_agents() -> Vec<&'static str> {
    let Ok(targets) = config::targets_for(None) else {
        return Vec::new();
    };
    targets
        .iter()
        .filter(|t| installed_for_agent(t))
        .map(|t| t.agent)
        .collect()
}

fn installed_for_agent(t: &config::Target) -> bool {
    match t.agent {
        "kimi" => kimi::is_installed(&t.path),
        "opencode" => opencode::is_installed(&t.path),
        "pi" => pi::is_installed(&t.path),
        "cursor" => read_settings(&t.path)
            .map(|s| {
                s.get("hooks")
                    .and_then(Value::as_object)
                    .is_some_and(|hooks| {
                        hooks.values().any(|entries| {
                            entries.as_array().into_iter().flatten().any(is_sailor_hook)
                        })
                    })
            })
            .unwrap_or(false),
        _ => read_settings(&t.path)
            .map(|s| has_sailor_hook(&s))
            .unwrap_or(false),
    }
}

fn has_sailor_hook(settings: &Value) -> bool {
    settings
        .get("hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks.values().any(|entries| {
                entries
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.get("hooks").and_then(Value::as_array))
                    .flatten()
                    .any(is_sailor_hook)
            })
        })
        .unwrap_or(false)
}

// --- Kimi (TOML) ------------------------------------------------------------

pub mod kimi {
    //! Kimi Code's config is `~/.kimi-code/config.toml` with a flat
    //! `[[hooks]]` array: each rule is `{ event, matcher, command, timeout }`
    //! and only those four fields (extra ones fail the file to load). We
    //! append one rule per subscribed event, marked by the same
    //! `# sailor-hook` sentinel in the command, and strip by that marker.

    use toml::Value;

    pub const KIMI_HOOKS: &[(&str, &str)] = super::KIMI_HOOKS;

    pub fn read_toml(path: &std::path::Path) -> anyhow::Result<Value> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Value::Table(Default::default()));
            }
            Err(e) => return Err(e.into()),
        };
        if raw.trim().is_empty() {
            return Ok(Value::Table(Default::default()));
        }
        toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("{} is not valid TOML: {e}", path.display()))
    }

    pub fn write_toml(path: &std::path::Path, value: &Value) -> anyhow::Result<()> {
        let mut out = toml::to_string_pretty(value)?;
        out.push('\n');
        super::write_with_bak(path, &out, "toml")
    }

    /// Append one `[[hooks]]` rule per subscribed event, replacing any we own.
    pub fn install_hooks(value: &mut Value, exe: &str) -> usize {
        strip_hooks(value);

        let Some(table) = value.as_table_mut() else {
            return 0;
        };
        let hooks = table
            .entry("hooks".to_string())
            .or_insert_with(|| Value::Array(vec![]));
        let Some(arr) = hooks.as_array_mut() else {
            return 0;
        };

        for (event, matcher) in KIMI_HOOKS {
            let mut rule = toml::map::Map::new();
            rule.insert("event".into(), Value::String((*event).into()));
            rule.insert(
                "command".into(),
                Value::String(super::command_for(exe, "kimi", false)),
            );
            rule.insert("matcher".into(), Value::String((*matcher).into()));
            rule.insert(
                "timeout".into(),
                Value::Integer(super::REPORT_TIMEOUT_SECS as i64),
            );
            arr.push(Value::Table(rule));
        }
        KIMI_HOOKS.len()
    }

    /// Remove every sailor-owned rule; drop `hooks` when it ends up empty.
    pub fn strip_hooks(value: &mut Value) -> usize {
        let Some(table) = value.as_table_mut() else {
            return 0;
        };
        let Some(hooks) = table.get_mut("hooks") else {
            return 0;
        };
        let Some(arr) = hooks.as_array_mut() else {
            return 0;
        };
        let before = arr.len();
        arr.retain(|rule| {
            !rule
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| c.contains(super::MARKER))
        });
        let removed = before - arr.len();
        if arr.is_empty() {
            table.remove("hooks");
        }
        removed
    }

    pub fn is_installed(path: &std::path::Path) -> bool {
        read_toml(path)
            .map(|value| {
                value
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|arr| {
                        arr.iter().any(|rule| {
                            rule.get("command")
                                .and_then(Value::as_str)
                                .is_some_and(|c| c.contains(super::MARKER))
                        })
                    })
            })
            .unwrap_or(false)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const EXE: &str = "/usr/local/bin/sailor-hook";

        #[test]
        fn installs_and_strips_idempotently() {
            let mut value = read_toml(std::path::Path::new("/nonexistent")).unwrap();
            assert_eq!(install_hooks(&mut value, EXE), KIMI_HOOKS.len());
            let once = value.clone();
            install_hooks(&mut value, EXE);
            assert_eq!(value, once);
            assert!(is_installed_in(&value));

            assert_eq!(strip_hooks(&mut value), KIMI_HOOKS.len());
            assert!(!is_installed_in(&value));
            assert!(value.get("hooks").is_none());
        }

        #[test]
        fn leaves_other_toml_keys_alone() {
            let mut value: Value = toml::from_str(
                r#"
model = "kimi-k2"
[ui]
theme = "dark"
"#,
            )
            .unwrap();
            install_hooks(&mut value, EXE);
            assert_eq!(value["model"].as_str(), Some("kimi-k2"));
            assert_eq!(value["ui"]["theme"].as_str(), Some("dark"));
            assert!(value["hooks"].as_array().is_some());

            strip_hooks(&mut value);
            assert!(value.get("hooks").is_none());
            assert_eq!(value["model"].as_str(), Some("kimi-k2"));
        }

        fn is_installed_in(value: &Value) -> bool {
            value
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|arr| {
                    arr.iter().any(|rule| {
                        rule.get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|c| c.contains(super::super::MARKER))
                    })
                })
        }
    }
}

// --- OpenCode (TS plugin) ----------------------------------------------------

pub mod opencode {
    //! OpenCode loads plugins from a plugin directory; there is no hooks
    //! file to merge into. We write one self-contained TypeScript plugin
    //! that forwards the events sailor models to the shim, guarded so a
    //! missing binary or daemon never stalls the agent. Install is
    //! idempotent: if the file already carries our marker it is left alone
    //! unless the embedded shim path changed.

    const PLUGIN_MARKER: &str = "// sailor-hook — managed by `sailor-hook install`";

    const PLUGIN: &str = r#"// sailor-hook — managed by `sailor-hook install`. Do not edit; run
// `sailor-hook uninstall --agent opencode` to remove.
//
// Reports opencode agent activity to the sailor host daemon. Uses the typed
// tool hooks plus the generic event stream (opencode's `permission.ask`
// plugin hook is declared in the SDK but never triggered — see
// anomalyco/opencode#7006/#9229 — so approvals surface as events only).
// Fails soft: a missing binary or daemon must never stall the agent.

const AGENT = "opencode";

function forward(payload: {
	hook_event_name: string;
	session_id?: string;
	cwd?: string;
	tool_name?: string;
	message?: string;
}): void {
	try {
		const proc = Bun.spawn({
			cmd: ["sailor-hook", "event", "--agent", AGENT],
			stdin: "pipe",
			stdout: "ignore",
			stderr: "ignore",
		});
		proc.stdin.write(JSON.stringify(payload) + "\n");
		proc.stdin.end();
	} catch {
		// ignore: no binary, no room — never fail the agent
	}
}

function str(v: unknown): string | undefined {
	return typeof v === "string" && v.length > 0 ? v : undefined;
}

export default async function sailorHooks(ctx: {
	directory?: string;
}): Promise<{
	event?: (input: { event: { type: string; data?: Record<string, unknown> } }) => Promise<void>;
	"tool.execute.before"?: (input: {
		tool: string;
		sessionID: string;
		callID: string;
	}) => Promise<void>;
	"tool.execute.after"?: (input: {
		tool: string;
		sessionID: string;
		callID: string;
	}) => Promise<void>;
}> {
	const cwd = str(ctx.directory);
	return {
		event: async ({ event }) => {
			const data = event.data ?? {};
			const sessionID = str(data.sessionID) ?? str(data.session_id);
			switch (event.type) {
				case "session.created":
					forward({ hook_event_name: "session.created", session_id: sessionID, cwd });
					break;
				case "session.idle":
					forward({ hook_event_name: "session.idle", session_id: sessionID, cwd });
					break;
				case "permission.asked":
				case "permission.v2.asked":
					forward({
						hook_event_name: "permission.asked",
						session_id: sessionID,
						cwd,
						message: str(data.action) ?? str(data.tool),
					});
					break;
				case "permission.replied":
				case "permission.v2.replied":
					forward({
						hook_event_name: "permission.replied",
						session_id: sessionID,
						cwd,
						message: str(data.reply),
					});
					break;
				default:
					break;
			}
		},
		"tool.execute.before": async ({ tool, sessionID }) => {
			forward({ hook_event_name: "tool.execute.before", session_id: sessionID, cwd, tool_name: tool });
		},
		"tool.execute.after": async ({ tool, sessionID }) => {
			forward({ hook_event_name: "tool.execute.after", session_id: sessionID, cwd, tool_name: tool });
		},
	};
}
"#;

    /// Write the plugin, but only when it differs from what's on disk — a
    /// shared file shouldn't be rewritten (and mtime-churned) on every
    /// install. Returns how many hooks the plugin subscribes to (all of
    /// them — it's one file), for the install summary.
    pub fn install_plugin(path: &std::path::Path) -> anyhow::Result<usize> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(path)
            .map(|s| s.contains(PLUGIN_MARKER) && s.contains(AGENT_SHIM))
            .unwrap_or(false)
        {
            return Ok(5);
        }
        std::fs::write(path, PLUGIN)?;
        Ok(5)
    }

    const AGENT_SHIM: &str = "sailor-hook\", \"event\", \"--agent\"";

    pub fn is_installed(path: &std::path::Path) -> bool {
        std::fs::read_to_string(path)
            .map(|s| s.contains(PLUGIN_MARKER))
            .unwrap_or(false)
    }

    /// Remove the plugin if we own it.
    pub fn uninstall_plugin(path: &std::path::Path) -> anyhow::Result<bool> {
        if !is_installed(path) {
            return Ok(false);
        }
        std::fs::remove_file(path)?;
        Ok(true)
    }
}
// --- pi (TS extension) ---------------------------------------------------------

pub mod pi {
    //! pi has no hooks file; its extension system is the hook surface. This
    //! module installs a pi extension into pi's global extensions directory
    //! (`~/.pi/agent/extensions/`), so every pi session reports to the
    //! daemon. The repo carries the same file at `.pi/extensions/
    //! sailor-hooks.ts` (project-local scope); keep the two in sync.
    //!
    //! pi extensions auto-load from `~/.pi/agent/extensions/*.ts` — no
    //! settings.json registration needed.

    const EXTENSION_MARKER: &str = "sailor-hook — managed by";

    const EXTENSION: &str = r#"// sailor-hook — managed by `sailor-hook install --agent pi`.
// Do not edit; run `sailor-hook uninstall --agent pi` to remove.
//
// Reports pi agent activity to the sailor host daemon (the phone's agent
// inbox). Same contract as the other agents' hooks: fire `sailor-hook event
// --agent pi` with a normalized payload on stdin; the daemon's `pi` adapter
// maps it onto the five inbox categories. pi's hooks are observers — there
// is no decision channel, so approvals never park (Herdr's native pane
// state covers blocked/working/done).
//
// Also reports context usage: `ctx.getContextUsage()` is one of the few
// sources of real context data in the agent landscape, so the rings get
// fed from it on `agent_settled`.
//
// Fails soft everywhere: a missing binary, an absent daemon, or a dead
// spawn must never stall the agent.

import { execSync, spawn } from "node:child_process";

const AGENT = "pi";

/** Resolve `sailor-hook` once: PATH first (spawn error tells us), then the
 * usual cargo install location. */
let sailorHookBin: string | undefined;

function resolveBin(): string | undefined {
	if (sailorHookBin !== undefined) return sailorHookBin;
	const candidates = [
		"sailor-hook",
		`${process.env.HOME ?? ""}/.cargo/bin/sailor-hook`,
	];
	for (const candidate of candidates) {
		try {
			// command -v is authoritative about PATH resolution; for the
			// explicit path we only need it to exist.
			if (
				candidate === "sailor-hook"
					? !!runSync("command -v sailor-hook")
					: candidate.length > 0
			) {
				sailorHookBin = candidate;
				return candidate;
			}
		} catch {
			// try the next candidate
		}
	}
	sailorHookBin = "";
	return undefined;
}

function runSync(command: string): string | undefined {
	try {
		return execSync(command, { encoding: "utf8", timeout: 2000 }).trim() || undefined;
	} catch {
		return undefined;
	}
}

function forward(payload: Record<string, unknown>): void {
	const bin = resolveBin();
	if (!bin) return;
	try {
		const child = spawn(bin, ["event", "--agent", AGENT], {
			stdio: ["pipe", "ignore", "ignore"],
		});
		child.on("error", () => {
			// ENOENT or a dead daemon side — never fail the agent.
		});
		child.stdin.write(`${JSON.stringify(payload)}\n`);
		child.stdin.end();
	} catch {
		// ignore
	}
}

type SailorPayload = {
	hook_event_name: string;
	session_id?: string;
	cwd?: string;
	tool_name?: string;
	context_usage?: number;
};

function str(v: unknown): string | undefined {
	return typeof v === "string" && v.length > 0 ? v : undefined;
}

/** Context fraction used, from ctx.getContextUsage() and the active model's
 * window. Returns undefined when either side is missing. */
function contextUsage(ctx: any): number | undefined {
	try {
		const usage = ctx?.getContextUsage?.();
		const tokens = typeof usage?.tokens === "number" ? usage.tokens : undefined;
		const window = ctx?.model?.contextWindow;
		if (tokens === undefined || typeof window !== "number" || window <= 0) {
			return undefined;
		}
		return Math.min(1, Math.max(0, tokens / window));
	} catch {
		return undefined;
	}
}

export default function (pi: any) {
	let sessionActive = false;

	pi.on("session_start", (_event: unknown, ctx: any) => {
		sessionActive = true;
		const payload: SailorPayload = {
			hook_event_name: "session_start",
			session_id: str(ctx?.sessionManager?.getSessionId?.()),
			cwd: str(ctx?.cwd),
		};
		forward(payload);
	});

	pi.on("agent_start", (_event: unknown, ctx: any) => {
		if (!sessionActive) return;
		forward({
			hook_event_name: "agent_start",
			session_id: str(ctx?.sessionManager?.getSessionId?.()),
			cwd: str(ctx?.cwd),
		});
	});

	pi.on("tool_execution_start", (event: any, ctx: any) => {
		if (!sessionActive) return;
		forward({
			hook_event_name: "tool_execution_start",
			session_id: str(ctx?.sessionManager?.getSessionId?.()),
			cwd: str(ctx?.cwd),
			tool_name: str(event?.toolName),
		});
	});

	pi.on("tool_execution_end", (event: any, ctx: any) => {
		if (!sessionActive) return;
		forward({
			hook_event_name: "tool_execution_end",
			session_id: str(ctx?.sessionManager?.getSessionId?.()),
			cwd: str(ctx?.cwd),
			tool_name: str(event?.toolName),
		});
	});

	// agent_end = the turn finished (a retry may still follow); agent_settled
	// = definitively idle. Both mean "task complete" to the inbox, and the
	// settled event is where the context usage rides.
	pi.on("agent_end", (_event: unknown, ctx: any) => {
		if (!sessionActive) return;
		forward({
			hook_event_name: "agent_end",
			session_id: str(ctx?.sessionManager?.getSessionId?.()),
			cwd: str(ctx?.cwd),
		});
	});

	pi.on("agent_settled", (_event: unknown, ctx: any) => {
		if (!sessionActive) return;
		const payload: SailorPayload = {
			hook_event_name: "agent_settled",
			session_id: str(ctx?.sessionManager?.getSessionId?.()),
			cwd: str(ctx?.cwd),
		};
		const usage = contextUsage(ctx);
		if (usage !== undefined) {
			payload.context_usage = usage;
		}
		forward(payload);
	});

	// /reload, /new, /resume rebind the runtime — the row stays; only a real
	// quit is worth nothing (the inbox ages rows out on its own).
	pi.on("session_shutdown", () => {
		sessionActive = false;
	});
}
"#;

    /// Write the extension, but only when it differs from what's on disk.
    /// Returns how many event kinds the extension subscribes to, for the
    /// install summary.
    pub fn install_extension(path: &std::path::Path) -> anyhow::Result<usize> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(path)
            .map(|s| s.contains(EXTENSION_MARKER))
            .unwrap_or(false)
        {
            return Ok(6);
        }
        std::fs::write(path, EXTENSION)?;
        Ok(6)
    }

    pub fn is_installed(path: &std::path::Path) -> bool {
        std::fs::read_to_string(path)
            .map(|s| s.contains(EXTENSION_MARKER))
            .unwrap_or(false)
    }

    /// Remove the extension if we own it.
    pub fn uninstall_extension(path: &std::path::Path) -> anyhow::Result<bool> {
        if !is_installed(path) {
            return Ok(false);
        }
        std::fs::remove_file(path)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/usr/local/bin/sailor-hook";

    fn foreign_settings() -> Value {
        json!({
            "editorMode": "vim",
            "hooks": {
                "PostToolUse": [{
                    "matcher": "*",
                    "hooks": [{ "type": "command", "command": "/other/tool.sh", "timeout": 10 }],
                }],
            },
        })
    }

    #[test]
    fn installs_every_subscribed_event() {
        let mut settings = json!({});
        assert_eq!(install_claude_hooks(&mut settings, EXE), CLAUDE_HOOKS.len());
        let hooks = settings["hooks"].as_object().unwrap();
        for (event, _) in CLAUDE_HOOKS {
            assert!(hooks.contains_key(*event), "missing {event}");
        }
        assert!(has_sailor_hook(&settings));
    }

    #[test]
    fn is_idempotent() {
        let mut settings = json!({});
        install_claude_hooks(&mut settings, EXE);
        let once = settings.clone();
        install_claude_hooks(&mut settings, EXE);
        assert_eq!(settings, once);
    }

    #[test]
    fn leaves_other_tools_hooks_and_settings_alone() {
        let mut settings = foreign_settings();
        install_claude_hooks(&mut settings, EXE);
        // The foreign PostToolUse hook is still there alongside ours.
        assert_eq!(
            settings["hooks"]["PostToolUse"].as_array().unwrap().len(),
            2
        );
        strip_sailor_hooks(&mut settings);
        assert_eq!(settings, foreign_settings());
    }

    #[test]
    fn uninstall_reports_what_it_removed_and_prunes_empties() {
        let mut settings = json!({});
        install_claude_hooks(&mut settings, EXE);
        assert_eq!(strip_sailor_hooks(&mut settings), CLAUDE_HOOKS.len());
        // Nothing else lived there, so the whole `hooks` key goes.
        assert!(settings.get("hooks").is_none());
        assert!(!has_sailor_hook(&settings));
    }

    #[test]
    fn stripping_a_file_we_never_touched_is_a_no_op() {
        let mut settings = foreign_settings();
        assert_eq!(strip_sailor_hooks(&mut settings), 0);
        assert_eq!(settings, foreign_settings());
    }

    #[test]
    fn command_falls_back_to_draining_stdin() {
        let cmd = command_for(EXE, "claude_code", false);
        assert!(cmd.contains(MARKER));
        assert!(cmd.contains("cat >/dev/null"));
    }

    #[test]
    fn only_permission_request_waits_and_its_hook_outlives_the_wait() {
        let mut settings = json!({});
        install_claude_hooks(&mut settings, EXE);
        let hooks = &settings["hooks"];

        let permission = &hooks["PermissionRequest"][0]["hooks"][0];
        assert!(permission["command"]
            .as_str()
            .unwrap()
            .contains(&format!("--wait-secs {APPROVAL_WAIT_SECS}")));
        // The agent must not kill the shim before it can print its fallback.
        assert_eq!(permission["timeout"], APPROVAL_TIMEOUT_SECS);

        for event in [
            "SessionStart",
            "PreToolUse",
            "PostToolUse",
            "Notification",
            "Stop",
        ] {
            let hook = &hooks[event][0]["hooks"][0];
            assert!(
                !hook["command"].as_str().unwrap().contains("--wait-secs"),
                "{event} should not park the agent"
            );
            assert_eq!(hook["timeout"], REPORT_TIMEOUT_SECS, "{event}");
        }
    }

    #[test]
    fn codex_parks_permission_request_like_claude() {
        let mut settings = json!({});
        install_nested_hooks(
            &mut settings,
            EXE,
            "codex",
            CODEX_HOOKS,
            |e| waits_for_a_decision("codex", e),
            seconds_timeout,
            none_extra,
        );
        let hooks = &settings["hooks"];
        for event in [
            "SessionStart",
            "PreToolUse",
            "PostToolUse",
            "SubagentStop",
            "Stop",
        ] {
            assert!(hooks.get(event).is_some(), "missing {event}");
        }
        let permission = &hooks["PermissionRequest"][0]["hooks"][0];
        assert!(permission["command"]
            .as_str()
            .unwrap()
            .contains("--wait-secs"));
        assert_eq!(permission["timeout"], APPROVAL_TIMEOUT_SECS);
        assert_eq!(
            hooks["PreToolUse"][0]["hooks"][0]["timeout"],
            REPORT_TIMEOUT_SECS
        );
    }

    #[test]
    fn qwen_parks_with_millisecond_timeouts() {
        let mut settings = json!({});
        install_nested_hooks(
            &mut settings,
            EXE,
            "qwen",
            QWEN_HOOKS,
            |e| waits_for_a_decision("qwen", e),
            |waits| {
                if waits {
                    QWEN_APPROVAL_TIMEOUT_MS
                } else {
                    QWEN_REPORT_TIMEOUT_MS
                }
            },
            none_extra,
        );
        let hooks = &settings["hooks"];
        assert_eq!(
            hooks["PermissionRequest"][0]["hooks"][0]["timeout"],
            QWEN_APPROVAL_TIMEOUT_MS
        );
        assert_eq!(
            hooks["PreToolUse"][0]["hooks"][0]["timeout"],
            QWEN_REPORT_TIMEOUT_MS
        );
    }

    #[test]
    fn gemini_hooks_carry_names_and_ms_timeouts() {
        let mut settings = json!({});
        install_nested_hooks(
            &mut settings,
            EXE,
            "gemini",
            GEMINI_HOOKS,
            |e| waits_for_a_decision("gemini", e),
            ms_timeout(GEMINI_REPORT_TIMEOUT_MS),
            |_, _| json!({ "name": "sailor-hook" }),
        );
        let hooks = &settings["hooks"];
        for event in [
            "SessionStart",
            "BeforeTool",
            "AfterTool",
            "Notification",
            "SessionEnd",
        ] {
            assert!(hooks.get(event).is_some(), "missing {event}");
        }
        let hook = &hooks["BeforeTool"][0]["hooks"][0];
        assert_eq!(hook["name"], "sailor-hook");
        assert_eq!(hook["timeout"], GEMINI_REPORT_TIMEOUT_MS);
        // Nothing decidable: no hook may wait.
        assert!(!serde_json::to_string(&settings)
            .unwrap()
            .contains("--wait-secs"));
    }

    #[test]
    fn cursor_uses_the_flat_shape() {
        let mut settings = json!({ "version": 1 });
        assert_eq!(install_cursor_hooks(&mut settings, EXE), CURSOR_HOOKS.len());
        let hooks = &settings["hooks"];
        for event in ["sessionStart", "preToolUse", "postToolUse", "stop"] {
            assert!(hooks.get(event).is_some(), "missing {event}");
        }
        // Flat: the array holds the definition directly, no nested `hooks`.
        let entry = &hooks["preToolUse"][0];
        assert!(entry["command"].as_str().is_some());
        assert!(entry.get("hooks").is_none());
        assert_eq!(entry["timeout"], REPORT_TIMEOUT_SECS);

        // Strip is flat-aware too and leaves foreign entries alone.
        settings["hooks"]["preToolUse"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "command": "/other/tool.sh" }));
        assert_eq!(strip_cursor_hooks(&mut settings), CURSOR_HOOKS.len());
        assert_eq!(settings["hooks"]["preToolUse"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn cursor_install_is_idempotent() {
        let mut settings = json!({});
        install_cursor_hooks(&mut settings, EXE);
        let once = settings.clone();
        install_cursor_hooks(&mut settings, EXE);
        assert_eq!(settings, once);
    }

    #[test]
    fn missing_and_empty_files_read_as_empty_objects() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        assert_eq!(read_settings(&path).unwrap(), json!({}));
        std::fs::write(&path, "  \n").unwrap();
        assert_eq!(read_settings(&path).unwrap(), json!({}));
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read_settings(&path).is_err());
    }

    #[test]
    fn write_backs_up_the_previous_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"editorMode":"vim"}"#).unwrap();
        write_settings(&path, &json!({ "editorMode": "emacs" })).unwrap();
        let bak = std::fs::read_to_string(path.with_extension("json.bak")).unwrap();
        assert!(bak.contains("vim"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("emacs"));
    }

    #[test]
    fn nested_strip_handles_foreign_entries_inside_our_event_groups() {
        let mut settings = json!({});
        install_claude_hooks(&mut settings, EXE);
        // A foreign hook shares an event group with ours.
        settings["hooks"]["PreToolUse"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "matcher": "*",
                "hooks": [{ "type": "command", "command": "/other/tool.sh" }],
            }));
        strip_sailor_hooks(&mut settings);
        // Our entry is gone, the foreign one survives.
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert!(settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("/other/tool.sh"));
    }

    #[test]
    fn installed_agents_detects_every_format() {
        // Point the targets at a temp home and install into it, then check
        // detection sees exactly the installed set.
        let tmp = tempfile::tempdir().unwrap();
        // Override HOME so config::targets_for resolves inside the temp dir.
        // This test only needs the pure detection paths, which take a Target
        // directly.
        let mut t = config::Target {
            agent: "codex",
            path: tmp.path().join("hooks.json"),
        };
        let mut settings = json!({});
        install_nested_hooks(
            &mut settings,
            EXE,
            "codex",
            CODEX_HOOKS,
            |e| waits_for_a_decision("codex", e),
            seconds_timeout,
            none_extra,
        );
        std::fs::write(&t.path, serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(installed_for_agent(&t));

        t.agent = "cursor";
        t.path = tmp.path().join("cursor-hooks.json");
        let mut cursor = json!({});
        install_cursor_hooks(&mut cursor, EXE);
        std::fs::write(&t.path, serde_json::to_string(&cursor).unwrap()).unwrap();
        assert!(installed_for_agent(&t));

        t.agent = "kimi";
        t.path = tmp.path().join("config.toml");
        let mut kimi = toml::Value::Table(Default::default());
        kimi::install_hooks(&mut kimi, EXE);
        kimi::write_toml(&t.path, &kimi).unwrap();
        assert!(installed_for_agent(&t));

        t.agent = "opencode";
        t.path = tmp.path().join("sailor-hooks.ts");
        opencode::install_plugin(&t.path).unwrap();
        assert!(installed_for_agent(&t));
        assert!(opencode::uninstall_plugin(&t.path).unwrap());
        assert!(!installed_for_agent(&t));
    }
}
