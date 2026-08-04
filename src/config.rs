//! Agent hook config locations — where `sailor-hook install` writes
//! sailor-owned entries. Mirrors Moshi's targets (see `TECH.md` §3).
//!
//! Two paths differ from the TECH.md table, per the agents' current docs:
//! OpenCode's global plugin dir (`~/.config/opencode/plugins/`) replaces the
//! per-project `.opencode/plugins/` — a user-level install must work in every
//! project, and per-project would only activate in the directory it was run
//! from. Kimi's CLI moved to `~/.kimi-code/` (the `~/.kimi/` path is the old
//! one; the current docs and the kimi-cli source both read `config.toml`
//! under `~/.kimi-code/`).

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    Codex,
    OpenCode,
    Gemini,
    Cursor,
    Kimi,
    Qwen,
    Pi,
}

impl Agent {
    pub fn all() -> &'static [Agent] {
        &[
            Agent::ClaudeCode,
            Agent::Codex,
            Agent::OpenCode,
            Agent::Gemini,
            Agent::Cursor,
            Agent::Kimi,
            Agent::Qwen,
            Agent::Pi,
        ]
    }

    pub fn id(&self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude_code",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
            Agent::Gemini => "gemini",
            Agent::Cursor => "cursor",
            Agent::Kimi => "kimi",
            Agent::Qwen => "qwen",
            Agent::Pi => "pi",
        }
    }

    pub fn parse(s: &str) -> Option<Agent> {
        Some(match s {
            "claude_code" | "claude" => Agent::ClaudeCode,
            "codex" => Agent::Codex,
            "opencode" => Agent::OpenCode,
            "gemini" => Agent::Gemini,
            "cursor" => Agent::Cursor,
            "kimi" => Agent::Kimi,
            "qwen" => Agent::Qwen,
            "pi" => Agent::Pi,
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub struct Target {
    pub agent: &'static str,
    pub path: PathBuf,
}

/// Resolve the config-file targets for a given agent filter (or all).
pub fn targets_for(filter: Option<&str>) -> anyhow::Result<Vec<Target>> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;

    let selected: Vec<Agent> = match filter {
        None => Agent::all().to_vec(),
        Some(name) => {
            let a = Agent::parse(name).ok_or_else(|| anyhow::anyhow!("unknown agent: {name}"))?;
            vec![a]
        }
    };

    let mut out = Vec::new();
    for a in selected {
        let path = match a {
            Agent::ClaudeCode => home.join(".claude").join("settings.json"),
            Agent::Codex => home.join(".codex").join("hooks.json"),
            Agent::OpenCode => home
                .join(".config")
                .join("opencode")
                .join("plugins")
                .join("sailor-hooks.ts"),
            Agent::Gemini => home.join(".gemini").join("settings.json"),
            Agent::Cursor => home.join(".cursor").join("hooks.json"),
            Agent::Kimi => home.join(".kimi-code").join("config.toml"),
            Agent::Qwen => home.join(".qwen").join("settings.json"),
            // pi has no hooks file — support is a pi extension (the same one
            // this repo carries at `.pi/extensions/sailor-hooks.ts`), copied
            // into pi's global extensions dir so every pi session reports.
            Agent::Pi => home
                .join(".pi")
                .join("agent")
                .join("extensions")
                .join("sailor-hooks.ts"),
        };
        out.push(Target {
            agent: a.id(),
            path,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for a in Agent::all() {
            assert_eq!(Agent::parse(a.id()), Some(*a));
        }
        assert_eq!(Agent::parse("bogus"), None);
    }
}
