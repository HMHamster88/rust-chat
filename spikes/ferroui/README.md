# FerroUI M0 gate

Standalone Cargo projects (`ferroui = "=0.5.0"`), so the framework's C++/Skia build cannot affect
the product workspace.

Two variants, because FerroUI has two Windows renderers:

| Directory | Renderer | Build prerequisites |
|---|---|---|
| `spikes/ferroui` | Skia + ANGLE (default) | MSVC C++ tools, **libclang/LLVM** (bindgen for ANGLE), long first build |
| `spikes/ferroui-d2d` | Direct2D/DirectWrite/D3D11 (opt-in native backend) | MSVC only; no Skia, no ANGLE, no libclang |

The default Skia path first failed on a clean machine at `mozangle`'s bindgen step:

```
Unable to find libclang: ... set the LIBCLANG_PATH environment variable
```

That is a documented FerroUI prerequisite (README, Windows requirements), not a framework bug.
`winget install LLVM.LLVM` (23.1.6) supplies it. The Direct2D variant avoids it entirely.

## Run

```powershell
cargo run
# closes by itself after N ms:
$env:FERROUI_SMOKE_EXIT_MS = "20000"; cargo run
# message count for the virtualization test:
$env:FERROUI_MESSAGES = "50000"; cargo run
```

Measure with `..\..\scripts\measure.ps1 -Exe .\target\debug\<name>.exe`.

## Measurements (2026-11-24, Windows 11, RTX 3070, 32 GB)

Window: 900×700, one 5,000-item `ListBox`, one multiline `TextBox`, one button, one status line.
The measured process tree also contains `conhost.exe` (the binary is a console program); the app
itself is therefore a little below these numbers. Idle CPU was ~0.0–0.1% of one core in every run.

| Variant | Messages | Working set | Private | Threads |
|---|---:|---:|---:|---:|
| Direct2D | 0 | 126.1 MB | 105.0 MB | 58 |
| Direct2D | 5,000 | 133.3 MB | 108.5 MB | 58 |
| Direct2D | 50,000 | 134.5 MB | 114.4 MB | 58 |
| Skia + ANGLE | 0 | 147.0 MB | 142.4 MB | 61 |
| Skia + ANGLE | 5,000 | 152.3 MB | 144.6 MB | 61 |
| Skia + ANGLE | 50,000 | 154.6 MB | 150.6 MB | 61 |

50,000 items cost only +8 MB over an empty list, which is the data alone: the `ListBox` is
virtualizing. Debug binaries: 84.6 MB (Direct2D) and 105.8 MB (Skia). The Skia first build after
its dependencies was 4m58s; the Direct2D stack is several minutes faster and has no C++ sources.

For comparison, the Kotlin client on its connect screen is 408–420 MB working set / 542–554 MB
private (`../../docs/BASELINE.md`).

## Gate

| # | Criterion | Result |
|---|---|---|
| 1 | Builds and runs on Windows from crates.io, no fork/patch | **pass** (Skia needs the documented LLVM/libclang prerequisite; Direct2D needs only MSVC) |
| 2 | 5,000-message virtualized list scrolls at 60 fps | **virtualization confirmed** by the 50k-vs-0 memory test; 60 fps needs a manual scroll check |
| 3 | Multiline CJK/emoji input, IME and selection | multiline + the sample string render; real IME/selection check is manual |
| 4 | Fake 720p10 texture presents at <10% CPU | not built yet (FerroUI has composited texture presenters; needs the GPU interop spike) |
| 5 | Android emulator **and** real TECNO LJ8, IME/back/rotation | not built yet (SDK + NDK 26.1 are installed) |
| 6 | Idle RSS ≤ 180 MB; compile time and binary size recorded | **pass**: 133–153 MB with the list; sizes above |
| 7 | No bug forces a fork or product-level workaround | **pass so far** (no fork; the LLVM prerequisite is documented) |

Verdict so far: the memory target passes with a comfortable margin, and the Direct2D backend is
both lighter and much cheaper to build. The remaining risk is Android (gate 5) and the video
texture path (gate 4). If those fail, the fallback is Slint 1.19 (licensing decision required) or
egui.
