//! Local HTTP dev-server discovery — moshi-hook's `servers` command
//! (docs/api.md §5). The app opens these origins in an in-app browser via
//! SSH same-port forwarding, so the hook's job is: find every listening
//! TCP port, probe it, keep only those that actually serve HTML, and tag
//! each with its owning process + PID for a safe kill affordance.
//!
//! Two deliberate scope cuts versus moshi: no container (`docker inspect`)
//! discovery yet, and no per-session `isCurrentContext` decoration — both
//! slot in later without changing the JSON shape.

use std::process::Command;

/// A listening TCP socket as reported by the OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub pid: u32,
    /// Bind address as printed by lsof: `127.0.0.1`, `*`, `::1`, …
    pub host: String,
    pub port: u16,
}

/// A dev server worth opening in the app.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    /// Best-effort page title; falls back to the process name.
    pub name: String,
    /// Always `127.0.0.1`: the app forwards `phone:port → host:port` over
    /// SSH, so loopback is the origin it loads, whatever the listener bound.
    pub host: String,
    pub port: u16,
    pub origin: String,
    pub process: String,
    pub pid: u32,
    /// No session lookup in this build, so nothing is ever attributed to
    /// the current context. Kept for shape parity with moshi.
    pub is_current_context: bool,
}

/// Enumerate every listening TCP socket via `lsof`. `None` when lsof is
/// missing or failed — callers degrade to an empty list rather than error.
pub fn listeners() -> Option<Vec<Listener>> {
    let out = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pn"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_lsof_output(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse `lsof -F pn` output: `p<pid>` lines set the current pid, `n<addr>`
/// lines emit a listener. Anything unparseable is skipped (lsof emits odd
/// rows for some kernel threads).
pub fn parse_lsof_output(out: &str) -> Vec<Listener> {
    let mut listeners = Vec::new();
    let mut pid: Option<u32> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.trim().parse().ok();
        } else if let Some(addr) = line.strip_prefix('n') {
            if let Some((host, port)) = parse_address(addr) {
                if let Some(pid) = pid {
                    listeners.push(Listener { pid, host, port });
                }
            }
        }
    }
    listeners
}

/// `127.0.0.1:8080` / `*:8080` / `[::1]:8080` / `::1:8080` → (host, port).
/// The last colon always separates the port; brackets are cosmetic.
fn parse_address(addr: &str) -> Option<(String, u16)> {
    let (host, port_s) = addr.rsplit_once(':')?;
    let port: u16 = port_s.parse().ok()?;
    let host = host.trim_matches(['[', ']']).to_string();
    Some((host, port))
}

/// The process name behind a pid (`ps -p N -o comm=`), best-effort.
/// Trimmed to the basename — moshi's shape shows `node`, not `/usr/bin/node`.
pub fn process_name(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then(|| basename(&name))
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Probe every listener over loopback and keep the ones that answer with
/// HTML. Only ports ≥ 1024 are probed — system services on low ports are
/// noise, not dev servers. Parallel probes, bounded per-request timeout.
pub async fn discover() -> Vec<Server> {
    let Some(listeners) = listeners() else {
        return Vec::new();
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| tracing::warn!("reqwest client: {e}"))
        .ok();
    let Some(client) = client else {
        return Vec::new();
    };

    let mut tasks = tokio::task::JoinSet::new();
    for listener in listeners {
        if listener.port < 1024 {
            continue;
        }
        tasks.spawn(probe(client.clone(), listener));
    }

    let mut servers = Vec::new();
    while let Some(res) = tasks.join_next().await {
        if let Ok(Some(server)) = res {
            servers.push(server);
        }
    }
    servers.sort_by_key(|s| s.port);
    for (i, server) in servers.iter_mut().enumerate() {
        server.id = format!("server_{}", i + 1);
    }
    servers
}

async fn probe(client: reqwest::Client, listener: Listener) -> Option<Server> {
    let origin = format!("http://127.0.0.1:{}", listener.port);
    let response = client.get(&origin).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)?
        .to_str()
        .ok()?;
    if !is_html(content_type) {
        return None;
    }
    // Cap the body read — only the head matters for a <title>.
    let body = response.text().await.ok()?;
    let body = body.chars().take(64 * 1024).collect::<String>();
    let process = process_name(listener.pid).unwrap_or_else(|| "unknown".into());
    let name = extract_title(&body).unwrap_or_else(|| process.clone());
    Some(Server {
        id: String::new(), // assigned by discover() after sorting
        name,
        host: "127.0.0.1".into(),
        port: listener.port,
        origin,
        process,
        pid: listener.pid,
        is_current_context: false,
    })
}

