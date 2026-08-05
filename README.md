# sailor-hook

Host daemon that bridges AI coding agents to the [sailor](https://github.com/Yoliani/sailor)
mobile app.

sailor-hook runs on your dev machine. It normalizes hook events from the coding
agents you already use — Claude Code, Codex, Gemini, Cursor, OpenCode, Kimi,
Qwen — into a single event stream, serves them to the phone over a local
gateway, and lets you answer an agent's approval prompt from the app.

Status: pre-alpha.

## Install

```sh
brew install yoliani/sailor/sailor-hook
```

Or build from source (Rust 1.75+):

```sh
cargo install --path .
```

## Quick start

```sh
sailor-hook install          # wire sailor's hooks into your agent configs
sailor-hook serve            # run the daemon (Unix socket + HTTP gateway)
sailor-hook easy-pair        # pick a host address, print a QR the app scans — no SSH creds on the phone
sailor-hook status           # daemon + hook status
sailor-hook servers          # list local HTTP dev servers the app can open in its browser
```

`sailor-hook install` only writes sailor-owned entries into your agent config
files and `uninstall` removes exactly those, so your own settings are left
alone. Run `sailor-hook --help` for the full command list.

Other commands worth knowing:

- `sailor-hook pair --token <t>` / `unpair` — store or remove the pairing token
  (Keychain by default; `--store file` for headless sessions).
- `sailor-hook service install` — Linux only: run the daemon persistently as a
  systemd user unit (`uninstall` / `status` manage it). On macOS use
  `brew services start sailor-hook` or a plain `serve` in a terminal.
- `sailor-hook servers kill --pid <pid> --port <port> [--force]` — terminate a
  discovered dev server after re-validating it still matches.
- `serve` is single-instance: a second `serve` exits with a clear message
  rather than colliding on the socket.

### Environment

| Variable | Effect |
| --- | --- |
| `SAILOR_STATE_DIR` | Redirect daemon state (socket, lock, gateway log). Default `~/.sailor`. |
| `SAILOR_CONFIG_DIR` | Redirect the file-backed secret store (pairing token, gateway token). Default `~/.config/sailor`. Setting it also makes `load` skip the login keychain, so a scratch daemon starts unpaired — useful for e2e isolation. |
| `SAILOR_HOOK_SOCKET` | Override the hook Unix socket path. |
| `SAILOR_HOOK_GATEWAY_LISTEN` | Override the gateway listen address (`host:port`). Explicit `--port`/`--bind` flags win. |

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs those three gates on every pull request.

## Releasing

Releases are driven by the version field, not by every merge. Bump `version` in
`Cargo.toml` and merge to `main`: the release workflow tags `v<version>`, cuts a
GitHub release, and bumps the formula in
[`Yoliani/homebrew-sailor`](https://github.com/Yoliani/homebrew-sailor). Merges
that don't change the version are a no-op.

## License

MIT — see [`LICENSE`](./LICENSE).
