//! Multiplexer context detection — the same probe the sailor app runs on
//! connect, plus the one-shot `sailor-hook context` CLI.
//!
//! Reads `$TMUX_PANE`, `$ZELLIJ`, `$HERDR_ENV`, `$HERDR_SESSION` from the
//! current shell and reports the detected kind + session/pane/workspace/cwd.
//! See `TECH.md` §2.3 for the multiplexer support matrix.

use std::env;

#[derive(Debug, Clone, Default)]
pub struct Context {
    pub kind: Kind,
    pub session: Option<String>,
    pub pane: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    None,
    Tmux,
    Zellij,
    Herdr,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::None => "none",
            Kind::Tmux => "tmux",
            Kind::Zellij => "zellij",
            Kind::Herdr => "herdr",
        }
    }
}

pub fn detect() -> Context {
    let cwd = env::current_dir().ok().map(|p| p.display().to_string());

    // Herdr: $HERDR_ENV is set inside a Herdr session.
    if let Ok(herdr_env) = env::var("HERDR_ENV") {
        if !herdr_env.is_empty() {
            return Context {
                kind: Kind::Herdr,
                session: env::var("HERDR_SESSION").ok(),
                pane: None,
                workspace: env::var("HERDR_WORKSPACE").ok(),
                cwd,
            };
        }
    }

    // Zellij: $ZELLIJ is set (e.g. "0"); $ZELLIJ_SESSION_NAME / $ZELLIJ_PANE_ID.
    if let Ok(zellij) = env::var("ZELLIJ") {
        if !zellij.is_empty() {
            return Context {
                kind: Kind::Zellij,
                session: env::var("ZELLIJ_SESSION_NAME").ok(),
                pane: env::var("ZELLIJ_PANE_ID").ok(),
                workspace: None,
                cwd,
            };
        }
    }

    // tmux: $TMUX is set ("socket,pid"); $TMUX_PANE identifies the pane.
    if let Ok(tmux) = env::var("TMUX") {
        if !tmux.is_empty() {
            return Context {
                kind: Kind::Tmux,
                session: parse_tmux_session(&tmux),
                pane: env::var("TMUX_PANE").ok(),
                workspace: None,
                cwd,
            };
        }
    }

    Context {
        kind: Kind::None,
        session: None,
        pane: None,
        workspace: None,
        cwd,
    }
}

/// `$TMUX` is `<socket>,<pid>,<session_id>`; the session id is the third field.
fn parse_tmux_session(tmux: &str) -> Option<String> {
    tmux.split(',').nth(2).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_env_parsing() {
        assert_eq!(
            parse_tmux_session("/tmp/tmux-501/default,1234,0"),
            Some("0".into())
        );
        assert_eq!(parse_tmux_session("only,one"), None);
    }

    #[test]
    fn kind_str() {
        assert_eq!(Kind::Herdr.as_str(), "herdr");
        assert_eq!(Kind::None.as_str(), "none");
    }
}
