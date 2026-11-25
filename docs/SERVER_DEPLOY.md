# Server deployment on Ubuntu (VPS)

Status: **phase 1 released in v0.1.3** (installer, `update`, systemd, coturn). The Ubuntu
end-to-end acceptance still needs a real VPS run.

Decisions:

- **x86_64 only** for now.
- **Automatic updates are on by default**, and the installer asks (default yes).
- **No APT repository.** The installer plus `rust-chat-server update` is the supported path.
- **coturn setup is included** and enabled by default with a prompt.
- No domain or ACME is required: the server keeps its self-signed certificate and clients pin it
  on first connect (TOFU).

## Install

On a fresh Ubuntu 22.04/24.04 (or Debian) VPS:

```bash
curl -fsSL https://raw.githubusercontent.com/HMHamster88/rust-chat/main/scripts/install-server.sh | sudo bash
```

The installer:

1. checks root, x86_64 and Ubuntu/Debian;
2. downloads the latest `rust-chat-server-vX.Y.Z-linux-x86_64.tar.gz` release, verifies it against the
   `sha256:` digest that GitHub reports for the release asset, then verifies the
   `rust-chat-server` SHA-256 from `SHA256SUMS.txt` inside it;
3. creates the `rust-chat` system user, `/etc/rust-chat` and `/var/lib/rust-chat`;
4. installs `/usr/local/bin/rust-chat-server` and runs `init --dir /etc/rust-chat
   --data-dir /var/lib/rust-chat` (asks for the password unless `RUST_CHAT_PASSWORD` is set);
5. writes and starts the hardened `rust-chat-server.service`;
6. asks whether to install coturn (default yes) and configures TURN credentials in both coturn and
   `server.toml`;
7. asks whether to enable daily automatic updates (default yes);
8. checks `/healthz` and prints the certificate fingerprint to approve in the client.

Environment overrides:

```bash
RUST_CHAT_VERSION=v0.1.2       # default: latest
RUST_CHAT_PORT=8444
RUST_CHAT_CONFIG_DIR=/etc/rust-chat
RUST_CHAT_DATA_DIR=/var/lib/rust-chat
RUST_CHAT_PASSWORD=...         # unattended install (shell history warning)
RUST_CHAT_PUBLIC_IP=...        # TURN; otherwise detected via api.ipify.org
RUST_CHAT_TURN=ask|yes|no
RUST_CHAT_AUTO_UPDATE=ask|yes|no
RUST_CHAT_YES=1                # accept defaults (unattended)
```

## Filesystem layout

| Path | Purpose | Owner / mode |
|---|---|---|
| `/usr/local/bin/rust-chat-server` | current binary | root:root 0755 |
| `/usr/local/bin/rust-chat-server.previous` | rollback copy | root:root 0755 |
| `/etc/rust-chat/server.toml` | configuration | root:rust-chat 0640 |
| `/etc/rust-chat/verifier` | Argon2id password verifier | root:rust-chat 0640 |
| `/etc/rust-chat/tls/{cert,key}.pem` | self-signed TLS material | root:rust-chat 0640 |
| `/var/lib/rust-chat/rooms.json` | client-created rooms | rust-chat:rust-chat 0640 |
| `/var/lib/rust-chat/` | working directory | rust-chat:rust-chat 0750 |
| `/etc/systemd/system/rust-chat-server.service` | unit | root 0644 |
| `/etc/systemd/system/rust-chat-update.{service,timer}` | daily update | root 0644 |

The systemd unit runs unprivileged with `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`,
`ProtectHome` and `ReadWritePaths=/var/lib/rust-chat`.

## Updating

```bash
sudo rust-chat-server update             # download, verify, install, restart, health check
rust-chat-server update --check          # current vs latest, no root needed
sudo rust-chat-server update --yes       # no confirmation prompt (used by the timer)
sudo rust-chat-server update --release v0.1.2
sudo rust-chat-server update --rollback  # swap rust-chat-server.previous back
```

