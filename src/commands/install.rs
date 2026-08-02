//! Write sailor-owned hook entries into supported agent config files.
//!
//! Phase 3 implements Claude Code (`~/.claude/settings.json`); the other
//! agents in `config.rs` still report what they would do. That file is
//! shared with the user's own settings and with other tools' hooks, so the
//! rules here are: never rewrite what we didn't write, mark our entries so
//! `uninstall` can find them again, and stay idempotent.

use serde_json::{json, Value};

use crate::config::{self, Agent};

/// Every sailor-written hook command ends with this shell comment, and
/// nothing else does. It is what makes install idempotent and uninstall
/// surgical — a sentinel rather than a substring of the command itself,
/// because the binary path is quoted and may live anywhere.
pub const MARKER: &str = "# sailor-hook";

/// How long the shim parks a `PermissionRequest` waiting for the phone, and
/// the hook timeout that has to outlive it. The gap matters: if Claude Code
/// killed the hook first, the shim would never get to print its fallback and
/// the agent would see a hung hook instead of a normal prompt.
const APPROVAL_WAIT_SECS: u64 = 240;
const APPROVAL_TIMEOUT_SECS: u64 = 300;
const _: () = assert!(APPROVAL_TIMEOUT_SECS > APPROVAL_WAIT_SECS);

/// Everything else is fire-and-forget, so it only needs long enough to write
/// one line to a Unix socket.
const REPORT_TIMEOUT_SECS: u64 = 10;

/// Claude Code hook events we subscribe to, with the matcher each takes.
/// Tool events match every tool (`*`); the rest carry no matcher.
/// `PermissionRequest` is the only one whose stdout the agent reads as a
/// decision, so it is the only one that waits.
const CLAUDE_HOOKS: &[(&str, &str)] = &[
    ("SessionStart", ""),
    ("PreToolUse", "*"),
    ("PostToolUse", "*"),
    ("Notification", ""),
    ("PermissionRequest", "*"),
    ("Stop", ""),
];

fn waits_for_a_decision(event: &str) -> bool {
    event == "PermissionRequest"
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
        if t.agent != Agent::ClaudeCode.id() {
            println!("{}: not yet implemented, skipped", t.agent);
            continue;
        }
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        std::fs::copy(path, path.with_extension("json.bak"))?;
    }
    let mut out = serde_json::to_string_pretty(settings)?;
    out.push('\n');
    std::fs::write(path, out)?;
    Ok(())
}

/// Merge sailor's hook entries into `settings`, replacing any it already
/// owns. Returns how many were written.
pub fn install_claude_hooks(settings: &mut Value, exe: &str) -> usize {
    strip_sailor_hooks(settings);

    let Some(root) = settings.as_object_mut() else {
        return 0;
    };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return 0;
    };

    for (event, matcher) in CLAUDE_HOOKS {
        let waits = waits_for_a_decision(event);
        let entry = json!({
            "matcher": matcher,
            "hooks": [{
                "type": "command",
                "command": command_for(exe, waits),
                "timeout": if waits { APPROVAL_TIMEOUT_SECS } else { REPORT_TIMEOUT_SECS },
            }],
        });
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
    CLAUDE_HOOKS.len()
}

/// Remove every sailor-owned hook, pruning entries and event keys that end up
/// empty. Returns how many hook commands were removed.
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
fn command_for(exe: &str, waits: bool) -> String {
    let args = if waits {
        format!("event --agent claude_code --wait-secs {APPROVAL_WAIT_SECS}")
    } else {
        "event --agent claude_code".to_string()
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
        .filter(|t| {
            read_settings(&t.path)
                .map(|s| has_sailor_hook(&s))
                .unwrap_or(false)
        })
        .map(|t| t.agent)
        .collect()
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
        let cmd = command_for(EXE, false);
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
}
