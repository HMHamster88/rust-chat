#!/usr/bin/env bash
# Install or upgrade rust-chat-server on Ubuntu/Debian (x86_64).
#
#   curl -fsSL https://raw.githubusercontent.com/HMHamster88/rust-chat/main/scripts/install-server.sh | sudo bash
#
# Environment overrides:
#   RUST_CHAT_VERSION=latest|v0.1.2   RUST_CHAT_PORT=8444
#   RUST_CHAT_CONFIG_DIR=/etc/rust-chat  RUST_CHAT_DATA_DIR=/var/lib/rust-chat
#   RUST_CHAT_PASSWORD=...             RUST_CHAT_PUBLIC_IP=...
#   RUST_CHAT_TURN=ask|yes|no          RUST_CHAT_AUTO_UPDATE=ask|yes|no
#   RUST_CHAT_YES=1                    accept defaults (unattended)

set -euo pipefail

REPO="HMHamster88/rust-chat"
CONFIG_DIR="${RUST_CHAT_CONFIG_DIR:-/etc/rust-chat}"
DATA_DIR="${RUST_CHAT_DATA_DIR:-/var/lib/rust-chat}"
BIN="${RUST_CHAT_BIN:-/usr/local/bin/rust-chat-server}"
SERVICE="rust-chat-server"
VERSION="${RUST_CHAT_VERSION:-latest}"
PORT="${RUST_CHAT_PORT:-8444}"
TURN="${RUST_CHAT_TURN:-ask}"
AUTO_UPDATE="${RUST_CHAT_AUTO_UPDATE:-ask}"
TURN_RELAY_PORT="49160:49200"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

[[ ${EUID} -eq 0 ]] || die "run this script with sudo"
[[ "$(uname -m)" == "x86_64" ]] || die "only x86_64 Linux is supported by this release"
[[ -r /etc/os-release ]] || die "cannot identify the distribution"
# shellcheck disable=SC1091
. /etc/os-release
case "${ID:-}" in
    ubuntu|debian) ;;
    *) die "this installer supports Ubuntu/Debian (found ${ID:-unknown})" ;;
esac

read_tty() {
    # read_tty VAR "prompt" [silent]
    local __var=$1 prompt=$2 silent=${3:-0} value=""
    if [[ -t 0 ]]; then
        if [[ $silent == 1 ]]; then read -r -s -p "$prompt" value; else read -r -p "$prompt" value; fi
    elif [[ -r /dev/tty ]]; then
        if [[ $silent == 1 ]]; then read -r -s -p "$prompt" value </dev/tty; else read -r -p "$prompt" value </dev/tty; fi
    fi
    printf -v "$__var" '%s' "$value"
}

ask() {
    # ask "prompt" y|n  -> returns 0 for yes
    local prompt=$1 default=$2 answer=""
    if [[ "${RUST_CHAT_YES:-0}" == "1" ]]; then
        [[ "$default" == "y" ]]
        return
    fi
    read_tty answer "$prompt [$([[ "$default" == "y" ]] && echo 'Y/n' || echo 'y/N')] "
    answer=${answer:-$default}
    [[ "$answer" =~ ^[Yy] ]]
}

log "installing dependencies"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq curl ca-certificates tar gzip >/dev/null

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

if [[ "$VERSION" == "latest" ]]; then
    log "looking up the latest release"
    TAG="$(curl -fsSL -H 'User-Agent: rust-chat-installer' \
        "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
else
    TAG="v${VERSION#v}"
fi
[[ -n "${TAG:-}" ]] || die "could not determine a release tag"
ARCHIVE="rust-chat-${TAG}-linux-x86_64.tar.gz"
URL="https://github.com/$REPO/releases/download/$TAG/$ARCHIVE"

log "downloading $TAG"
curl -fL --retry 3 --retry-delay 2 -o "$TMP/$ARCHIVE" "$URL"
tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
( cd "$TMP" && grep -E '  rust-chat-server$' SHA256SUMS.txt | sha256sum -c - >/dev/null ) \
    || die "release checksum verification failed"

if [[ ! -f "$CONFIG_DIR/server.toml" ]]; then
    NEW_INSTALL=1
    if [[ -n "${RUST_CHAT_PASSWORD:-}" ]]; then
        PASSWORD="$RUST_CHAT_PASSWORD"
    else
        read_tty PASSWORD "Choose a server password: " 1
        echo
    fi
    [[ -n "$PASSWORD" ]] || die "the server password must not be empty"
