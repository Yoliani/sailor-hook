//! Print version.

pub fn run() -> anyhow::Result<()> {
    println!("sailor-hook {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}
