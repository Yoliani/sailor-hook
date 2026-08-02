//! Integration: the daemon driven the way its real clients drive it.
//!
//! - the app reading the inbox (`sailor-hook inbox --watch`, NDJSON over what
//!   will be an SSH exec channel) and over `GET`/`WS /events`;
//! - an agent hook parking on an approval and the phone answering it
//!   (`sailor-hook approve`), including every way that can fail.
//!
//! Everything runs against a real daemon in a sandboxed `$HOME`, because the
//! parts worth proving here — the socket protocol, the blocking hook, the
//! WebSocket upgrade — are exactly the parts unit tests can't see.

use std::process::Stdio;

use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;

const APPROVAL: &str = r#"{"session_id":"3f2504e0-4f89-11d3-9a0c-0305e82c3301","cwd":"/p/foo",
    "hook_event_name":"PermissionRequest","tool_name":"Bash",
    "tool_input":{"command":"npm test"}}"#;

struct Sandbox {
    dir: tempfile::TempDir,
    port: u16,
    _daemon: Child,
}

impl Sandbox {
    async fn start(port: u16) -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Command::new(env!("CARGO_BIN_EXE_sailor-hook"))
            .args(["serve", "--port", &port.to_string(), "--no-advertise"])
            .envs(env(dir.path()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sailor-hook serve");
        let sandbox = Sandbox {
            dir,
            port,
            _daemon: daemon,
        };
        sandbox.wait_for_gateway().await;
        sandbox
    }

    async fn wait_for_gateway(&self) {
        for _ in 0..100 {
            if self.get("/health").await.is_some() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("gateway never came up on port {}", self.port);
    }

    /// Minimal GET over a raw TCP socket — the crate ships no HTTP client and
    /// the gateway is loopback-only, so this keeps the dep list flat.
    async fn get(&self, path: &str) -> Option<String> {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", self.port))
            .await
            .ok()?;
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .ok()?;
        let mut body = String::new();
        stream.read_to_string(&mut body).await.ok()?;
        Some(body)
    }

    async fn get_json(&self, path: &str) -> serde_json::Value {
        let response = self.get(path).await.unwrap();
        let body = response.split("\r\n\r\n").nth(1).expect("response body");
        serde_json::from_str(body.trim()).expect("json body")
    }

    /// Spawn the hook shim with a payload on stdin. Returns the child so a
    /// blocking approval can be answered before it is awaited.
    fn fire_hook(&self, payload: &str, extra: &[&str]) -> Child {
        let mut args = vec!["event", "--agent", "claude_code"];
        args.extend_from_slice(extra);
        let mut child = Command::new(env!("CARGO_BIN_EXE_sailor-hook"))
            .args(&args)
            .envs(env(self.dir.path()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sailor-hook event");
        let payload = payload.to_string();
        let mut stdin = child.stdin.take().unwrap();
        tokio::spawn(async move {
            let _ = stdin.write_all(payload.as_bytes()).await;
        });
        child
    }

    /// Fire a hook that doesn't park, and wait for it.
    async fn report(&self, payload: &str) {
        self.fire_hook(payload, &[]).wait().await.unwrap();
    }

    async fn approve(&self, id: &str, flag: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_sailor-hook"))
            .args(["approve", id, flag])
            .envs(env(self.dir.path()))
            .output()
            .await
            .unwrap()
    }

    async fn pending_action_id(&self) -> String {
        let rows = self.get_json("/events").await;
        rows[0]["pendingActionId"]
            .as_str()
            .expect("row has a pending action id")
            .to_string()
    }
}

fn env(dir: &std::path::Path) -> Vec<(String, std::ffi::OsString)> {
    vec![
        ("HOME".into(), dir.into()),
        ("XDG_CONFIG_HOME".into(), dir.join(".config").into()),
        ("SAILOR_HOOK_SOCKET".into(), dir.join("hook.sock").into()),
    ]
}

/// Wait for the daemon to have folded in an event we just fired.
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

// --- the app reading the inbox -------------------------------------------

#[tokio::test]
async fn inbox_watch_streams_ndjson_rows() {
    let sandbox = Sandbox::start(24683).await;
    sandbox
        .report(r#"{"session_id":"a","hook_event_name":"SessionStart"}"#)
        .await;
    settle().await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_sailor-hook"))
        .args(["inbox", "--watch"])
        .envs(env(sandbox.dir.path()))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    // The snapshot arrives first...
    let first: serde_json::Value =
        serde_json::from_str(&next_line(&mut lines).await).expect("ndjson row");
    assert_eq!(first["title"], "Session started");

    // ...then live updates on the same stream.
    sandbox
        .report(r#"{"session_id":"b","cwd":"/p/bar","hook_event_name":"Stop"}"#)
        .await;
    let second: serde_json::Value =
        serde_json::from_str(&next_line(&mut lines).await).expect("ndjson row");
    assert_eq!(second["title"], "Task complete");
}

#[tokio::test]
async fn inbox_without_watch_prints_a_snapshot_and_exits() {
    let sandbox = Sandbox::start(24684).await;
    sandbox
        .report(r#"{"session_id":"a","hook_event_name":"SessionStart"}"#)
        .await;
    settle().await;

    let out = Command::new(env!("CARGO_BIN_EXE_sailor-hook"))
        .args(["inbox"])
        .envs(env(sandbox.dir.path()))
        .output()
        .await
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.contains("Session started"));
}

#[tokio::test]
async fn websocket_replays_the_inbox_then_streams_updates() {
    let sandbox = Sandbox::start(24681).await;
    sandbox
        .report(r#"{"session_id":"a","cwd":"/p/foo","hook_event_name":"Notification","message":"needs you"}"#)
        .await;
    settle().await;

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/events", sandbox.port))
            .await
            .expect("websocket upgrade");

    let replayed = next_row(&mut ws).await;
    assert_eq!(replayed["title"], "needs you");
    assert_eq!(replayed["category"], "approval_required");

    sandbox
        .report(r#"{"session_id":"b","cwd":"/p/bar","hook_event_name":"Stop"}"#)
        .await;
    let streamed = next_row(&mut ws).await;
    assert_eq!(streamed["title"], "Task complete");
}

#[tokio::test]
async fn get_events_returns_rows_with_approvals_pinned() {
    let sandbox = Sandbox::start(24682).await;
    sandbox
        .report(r#"{"session_id":"a","hook_event_name":"Notification","message":"needs you"}"#)
        .await;
    sandbox
        .report(r#"{"session_id":"b","hook_event_name":"PreToolUse","tool_name":"Read"}"#)
        .await;
    settle().await;

    let rows = sandbox.get_json("/events").await;
    assert_eq!(rows[0]["title"], "needs you");
    assert_eq!(rows[1]["title"], "Running Read");
    assert!(rows[0]["hostId"].as_str().is_some_and(|s| !s.is_empty()));
}

// --- approving from the phone --------------------------------------------

#[tokio::test]
async fn approving_unblocks_the_agent_with_an_allow_decision() {
    let sandbox = Sandbox::start(24685).await;
    let mut hook = sandbox.fire_hook(APPROVAL, &["--wait-secs", "30"]);
    settle().await;

    let id = sandbox.pending_action_id().await;
    let out = sandbox.approve(&id, "--allow").await;
    assert!(out.status.success(), "approve should succeed");

    let decision = hook_decision(&mut hook)
        .await
        .expect("hook printed a decision");
    let inner = &decision["hookSpecificOutput"];
    assert_eq!(inner["hookEventName"], "PermissionRequest");
    assert_eq!(inner["decision"]["behavior"], "allow");
    // The tool input comes back unchanged — sailor answers, it doesn't rewrite.
    assert_eq!(inner["decision"]["updatedInput"]["command"], "npm test");

    // And the row stops offering a decision nobody can make twice.
    let rows = sandbox.get_json("/events").await;
    assert_eq!(rows[0]["resolution"], "allowed");
}

#[tokio::test]
async fn denying_produces_a_deny_decision() {
    let sandbox = Sandbox::start(24686).await;
    let mut hook = sandbox.fire_hook(APPROVAL, &["--wait-secs", "30"]);
    settle().await;

    let id = sandbox.pending_action_id().await;
    assert!(sandbox.approve(&id, "--deny").await.status.success());

    let decision = hook_decision(&mut hook)
        .await
        .expect("hook printed a decision");
    assert_eq!(
        decision["hookSpecificOutput"]["decision"]["behavior"],
        "deny"
    );
    assert_eq!(sandbox.get_json("/events").await[0]["resolution"], "denied");
}

/// The safety property: an approval nobody answers must fall through to the
/// agent's own prompt, never to "allow".
#[tokio::test]
async fn an_unanswered_approval_prints_nothing_and_exits_clean() {
    let sandbox = Sandbox::start(24687).await;
    let mut hook = sandbox.fire_hook(APPROVAL, &["--wait-secs", "1"]);

    let status = hook.wait().await.unwrap();
    assert!(status.success(), "the hook must not fail the agent");
    let mut stdout = String::new();
    hook.stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .await
        .unwrap();
    assert!(
        stdout.trim().is_empty(),
        "a timeout must not decide anything, got: {stdout}"
    );
}

/// Same property with no daemon at all — the agent still gets asked normally.
#[tokio::test]
async fn an_approval_with_no_daemon_prints_nothing_and_exits_clean() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sailor-hook"))
        .args(["event", "--agent", "claude_code", "--wait-secs", "30"])
        .envs(env(dir.path()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(APPROVAL.as_bytes())
        .await
        .unwrap();

    // It must not sit out the full wait when there is nobody to wait for.
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .expect("hook should give up immediately without a daemon")
        .unwrap();
    assert!(status.success());
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .await
        .unwrap();
    assert!(stdout.trim().is_empty());
}

/// A `Notification` looks like an approval but cannot be answered, so it must
/// not park the agent.
#[tokio::test]
async fn a_notification_does_not_park_the_agent() {
    let sandbox = Sandbox::start(24688).await;
    let mut hook = sandbox.fire_hook(
        r#"{"session_id":"a","hook_event_name":"Notification","message":"needs you"}"#,
        &["--wait-secs", "30"],
    );
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), hook.wait())
        .await
        .expect("Notification must return immediately")
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn answering_an_unknown_or_stale_approval_fails_loudly() {
    let sandbox = Sandbox::start(24689).await;

    // Never existed.
    let out = sandbox
        .approve("00000000-0000-0000-0000-000000000000", "--allow")
        .await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no approval is waiting"));

    // Not even a uuid.
    let out = sandbox.approve("not-a-uuid", "--allow").await;
    assert!(!out.status.success());

    // Already answered.
    let mut hook = sandbox.fire_hook(APPROVAL, &["--wait-secs", "30"]);
    settle().await;
    let id = sandbox.pending_action_id().await;
    assert!(sandbox.approve(&id, "--allow").await.status.success());
    hook.wait().await.unwrap();
    assert!(!sandbox.approve(&id, "--allow").await.status.success());
}

// --- helpers --------------------------------------------------------------

async fn hook_decision(child: &mut Child) -> Option<serde_json::Value> {
    let stdout = child.stdout.take().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .expect("hook should exit once answered")
        .unwrap();
    let mut text = String::new();
    BufReader::new(stdout)
        .read_to_string(&mut text)
        .await
        .ok()?;
    serde_json::from_str(text.trim()).ok()
}

async fn next_line<R: tokio::io::AsyncRead + Unpin>(
    lines: &mut tokio::io::Lines<BufReader<R>>,
) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
        .await
        .expect("timed out waiting for a line")
        .unwrap()
        .expect("stream ended")
}

async fn next_row(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> serde_json::Value {
    let msg = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
        .await
        .expect("timed out waiting for a row")
        .expect("stream ended")
        .expect("websocket error");
    match msg {
        Message::Text(t) => serde_json::from_str(&t).expect("row json"),
        other => panic!("expected a text frame, got {other:?}"),
    }
}
