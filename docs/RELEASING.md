# Releasing

The release workflow (`.github/workflows/release.yml`) builds tagged releases and attaches
artifacts to a GitHub Release. The in-app updater reads
`https://api.github.com/repos/HMHamster88/rust-chat/releases/latest`.

## Checklist

1. Bump the version in the workspace `Cargo.toml` (`[workspace.package] version`). All crates
   inherit it.
2. Add the release notes to `CHANGELOG.md`.
3. `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace`, and `cargo build --release -p rust-chat-desktop` (the server is a
   development tool; build it separately with `-p rust-chat-server` when needed).
4. Push `main` and wait for the **Windows build** workflow (it uploads the release client as an
   artifact). The release reuses that artifact when the tag points at the same commit, and builds
   only as a fallback.
5. Commit, then tag and push:
   ```powershell
   git tag v0.1.0
   git push origin main
   git push origin v0.1.0
   ```
6. The workflow produces (asset names state which side they are):
   - `rust-chat-desktop-vX.Y.Z-windows-x64.zip` — `rust-chat.exe`, README and `SHA256SUMS.txt`
   - `rust-chat-server-vX.Y.Z-linux-x86_64.tar.gz` — `rust-chat-server`, README and `SHA256SUMS.txt`
7. Verify the Release page has both assets, that the release body lists them, and that
   `rust-chat` reports “up to date” (or the new version) from the About row. For the server,
   `sudo rust-chat-server update --check` must report the same tag.

## Notes

- The desktop app is Windows-only; the server is cross-platform.
- Artifacts are unsigned (no Authenticode certificate yet). SmartScreen may warn.
- No license has been chosen for the product crates yet; decide before distributing widely.
