# FerroUI M0 gate

Standalone Cargo projects (`ferroui = "=0.5.0"`), so the framework's C++/Skia build cannot affect
the product workspace.

Two variants, because FerroUI has two Windows renderers:

| Directory | Renderer | Build prerequisites |
|---|---|---|
| `spikes/ferroui` | Skia + ANGLE (default) | MSVC C++ tools, **libclang/LLVM** (bindgen for ANGLE), long first build |
| `spikes/ferroui-d2d` | Direct2D/DirectWrite/D3D11 (opt-in native backend) | MSVC only; no Skia, no ANGLE, no libclang |

The default Skia path failed on a clean machine at `mozangle`'s bindgen step:

```
Unable to find libclang: ... set the LIBCLANG_PATH environment variable
```

That is a documented FerroUI prerequisite (README, Windows requirements), not a framework bug.
`winget install LLVM.LLVM` supplies it. The Direct2D variant is the low-memory, low-build-risk
option and is what this gate should measure first.

## Run

```powershell
cargo run
# closes by itself after N ms:
$env:FERROUI_SMOKE_EXIT_MS = "20000"; cargo run
```

Measure with `..\..\scripts\measure.ps1 -Exe .\target\debug\<name>.exe`.

## Gate

| # | Criterion | Result |
|---|---|---|
| 1 | Builds and runs on Windows from crates.io, no fork/patch | Skia: needs LLVM (documented); Direct2D: pending |
| 2 | 5,000-message virtualized list scrolls at 60 fps | not built yet |
| 3 | Multiline CJK/emoji input, IME and selection | not built yet |
| 4 | Fake 720p10 texture presents at <10% CPU | not built yet |
| 5 | Android emulator **and** real TECNO LJ8, IME/back/rotation | not built yet (SDK + NDK 26.1 present) |
| 6 | Idle RSS ≤ 180 MB; compile time and binary size recorded | pending |
| 7 | No bug forces a fork or product-level workaround | pending |

A failed gate selects the fallback: Slint 1.19 (licensing decision required) or egui.
