//! Self-hostable push delivery (`TECH.md` §2.4).
//!
//! The daemon POSTs notable events straight to a user-configured ntfy,
//! Gotify, or UnifiedPush endpoint. No Apple/Google account, no relay, no
//! sailor-operated server in the path — the same property as the direct
//! SSH/Mosh transport. Native APNs/FCM lands alongside this in Phase 5.
//!
//! Only `approval_required` and `task_complete` are pushed: those are the two
//! categories where somebody is waiting on the phone. Tool start/finish
//! events would be a notification per tool call.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::events::Category;
use crate::inbox::Row;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Ntfy,
    Gotify,
    UnifiedPush,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "ntfy" => Kind::Ntfy,
            "gotify" => Kind::Gotify,
            "unifiedpush" | "up" => Kind::UnifiedPush,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Ntfy => "ntfy",
            Kind::Gotify => "gotify",
            Kind::UnifiedPush => "unifiedpush",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub kind: Kind,
    /// ntfy: the topic URL. Gotify: the server base URL. UnifiedPush: the
    /// endpoint the distributor handed out.
    pub url: String,
    /// ntfy access token (optional) or Gotify application token (required).
    #[serde(default)]
    pub token: Option<String>,
}

pub fn config_path() -> anyhow::Result<PathBuf> {
    let config = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    Ok(config.join("sailor").join("push.json"))
}

pub fn load() -> Option<Config> {
    load_from(&config_path().ok()?)
}

pub fn load_from(path: &Path) -> Option<Config> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Written 0600: a Gotify or ntfy token is a credential.
pub fn save(config: &Config) -> anyhow::Result<PathBuf> {
    let path = config_path()?;
    save_to(&path, config)?;
    Ok(path)
}

fn save_to(path: &Path, config: &Config) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(config)?)?;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// Whether a row is worth waking somebody's phone for.
pub fn is_notable(category: Category) -> bool {
    matches!(
        category,
        Category::ApprovalRequired | Category::TaskComplete
    )
}

/// What to POST, worked out without touching the network so it can be tested.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

pub fn build_request(config: &Config, title: &str, body: &str) -> Request {
    match config.kind {
        Kind::Ntfy => {
            let mut headers = vec![
                ("Title".to_string(), header_safe(title)),
                // Approvals are what the user is waiting on; ntfy's high
                // priority is what breaks through a silenced phone.
                ("Priority".to_string(), "high".to_string()),
            ];
            if let Some(token) = &config.token {
                headers.push(("Authorization".to_string(), format!("Bearer {token}")));
            }
            Request {
                url: config.url.clone(),
                headers,
                body: body.to_string(),
            }
        }
        Kind::Gotify => {
            let url = match &config.token {
                Some(token) => {
                    format!("{}/message?token={token}", config.url.trim_end_matches('/'))
                }
                None => format!("{}/message", config.url.trim_end_matches('/')),
            };
            Request {
                url,
                headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                body: serde_json::json!({
                    "title": title,
                    "message": body,
                    "priority": 8,
                })
                .to_string(),
            }
        }
        // UnifiedPush distributors forward an opaque body verbatim; the app
        // decodes it, so send the same JSON shape the WebSocket carries.
        Kind::UnifiedPush => Request {
            url: config.url.clone(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: serde_json::json!({ "title": title, "message": body }).to_string(),
        },
    }
}

/// ntfy carries the title in an HTTP header, so a newline in an agent's
/// message would truncate or corrupt the request.
fn header_safe(s: &str) -> String {
    s.replace(['\r', '\n'], " ").trim().to_string()
}

/// The notification text for a row: the title is already one scannable line
/// (`adapters`), so the body carries the project it came from.
pub fn describe(row: &Row) -> (String, String) {
    let project = row
        .project
        .as_deref()
        .map(short_project)
        .unwrap_or_else(|| row.source.clone());
    (row.title.clone(), project)
}

/// `/Users/x/projects/foo` → `foo`. The full path is noise on a lock screen.
fn short_project(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

pub async fn deliver(config: &Config, title: &str, body: &str) -> anyhow::Result<()> {
    let req = build_request(config, title, body);
    let client = reqwest::Client::new();
    let mut builder = client.post(&req.url);
    for (name, value) in &req.headers {
        builder = builder.header(name, value);
    }
    let response = builder.body(req.body).send().await?;
    if !response.status().is_success() {
        anyhow::bail!("push endpoint returned {}", response.status());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ntfy() -> Config {
        Config {
            kind: Kind::Ntfy,
            url: "https://ntfy.sh/my-topic".into(),
            token: None,
        }
    }

    #[test]
    fn only_approvals_and_completions_push() {
        assert!(is_notable(Category::ApprovalRequired));
        assert!(is_notable(Category::TaskComplete));
        assert!(!is_notable(Category::ToolRunning));
        assert!(!is_notable(Category::ToolFinished));
        assert!(!is_notable(Category::SessionStarted));
    }

    #[test]
    fn ntfy_puts_the_title_in_a_header() {
        let req = build_request(&ntfy(), "Approve: Bash", "foo");
        assert_eq!(req.url, "https://ntfy.sh/my-topic");
        assert!(req
            .headers
            .contains(&("Title".into(), "Approve: Bash".into())));
        assert_eq!(req.body, "foo");
        // No token configured, no Authorization header.
        assert!(!req.headers.iter().any(|(n, _)| n == "Authorization"));
    }

    #[test]
    fn ntfy_title_survives_a_multiline_agent_message() {
        let req = build_request(&ntfy(), "Claude needs\r\npermission", "foo");
        let title = req
            .headers
            .iter()
            .find(|(n, _)| n == "Title")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(!title.contains('\n') && !title.contains('\r'));
    }

    #[test]
    fn ntfy_token_becomes_a_bearer_header() {
        let config = Config {
            token: Some("tk_secret".into()),
            ..ntfy()
        };
        let req = build_request(&config, "t", "b");
        assert!(req
            .headers
            .contains(&("Authorization".into(), "Bearer tk_secret".into())));
    }

    #[test]
    fn gotify_puts_the_token_in_the_query_and_json_in_the_body() {
        let config = Config {
            kind: Kind::Gotify,
            url: "https://gotify.example/".into(),
            token: Some("AppToken".into()),
        };
        let req = build_request(&config, "Approve: Bash", "foo");
        assert_eq!(req.url, "https://gotify.example/message?token=AppToken");
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["title"], "Approve: Bash");
        assert_eq!(body["message"], "foo");
    }

    #[test]
    fn project_paths_shorten_to_their_last_segment() {
        assert_eq!(short_project("/Users/x/projects/foo"), "foo");
        assert_eq!(short_project("/Users/x/projects/foo/"), "foo");
        assert_eq!(short_project("foo"), "foo");
    }

    #[test]
    fn config_round_trips_and_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("push.json");
        save_to(&path, &ntfy()).unwrap();
        let back = load_from(&path).unwrap();
        assert_eq!(back.kind, Kind::Ntfy);
        assert_eq!(back.url, "https://ntfy.sh/my-topic");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[test]
    fn a_missing_config_is_none_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_from(&tmp.path().join("absent.json")).is_none());
    }

    #[test]
    fn kind_parse_roundtrip() {
        for k in [Kind::Ntfy, Kind::Gotify, Kind::UnifiedPush] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Kind::parse("apns"), None);
    }
}
