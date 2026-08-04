//! One-shot multiplexer listing for the app's session browser.
//!
//! An Easy Pair session is plain mosh — one channel, no SSH side channel — so
//! the browser has to type its probe into the user's visible shell. Doing that
//! as raw shell means ~300 characters of `command -v …; tmux list-sessions …`
//! plus a screenful of output, once per multiplexer, echoed into the terminal
//! the user is actually working in. `sailor-hook mux-list` collapses all of it
//! into one short command and one line of JSON.
//!
//! This deliberately emits the **raw** output of each CLI rather than parsed
//! rows: the parsers live in the app (`app/src/lib/multiplexer.ts`, covered by
//! vitest) and stay the single source of truth. Duplicating them here would
//! just create two things to keep in sync. The only thing read on this side is
//! herdr session *names*, because the agent list has to be queried per session.

use std::io::ErrorKind;
use std::process::Command;

const MUXES: [&str; 3] = ["tmux", "zellij", "herdr"];

pub fn run() -> anyhow::Result<()> {
    // One line: over a terminal the app has to scrape this back out of the
    // screen, so wrapping it across lines would only make that harder.
    println!("{}", collect());
    Ok(())
}

/// Detect the installed multiplexers and collect their raw listings. Shared
/// by the CLI and the gateway's `/mux-list`; the gateway is the path that
/// actually works for large listings, since mosh can only carry a screenful.
pub fn collect() -> serde_json::Value {
    let mut detect = Vec::new();
    let mut lists = serde_json::Map::new();
    let mut agents = serde_json::Map::new();

    for mux in MUXES {
        let Some(output) = list_sessions(mux) else {
            detect.push(format!("{mux}=0"));
            continue;
        };
        detect.push(format!("{mux}=1"));
        if mux == "herdr" {
            for session in herdr_session_names(&output) {
                if let Some(list) = capture("herdr", &["--session", &session, "agent", "list"]) {
                    agents.insert(session, serde_json::Value::String(list));
                }
            }
        }
        lists.insert(mux.to_string(), serde_json::Value::String(output));
    }

    serde_json::json!({
        "detect": detect.join("\n"),
        "lists": lists,
        "agents": agents,
    })
}

/// The same listing command the app would have run over the shell. `None`
/// means the binary isn't installed; `Some("")` means it is but has nothing
/// to report (e.g. tmux with no server running), which is what `command -v`
/// detection reported too.
fn list_sessions(mux: &str) -> Option<String> {
    match mux {
        "tmux" => capture(
            "tmux",
            &[
                "list-sessions",
                "-F",
                "#{session_name}\t#{session_windows}\t#{session_attached}\t#{session_created}",
            ],
        ),
        "zellij" => capture("zellij", &["list-sessions", "--no-formatting"]),
        "herdr" => capture("herdr", &["session", "list", "--json"]),
        _ => None,
    }
}

/// Run a command and return its stdout. A non-zero exit still counts as a
/// result (tmux exits 1 with "no server running"); only a missing binary is
/// `None`.
fn capture(program: &str, args: &[&str]) -> Option<String> {
    match Command::new(program).args(args).output() {
        Ok(out) => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(_) => None,
    }
}

/// Session names out of `herdr session list --json` — a bare array or
/// `{sessions: [...]}`. Only the names are needed here; which sessions are
/// worth showing is the app's call.
fn herdr_session_names(output: &str) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let items = match &parsed {
        serde_json::Value::Array(items) => items.as_slice(),
        serde_json::Value::Object(map) => match map.get("sessions") {
            Some(serde_json::Value::Array(items)) => items.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    items
        .iter()
        .filter_map(|item| item.get("name")?.as_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_herdr_session_names_from_both_shapes() {
        let wrapped = r#"{"sessions":[{"name":"default"},{"name":"gami"}]}"#;
        assert_eq!(herdr_session_names(wrapped), vec!["default", "gami"]);
        let bare = r#"[{"name":"gami"}]"#;
        assert_eq!(herdr_session_names(bare), vec!["gami"]);
    }

    #[test]
    fn tolerates_output_that_is_not_json() {
        assert!(herdr_session_names("herdr: command not found").is_empty());
        assert!(herdr_session_names("").is_empty());
    }

    #[test]
    fn reports_a_missing_binary_as_unavailable() {
        assert!(capture("sailor-definitely-not-a-real-binary", &[]).is_none());
    }
}
