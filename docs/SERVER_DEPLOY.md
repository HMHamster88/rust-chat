# Server deployment on Ubuntu (VPS)

This is the plan for installing and updating `rust-chat-server` on an Ubuntu VPS. It covers the
recommended flow, the changes needed in the repository, and the phases. Nothing here is
implemented yet.

## Goals

- One command to install on a fresh Ubuntu 22.04/24.04 VPS.
- A single command (or unattended timer) to update to the latest GitHub release.
- No Docker or external database required; the server stays a single static-ish binary.
- No ACME/DNS requirement: the server keeps its self-signed certificate and clients keep TOFU
  pinning. A domain is optional.
- Safe updates: checksum verification, atomic binary replacement, service restart, health check,
  and a one-step rollback.
- Preserve `server.toml`, the verifier, TLS material and client-created rooms across updates.

## Recommended UX

```bash
# Install latest, create config, service and firewall hints
curl -fsSL https://raw.githubusercontent.com/HMHamster88/rust-chat/main/scripts/install-server.sh | sudo bash

# Show status / fingerprint / config
sudo rust-chat-server --version
sudo systemctl status rust-chat-server
sudo rust-chat-server fingerprint --config /etc/rust-chat/server.toml

# Update later (checks GitHub, verifies SHA-256, restarts, health-checks)
sudo rust-chat-server update

# Only check
rust-chat-server update --check
```

An unattended path is a systemd timer that runs `rust-chat-server update --yes` daily (with a
random delay) only when `autoUpdate = true` in the config or `/etc/rust-chat/auto-update` exists.

## Filesystem layout

| Path | Purpose | Owner / mode |
|---|---|---|
| `/usr/local/bin/rust-chat-server` | current binary | root:root 0755 |
| `/usr/local/bin/rust-chat-server.previous` | rollback copy | root:root 0755 |
| `/etc/rust-chat/server.toml` | configuration | root:rust-chat 0640 |
| `/etc/rust-chat/verifier` | Argon2id password verifier | root:rust-chat 0640 |
| `/etc/rust-chat/tls/{cert,key}.pem` | self-signed TLS material | key 0600 |
| `/var/lib/rust-chat/rooms.json` | client-created rooms | rust-chat:rust-chat 0640 |
| `/var/lib/rust-chat/` | working directory, future state | rust-chat:rust-chat 0750 |
| `/etc/systemd/system/rust-chat-server.service` | unit | root:root 0644 |
| `/etc/systemd/system/rust-chat-update.{service,timer}` | optional updater | root:root 0644 |

The system user `rust-chat` is created with `--system --no-create-home --shell /usr/sbin/nologin`.

## Required repository changes