/// `text/html` or `application/xhtml+xml` — the only kinds a WebView opens.
fn is_html(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.starts_with("text/html") || ct.starts_with("application/xhtml+xml")
}

/// First `<title>` text, case-insensitive, tolerant of attributes on the
/// tag. Byte indices from the lowercased copy are valid on the original
/// because ASCII-lowercasing never changes length.
fn extract_title(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let gt = start + lower[start..].find('>')? + 1;
    let end = gt + lower[gt..].find("</title>")?;
    let title = body[gt..end].trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// Re-validate that `pid:port` is still a discovered HTML server, then
/// terminate it: SIGTERM, and SIGKILL after a short grace when `force`.
/// Returns the outcome and the server it verified, moshi-style.
pub async fn kill(pid: u32, port: u16, force: bool) -> anyhow::Result<serde_json::Value> {
    let servers = discover().await;
    let Some(server) = servers.into_iter().find(|s| s.pid == pid && s.port == port) else {
        anyhow::bail!(
            "pid {pid} on port {port} is not a discovered HTML server (stale?) — nothing killed"
        );
    };

    let term = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    if !term.success() {
        anyhow::bail!("could not signal pid {pid} (kill -TERM failed)");
    }
    let mut forced = false;
    if !wait_for_exit(pid, std::time::Duration::from_secs(2)).await && force {
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
        forced = true;
        wait_for_exit(pid, std::time::Duration::from_secs(2)).await;
    }
    let killed = !process_alive(pid);
    Ok(serde_json::json!({
        "killed": killed,
        "forced": forced,
        "pid": pid,
        "port": port,
        "server": server,
    }))
}

fn process_alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn wait_for_exit(pid: u32, budget: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    while std::time::Instant::now() < deadline {
        if !process_alive(pid) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    !process_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsof_output_with_pid_and_address_lines() {
        let out = "\
p27753
n127.0.0.1:5173
p312
n*:8080
n[::1]:9090
p99999
ngarbage
";
        let listeners = parse_lsof_output(out);
        assert_eq!(listeners.len(), 3);
        assert_eq!(
            listeners[0],
            Listener {
                pid: 27753,
                host: "127.0.0.1".into(),
                port: 5173
            }
        );
        assert_eq!(
            listeners[1],
            Listener {
                pid: 312,
                host: "*".into(),
                port: 8080
            }
        );
        assert_eq!(
            listeners[2],
            Listener {
                pid: 312,
                host: "::1".into(),
                port: 9090
            }
        );
    }

    #[test]
    fn tolerates_odd_lsof_rows_and_ipv6() {
        assert!(parse_lsof_output("").is_empty());
        assert!(parse_lsof_output("pabc\nn:notaport").is_empty());
        assert!(parse_lsof_output("n127.0.0.1:80").is_empty()); // no pid yet
    }

    #[test]
    fn html_filter_accepts_only_htmlish() {
        assert!(is_html("text/html; charset=utf-8"));
        assert!(is_html("application/xhtml+xml"));
        assert!(!is_html("application/json"));
        assert!(!is_html("text/plain"));
        assert!(!is_html(""));
    }

    #[test]
    fn extracts_title_with_attributes_and_missing() {
        assert_eq!(
            extract_title("<html><title>Vite + React</title></html>"),
            Some("Vite + React".into())
        );
        assert_eq!(
            extract_title(r#"<title lang="en"> My App </title>"#),
            Some("My App".into())
        );
        assert_eq!(extract_title("<html><head></head></html>"), None);
        assert_eq!(extract_title(""), None);
        assert_eq!(extract_title("<TITLE>CAPS</TITLE>"), Some("CAPS".into()));
    }

    #[test]
    fn lsof_listeners_run_or_degrade() {
        // lsof exists on macOS and most Linux boxes; if it doesn't, the
        // command must degrade to an empty parse, never panic.
        let _ = listeners();
    }
}
