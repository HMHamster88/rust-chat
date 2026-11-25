# Agent instructions

## Releases

- **Create releases only when the user explicitly asks for one.** Do not bump the version, create
  or move tags, push a `v*` tag, run the Release workflow, or publish GitHub release assets on your
  own initiative.
- Normal work ends at code, tests, docs and a commit (and a push to `main` when the task calls for
  it). If a release seems useful, ask first.
- A release means: update `CHANGELOG.md` and the workspace version, then follow
  `docs/RELEASING.md`.

## Project notes

- Rust workspace: `crates/app-desktop` is Windows-only; the server is cross-platform.
- Check changes with `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace`.
- Windows release artifacts reuse the client built by `.github/workflows/windows-build.yml` on
  `main`; the tagged release only falls back to compiling when that artifact is missing.