1. **`init --dir` / relative-path resolution.**
   Today `init --config` writes `config/verifier`, `config/tls/*` and (default)
   `config/rooms.json` relative to the process working directory. For a split `/etc` + `/var`
   layout, add `--dir /etc/rust-chat` (or resolve relative paths against the config file's parent)
   and default `clientRoomsFile` to `/var/lib/rust-chat/rooms.json` when installed. Manual absolute
   paths in `server.toml` are the fallback.
2. **`rust-chat-server update`.**
   New subcommand with `--check`, `--yes`, `--version <tag>` and `--rollback`:
   - queries `https://api.github.com/repos/HMHamster88/rust-chat/releases/latest`;
   - compares the local `CARGO_PKG_VERSION` numerically;
   - downloads `rust-chat-vX.Y.Z-linux-x86_64.tar.gz` and `SHA256SUMS.txt`;
   - verifies SHA-256, extracts to a temp dir next to the binary, `fsync` + atomic rename;
   - keeps the previous binary for `--rollback`;
   - restarts the unit via `systemctl restart rust-chat-server` (only if running as root and the
     unit exists) and waits for `/healthz` (TLS, `curl -k`);
   - refuses to run inside the service process (safety), prints clear non-root instructions.
   The HTTP client can reuse the `ureq` dependency already used by the desktop update check; put
   the version comparison in `client-core`? No — the server should stay independent. Add a small
   `update.rs` to the server with `ureq` + `sha2` (already present via crypto).
3. **`scripts/install-server.sh`.**
   - detect `x86_64`/`aarch64` (see ARM below);
   - fetch the release tarball and checksums (override with `VERSION`, `URL_PREFIX`);
   - install binary + `systemctl` unit, create the user/dirs, set permissions;
   - run `init` if `/etc/rust-chat/server.toml` is missing (prompt for the password, or
     `PASSWORD` env for unattended installs — warn about shell history);
   - enable and start the service, run the health check, print the fingerprint and firewall
     hints;
   - idempotent: re-running upgrades the binary and keeps the config.
4. **`packaging/rust-chat-server.service`** (checked into the repo, installed by the script).
5. **Release workflow:** add `linux-arm64` (optional decision) and attach a `.deb` later; the
   installer uses the tarball. Keep `SHA256SUMS.txt` next to the tarball in the release.

## systemd unit (draft)

```ini
[Unit]
Description=rust-chat server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=rust-chat
Group=rust-chat
WorkingDirectory=/var/lib/rust-chat
ExecStart=/usr/local/bin/rust-chat-server serve --config /etc/rust-chat/server.toml
Restart=on-failure
RestartSec=2
Environment=RUST_LOG=info
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/rust-chat
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

The server does not need `CAP_NET_BIND_SERVICE` (8444 > 1024). `ProtectSystem=strict` plus
`ReadWritePaths` keeps `/etc` read-only to the process.

## Update flow

1. `update --check` prints `current` and `latest` (or "up to date") and the release URL.
2. `update` downloads the tarball and `SHA256SUMS.txt` into a root-only temp directory, verifies
   the hash, extracts only `rust-chat-server`, and checks `--version`.
3. Copies the current binary to `rust-chat-server.previous`, atomically renames the new binary into
   place, and restarts the unit.
4. Polls `https://127.0.0.1:8444/healthz` (with `-k`, since the certificate is self-signed) for up
   to 15 s. On failure, restores the previous binary and restarts, then exits non-zero.
5. `--rollback` swaps the previous binary back and restarts.

Unattended updates use a timer:

```ini
[Timer]
OnCalendar=daily
RandomizedDelaySec=30m
Persistent=true
```

Systemd timer updates are opt-in; the default install does **not** enable automatic updates.

## APT repository (phase 2)

Once the packaging is stable, the nicest update path on Ubuntu is a signed APT repository:

- Build `.deb` packages (amd64, arm64) with `cargo-deb` or a `dpkg-deb` script; the package
  creates the user, installs the unit, and preserves `/etc` on upgrade.
- Publish a signed `Packages`/`Release` repository to GitHub Pages (or a small VPS/object store)
  with a dedicated GPG key; document the `sources.list.d` entry and keyring.
- Updates then come from `apt upgrade` and `unattended-upgrades`, and `apt` handles rollback to a
  previous version (`apt install rust-chat-server=<version>`).

This is more setup (signing key, package scripts, repo publish workflow) but removes all custom
update code from the path. Recommendation: ship the installer first, then add the APT repo once the
config layout and release matrix are settled.

## Docker (optional)

A `ghcr.io/hmhamster88/rust-chat-server` image with `/etc/rust-chat` and `/var/lib/rust-chat`
volumes is useful for compose users; it is not the recommended VPS path because TURN/UDP and
`systemd`/`ufw` integration are simpler with the binary.

## Network and firewall

- `8444/tcp` — WSS control plane (required).
- `coturn`, if used: `3478/tcp+udp`, `5349/tcp+udp`, and `49152–65535/udp` (or a narrower relay
  range) — configure `turn` in `server.toml` so `welcome` carries credentials.
- SSH only from trusted addresses where possible.
- No domain or inbound HTTP/ACME is required by the server itself.

`ufw` example:

```bash
sudo ufw allow 8444/tcp
sudo ufw allow 3478,5349/tcp
sudo ufw allow 3478,5349/udp
sudo ufw allow 49152:65535/udp
sudo ufw enable
```

## Security

- Unprivileged service user; hardened unit; `/etc` read-only at runtime.
- Password is never stored in the config; only the Argon2id verifier is read.
- Updates verify the release SHA-256; TLS and TOFU pinning are unchanged.
- Later: sign releases with `cosign`/minisign and verify signatures in the updater.
- Backups: copy `/etc/rust-chat` and `/var/lib/rust-chat/rooms.json`; there is no server-side chat
  history to back up.

## Phases and acceptance

| Phase | Work | Acceptance |
|---|---|---|
| 1 | `init --dir`/absolute defaults, `update` subcommand, installer script, unit, docs | Fresh Ubuntu 24.04 VM: one-liner installs, service survives reboot, client connects, `update` on a test tag replaces the binary and passes health; rollback works |
| 2 | APT repository, `.deb`, optional `unattended-upgrades` | `apt install rust-chat-server`, `apt upgrade` to a new version, config preserved |
| 3 | ARM64 artifact, release signatures, optional Docker image | arm64 VPS installs; signature verification test |

## Open questions

1. **ARM64 VPS?** Add an arm64 release build now, or x86_64 only for the first installer?
2. **Auto-update default?** Keep it off (recommended) with an opt-in timer, or on for the server?
3. **APT repo vs installer-first?** The plan recommends installer first, APT in phase 2.
4. **Domain/reverse proxy?** Not required with TOFU pinning; the client connects by IP/hostname and
   pins the certificate. A proxy would break the pinned endpoint unless it passes the TLS through.
5. **TURN on the same VPS?** If yes, document coturn and the relay range; if elsewhere, only
   `turn` credentials in `server.toml`.
