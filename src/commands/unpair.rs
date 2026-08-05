//! Unpair: remove the pairing token stored by `sailor-hook pair`.
//!
//! Moshi parity (`moshi-hook unpair`): the daemon holds no server
//! registration today (pairing is Phase 3), so unpairing is exactly
//! removing the stored secret — nothing else to tear down yet.

use crate::secret;

pub fn run() -> anyhow::Result<()> {
    if secret::remove_pairing_token()? {
        println!(
            "pairing removed — this host is unpaired. Re-pair with `sailor-hook pair --token <t>`."
        );
    } else {
        println!("nothing to unpair — no pairing token is stored.");
    }
    Ok(())
}
