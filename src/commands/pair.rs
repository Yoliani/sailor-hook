//! Pair with the sailor app using a token from Settings.
//!
//! Phase 0: validate the token shape, then store the host secret via the
//! configured backend. Full pairing (registering the host with the app's
//! push relay, exchanging a host id) lands in Phase 3.

use crate::secret;

pub async fn run(token: String, store: String) -> anyhow::Result<()> {
    if token.trim().is_empty() {
        anyhow::bail!("pairing token is empty");
    }

    let backend = match store.as_str() {
        "keychain" => secret::Backend::Keychain,
        "file" => secret::Backend::File,
        other => anyhow::bail!("unknown --store backend: {other}"),
    };

    secret::store_pairing_token(&token, backend)?;
    tracing::info!(
        "paired (store={store}). host id + relay registration: not yet implemented (Phase 3)."
    );
    Ok(())
}
