# Agent instructions

## Releases

- **Create releases only when the user explicitly asks for one.** Do not bump the version, create
  or move tags, push a `v*` tag, run the Release workflow, or publish GitHub release assets on your
  own initiative.
- **Push to git only when the user asked for a release.** Normal work ends at code, tests, docs and
  a **local** commit; do not `git push` to `origin`, open pull requests, or otherwise publish
  commits on your own. When the user asks for a release, push the commits, then tag and publish.
- A release means: update `CHANGELOG.md` and the workspace version, push, tag `vX.Y.Z`, then follow
  `docs/RELEASING.md`.

## Project notes

- Rust workspace: `crates/app-desktop` is Windows-only; the server is cross-platform.
- Check changes with `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace`.
- Windows release artifacts reuse the client built by `.github/workflows/windows-build.yml` on
  `main`; the tagged release only falls back to compiling when that artifact is missing.
