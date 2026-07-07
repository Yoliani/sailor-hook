//! `sailor-hook cwd-list` — recent project directories from agent transcript
//! history on this host (TECH.md §2.3 "Recent directories"). The app's session
//! picker shows these when no live multiplexer session exists; tapping one
//! starts a fresh tmux session rooted there.
//!
//! Sources implemented: Claude Code (`~/.claude.json` project keys, recency
//! from `~/.claude/projects/<encoded>` mtimes) and Codex (session JSONL
//! `cwd` fields). Cursor/OpenCode land with their hook installers in Phase 3.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RecentCwd {
    pub path: String,
    pub source: &'static str,
    /// Unix seconds of last use, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
}

pub fn run() -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
    let mut entries = Vec::new();
    entries.extend(claude_recent_cwds(&home));
    entries.extend(codex_recent_cwds(&home));
    let merged = merge_recent(entries);
    println!("{}", serde_json::to_string_pretty(&merged)?);
    Ok(())
}

/// Dedup by path (keeping the newest `last_used`) and sort newest-first,
/// unknown-recency entries last.
pub fn merge_recent(entries: Vec<RecentCwd>) -> Vec<RecentCwd> {
    let mut by_path: HashMap<String, RecentCwd> = HashMap::new();
    for e in entries {
        by_path
            .entry(e.path.clone())
            .and_modify(|cur| {
                if e.last_used > cur.last_used {
                    *cur = e.clone();
                }
            })
            .or_insert(e);
    }
    let mut merged: Vec<RecentCwd> = by_path.into_values().collect();
    merged.sort_by(|a, b| b.last_used.cmp(&a.last_used).then(a.path.cmp(&b.path)));
    merged
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

fn claude_recent_cwds(home: &Path) -> Vec<RecentCwd> {
    let Ok(raw) = std::fs::read_to_string(home.join(".claude.json")) else {
        return Vec::new();
    };
    let projects_dir = home.join(".claude").join("projects");
    parse_claude_projects(&raw)
        .into_iter()
        .map(|path| {
            let last_used = mtime_secs(&projects_dir.join(encode_claude_project_dir(&path)));
            RecentCwd {
                path,
                source: "claude_code",
                last_used,
            }
        })
        .collect()
}

/// Project paths are the keys of the `projects` object in `~/.claude.json`.
pub fn parse_claude_projects(claude_json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(claude_json) else {
        return Vec::new();
    };
    value
        .get("projects")
        .and_then(|p| p.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Claude Code names `~/.claude/projects/` subdirectories by replacing every
/// `/` and `.` in the absolute project path with `-`.
pub fn encode_claude_project_dir(path: &str) -> String {
    path.replace(['/', '.'], "-")
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

/// Codex writes one rollout JSONL per session under
/// `~/.codex/sessions/YYYY/MM/DD/`; the session-meta line carries the cwd.
fn codex_recent_cwds(home: &Path) -> Vec<RecentCwd> {
    let sessions = home.join(".codex").join("sessions");
    let mut out = Vec::new();
    for file in jsonl_files_recursive(&sessions, 4) {
        let Some(first_line) = read_first_line(&file) else {
            continue;
        };
        if let Some(cwd) = extract_cwd(&first_line) {
            out.push(RecentCwd {
                path: cwd,
                source: "codex",
                last_used: mtime_secs(&file),
            });
        }
    }
    out
}

/// Find a `"cwd"` string anywhere in one JSON line (schema-tolerant: Codex
/// nests it differently across versions).
pub fn extract_cwd(json_line: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(json_line).ok()?;
    find_cwd(&value)
}

fn find_cwd(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(cwd) = map.get("cwd").and_then(|v| v.as_str()) {
                return Some(cwd.to_string());
            }
            map.values().find_map(find_cwd)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_cwd),
        _ => None,
    }
}

fn jsonl_files_recursive(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if max_depth == 0 {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(jsonl_files_recursive(&path, max_depth - 1));
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    out
}

fn read_first_line(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    let trimmed = line.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_project_keys() {
        let json = r#"{
            "projects": {
                "/Users/ed/work/sailor": {"lastSessionId": "x"},
                "/Users/ed/oss/happy": {}
            },
            "other": true
        }"#;
        let mut paths = parse_claude_projects(json);
        paths.sort();
        assert_eq!(paths, vec!["/Users/ed/oss/happy", "/Users/ed/work/sailor"]);
    }

    #[test]
    fn parse_claude_projects_tolerates_bad_input() {
        assert!(parse_claude_projects("not json").is_empty());
        assert!(parse_claude_projects("{}").is_empty());
    }

    #[test]
    fn encodes_claude_project_dir_names() {
        assert_eq!(
            encode_claude_project_dir("/Users/ed/work/sailor.app"),
            "-Users-ed-work-sailor-app"
        );
    }

    #[test]
    fn extracts_cwd_from_flat_and_nested_lines() {
        assert_eq!(
            extract_cwd(r#"{"cwd":"/repo/a","id":"1"}"#),
            Some("/repo/a".to_string())
        );
        assert_eq!(
            extract_cwd(r#"{"type":"session_meta","payload":{"meta":{"cwd":"/repo/b"}}}"#),
            Some("/repo/b".to_string())
        );
        assert_eq!(extract_cwd(r#"{"type":"message"}"#), None);
        assert_eq!(extract_cwd("garbage"), None);
    }

    #[test]
    fn merges_dedups_and_sorts_newest_first() {
        let merged = merge_recent(vec![
            RecentCwd {
                path: "/a".into(),
                source: "codex",
                last_used: Some(100),
            },
            RecentCwd {
                path: "/a".into(),
                source: "claude_code",
                last_used: Some(200),
            },
            RecentCwd {
                path: "/b".into(),
                source: "claude_code",
                last_used: Some(150),
            },
            RecentCwd {
                path: "/c".into(),
                source: "codex",
                last_used: None,
            },
        ]);
        let paths: Vec<&str> = merged.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["/a", "/b", "/c"]);
        assert_eq!(merged[0].source, "claude_code");
        assert_eq!(merged[0].last_used, Some(200));
    }
}
