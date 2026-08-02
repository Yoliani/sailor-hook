//! `sailor-hook push` — configure and test self-hostable push delivery.
//!
//! Bare `push` prints the current setup; `--set` writes it; `--test` sends a
//! notification through it so the endpoint is proven before an approval
//! depends on it.

use crate::push::{self, Config, Kind};

pub async fn run(
    set: Option<String>,
    kind: String,
    token: Option<String>,
    test: bool,
) -> anyhow::Result<()> {
    if let Some(url) = set {
        let kind = Kind::parse(&kind).ok_or_else(|| {
            anyhow::anyhow!("unknown push kind `{kind}` (ntfy|gotify|unifiedpush)")
        })?;
        if kind == Kind::Gotify && token.is_none() {
            anyhow::bail!("gotify needs an application token — pass --token");
        }
        let path = push::save(&Config { kind, url, token })?;
        println!("push: saved {} config to {}", kind.as_str(), path.display());
    }

    let Some(config) = push::load() else {
        println!("push: not configured");
        println!("  sailor-hook push --set https://ntfy.sh/<your-topic>");
        return Ok(());
    };

    println!("push: {} → {}", config.kind.as_str(), config.url);
    println!(
        "  token:    {}",
        if config.token.is_some() {
            "set"
        } else {
            "(none)"
        }
    );

    if test {
        push::deliver(
            &config,
            "sailor: test notification",
            "If this reached your phone, push is working.",
        )
        .await?;
        println!("  test:     delivered");
    }
    Ok(())
}