else
    NEW_INSTALL=0
fi

log "creating the service user and directories"
getent group rust-chat >/dev/null || groupadd --system rust-chat
id -u rust-chat >/dev/null 2>&1 || \
    useradd --system --gid rust-chat --home-dir "$DATA_DIR" --shell /usr/sbin/nologin rust-chat
install -d -m 0750 -o root -g rust-chat "$CONFIG_DIR" "$CONFIG_DIR/tls"
install -d -m 0750 -o rust-chat -g rust-chat "$DATA_DIR"

if systemctl is-active --quiet "$SERVICE" 2>/dev/null; then
    log "stopping the running service"
    systemctl stop "$SERVICE"
fi

log "installing the binary"
install -m 0755 "$TMP/rust-chat-server" "$BIN"

if [[ "$NEW_INSTALL" == "1" ]]; then
    log "writing the first configuration"
    PWFILE="$(mktemp)"
    chmod 600 "$PWFILE"
    printf '%s\n' "$PASSWORD" >"$PWFILE"
    "$BIN" init --config "$CONFIG_DIR/server.toml" --dir "$CONFIG_DIR" \
        --data-dir "$DATA_DIR" --password-file "$PWFILE"
    rm -f "$PWFILE"
    unset PASSWORD
else
    log "keeping the existing configuration"
fi

if [[ "$PORT" != "8444" ]]; then
    sed -i -E "s/^port[[:space:]]*=.*/port = ${PORT}/" "$CONFIG_DIR/server.toml"
fi

chown root:rust-chat "$CONFIG_DIR/server.toml" "$CONFIG_DIR/verifier" 2>/dev/null || true
chmod 0640 "$CONFIG_DIR/server.toml" "$CONFIG_DIR/verifier" 2>/dev/null || true
chown -R root:rust-chat "$CONFIG_DIR/tls"
chmod 0750 "$CONFIG_DIR/tls"
chmod 0640 "$CONFIG_DIR/tls/cert.pem" "$CONFIG_DIR/tls/key.pem" 2>/dev/null || true
chown -R rust-chat:rust-chat "$DATA_DIR"
chmod 0640 "$DATA_DIR/rooms.json" 2>/dev/null || true

log "writing the systemd unit"
cat >"/etc/systemd/system/${SERVICE}.service" <<EOF
[Unit]
Description=rust-chat control plane
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=rust-chat
Group=rust-chat
WorkingDirectory=${DATA_DIR}
ExecStart=${BIN} serve --config ${CONFIG_DIR}/server.toml
Restart=on-failure
RestartSec=2
Environment=RUST_LOG=info
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=${DATA_DIR}
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

# ---- coturn -----------------------------------------------------------------
want_turn=0
if [[ "$TURN" == "yes" ]]; then want_turn=1
elif [[ "$TURN" == "ask" ]]; then
    if ask "Set up a coturn TURN relay on this server?" y; then want_turn=1; fi
fi

if [[ "$want_turn" == "1" ]]; then
    if grep -q '^\[turn\]' "$CONFIG_DIR/server.toml" 2>/dev/null; then
        warn "server.toml already has a [turn] section; leaving coturn untouched"
    else
        PUBLIC_IP="${RUST_CHAT_PUBLIC_IP:-}"
        if [[ -z "$PUBLIC_IP" ]]; then
            PUBLIC_IP="$(curl -fsS --max-time 5 https://api.ipify.org || true)"
        fi
        if [[ -z "$PUBLIC_IP" ]]; then
            read_tty PUBLIC_IP "Public IP of this VPS: "
        fi
        [[ -n "$PUBLIC_IP" ]] || die "a public IP is required for TURN"
        TURN_SECRET="$(openssl rand -hex 32 2>/dev/null || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"

        log "installing coturn"
        apt-get install -y -qq coturn >/dev/null
        if [[ -f /etc/turnserver.conf ]]; then
            cp -a /etc/turnserver.conf "/etc/turnserver.conf.bak.$(date +%s)"
        fi
        cat >/etc/turnserver.conf <<EOF