The update verifies the downloaded archive against the `sha256:` digest that GitHub reports for the
release asset, checks the binary hash from `SHA256SUMS.txt` inside it, runs `--version` on the
downloaded binary, keeps the previous binary, replaces the file atomically and restarts the unit.
If the health probe on `127.0.0.1:<port>` fails, the operator can roll back; `--rollback` is also
automatic-friendly for a wrapper script.

Automatic updates use `rust-chat-update.timer`:

```ini
[Timer]
OnCalendar=daily
RandomizedDelaySec=1h
Persistent=true
```

The timer is enabled by the installer when accepted and can be disabled with
`systemctl disable --now rust-chat-update.timer`.

## coturn

When enabled, the installer:

- installs `coturn` and enables it;
- writes `/etc/turnserver.conf` with `use-auth-secret` and a random 32-byte secret, UDP/TCP on
  `3478`, relay ports `49160–49200`, no TLS/DTLS (the server's TLS is unrelated);
- adds a `[turn]` block to `server.toml` with the public IP and the same secret, so `welcome`
  hands clients TURN credentials;
- opens `3478/tcp`, `3478/udp` and `49160:49200/udp` in ufw when ufw is active.

The relay range is deliberately small for a VPS. Increase `min-port`/`max-port` in
`/etc/turnserver.conf` for larger calls and open the same range in the provider firewall.

## Firewall

- `8444/tcp` — WSS control plane (required).
- `3478/tcp` + `3478/udp` and `49160–49200/udp` — only when coturn is used.
- SSH from trusted addresses where possible.

The installer applies these rules when ufw is active; otherwise it prints the list for the
provider firewall.

## Manual install and operations

```bash
# binary only, no service
tar -xzf rust-chat-server-vX.Y.Z-linux-x86_64.tar.gz
sudo install -m 0755 rust-chat-server /usr/local/bin/
rust-chat-server init --config /etc/rust-chat/server.toml \
  --dir /etc/rust-chat --data-dir /var/lib/rust-chat
sudo systemctl restart rust-chat-server

# status and logs
systemctl status rust-chat-server
journalctl -u rust-chat-server -f

# fingerprint / password rotation
rust-chat-server fingerprint --config /etc/rust-chat/server.toml
rust-chat-server hash-password --config /etc/rust-chat/server.toml
```

Uninstall (keeps configuration unless the directories are removed):

```bash
sudo systemctl disable --now rust-chat-server rust-chat-update.timer
sudo rm /etc/systemd/system/rust-chat-server.service \
        /etc/systemd/system/rust-chat-update.service \
        /etc/systemd/system/rust-chat-update.timer
sudo systemctl daemon-reload
sudo rm -rf /usr/local/bin/rust-chat-server /usr/local/bin/rust-chat-server.previous
# optional: sudo rm -rf /etc/rust-chat /var/lib/rust-chat
```

## Security and backups

- The service runs unprivileged; `/etc` is read-only at runtime.
- Back up `/etc/rust-chat` and `/var/lib/rust-chat/rooms.json`; there is no server-side chat
  history.
- The update trusts the GitHub release over TLS and verifies the archive against GitHub's own
  `sha256:` asset digest, then the binary against `SHA256SUMS.txt`. Release signing
  (`cosign`/minisign) is a later hardening step.
- TLS and TOFU pinning are unchanged; changing the certificate requires clients to re-approve.

## Acceptance (phase 1)

Tested locally on Windows for `init --dir`, `fingerprint`, `update --check` and `/healthz`. Still
to run on a fresh Ubuntu 24.04 VM:

- one-liner install, service survives a reboot;
- client connects and pins the fingerprint;
- coturn credentials are returned and a relayed call works;
- `update` to a newer test tag replaces the binary and passes health;
- `update --rollback` restores the previous binary;
- `RUST_CHAT_YES=1` installs unattended with ufw inactive.

## Later

- ARM64 artifact (not now; x86_64 only).
- Release signatures.
- Optional Docker image; not the recommended VPS path.
