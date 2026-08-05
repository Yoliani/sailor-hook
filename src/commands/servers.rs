//! `sailor-hook servers` — list local HTTP dev servers, and `servers kill`
//! to terminate one. moshi-hook parity (docs/api.md §5): the app opens the
//! discovered origins in an in-app browser via SSH same-port forwarding.

use crate::servers;

pub async fn run_list() -> anyhow::Result<()> {
    let found = servers::discover().await;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({ "servers": found }))?
    );
    Ok(())
}

pub async fn run_kill(pid: u32, port: u16, force: bool) -> anyhow::Result<()> {
    let outcome = servers::kill(pid, port, force).await?;
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    Ok(())
}