# Managed by the rust-chat installer.
listening-port=3478
fingerprint
lt-cred-mech
use-auth-secret
static-auth-secret=${TURN_SECRET}
realm=rust-chat
total-quota=100
stale-nonce
no-multicast-peers
no-cli
no-tls
no-dtls
min-port=${TURN_RELAY_PORT%:*}
max-port=${TURN_RELAY_PORT#*:}
EOF
        if [[ -f /etc/default/coturn ]]; then
            sed -i 's/^#*TURNSERVER_ENABLED=.*/TURNSERVER_ENABLED=1/' /etc/default/coturn
            grep -q '^TURNSERVER_ENABLED=' /etc/default/coturn || \
                echo 'TURNSERVER_ENABLED=1' >>/etc/default/coturn
        fi
        cat >>"$CONFIG_DIR/server.toml" <<EOF

[turn]
urls = ["turn:${PUBLIC_IP}:3478?transport=udp", "turn:${PUBLIC_IP}:3478?transport=tcp"]
secret = "${TURN_SECRET}"
ttlSeconds = 3600
EOF
        chown root:rust-chat "$CONFIG_DIR/server.toml"
        chmod 0640 "$CONFIG_DIR/server.toml"
        systemctl enable coturn >/dev/null 2>&1 || true
        systemctl restart coturn
        log "coturn is configured for ${PUBLIC_IP}:3478"
    fi
fi

# ---- automatic updates ------------------------------------------------------
want_updates=0
if [[ "$AUTO_UPDATE" == "yes" ]]; then want_updates=1
elif [[ "$AUTO_UPDATE" == "ask" ]]; then
    if ask "Enable automatic daily updates?" y; then want_updates=1; fi
fi

if [[ "$want_updates" == "1" ]]; then
    cat >/etc/systemd/system/rust-chat-update.service <<EOF
[Unit]
Description=Update rust-chat-server from GitHub
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=${BIN} update --yes --config ${CONFIG_DIR}/server.toml
EOF
    cat >/etc/systemd/system/rust-chat-update.timer <<'EOF'
[Unit]
Description=Daily rust-chat-server update check

[Timer]
OnCalendar=daily
RandomizedDelaySec=1h
Persistent=true

[Install]
WantedBy=timers.target
EOF
    systemctl daemon-reload
    systemctl enable --now rust-chat-update.timer >/dev/null 2>&1 || true
    log "automatic updates are enabled (rust-chat-update.timer)"
else
    systemctl disable --now rust-chat-update.timer >/dev/null 2>&1 || true
    rm -f /etc/systemd/system/rust-chat-update.service /etc/systemd/system/rust-chat-update.timer
    log "automatic updates are disabled"
fi

log "starting the service"
systemctl daemon-reload
systemctl enable "$SERVICE" >/dev/null 2>&1 || true
systemctl restart "$SERVICE"

healthy=0
for _ in $(seq 1 30); do
    if curl -ksf --max-time 2 "https://127.0.0.1:${PORT}/healthz" >/dev/null 2>&1; then
        healthy=1
        break
    fi
    sleep 0.5
done
if [[ "$healthy" != "1" ]]; then
    warn "the health check did not pass; recent logs:"
    journalctl -u "$SERVICE" -n 20 --no-pager || true
    die "the server did not become healthy"
fi

# ---- firewall ---------------------------------------------------------------
if command -v ufw >/dev/null && ufw status 2>/dev/null | grep -q "Status: active"; then
    log "opening the required ports in ufw"
    ufw allow "${PORT}/tcp" >/dev/null
    if [[ "$want_turn" == "1" ]]; then
        ufw allow 3478/tcp >/dev/null
        ufw allow 3478/udp >/dev/null
        ufw allow "${TURN_RELAY_PORT}/udp" >/dev/null
    fi
    ufw reload >/dev/null
else
    cat <<EOF

Firewall (ufw is not active). Open these ports from your provider firewall:
  ${PORT}/tcp                 WSS
EOF
    if [[ "$want_turn" == "1" ]]; then
        cat <<EOF
  3478/tcp, 3478/udp          TURN
  ${TURN_RELAY_PORT}/udp      TURN relay range
EOF
    fi
fi

FINGERPRINT="$("$BIN" fingerprint --config "$CONFIG_DIR/server.toml")"
cat <<EOF

rust-chat-server ${TAG#v} is installed and running.

  Connect:      <this server's address>:${PORT}
  Fingerprint:  ${FINGERPRINT}
  Config:       ${CONFIG_DIR}/server.toml
  Logs:         journalctl -u ${SERVICE} -f
  Update:       sudo ${BIN} update
  Rollback:     sudo ${BIN} update --rollback
  Automatic:    $([[ "$want_updates" == "1" ]] && echo "daily timer enabled" || echo "off")

Approve the fingerprint in the client on first connect.
EOF
