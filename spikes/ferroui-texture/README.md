# Direct2D live-texture gate

Standalone spike for the M3 screen-share path. It presents a generated 1280×720 BGRA test pattern
through FerroUI's `TexturePresenter` using the **native Direct3D 11 shared-frame importer** of the
Direct2D backend (`ferroui-direct2d`, feature `media-controls`).

## Run

```powershell
cargo run --release
$env:FERROUI_SMOKE_EXIT_MS = "30000"; $env:FERROUI_FPS = "10"; cargo run --release
..\..\scripts\measure.ps1 -Exe .\target\release\ferroui-texture-spike.exe -WarmupSeconds 5 -Seconds 10
powershell -ExecutionPolicy Bypass -File .\verify.ps1
```

`verify.ps1` brings the window forward, captures it twice and checks that the colour bars are
visible and that pixels change between captures.

## Result (2026-11-25, Windows 11, RTX 3070)

Release build, 1280×720 BGRA, an 8-texture ring:

| Run | Published / presented | CPU | Working set | Private |
|---|---|---:|---:|---:|
| 10 fps timer, 15 s | 137 / 136 | **3.0% of one core** (peak 5.7%) | 81 MB | 207 MB |
| 30 fps timer, 10 s | 177 / 177 in 8 s (~22 fps effective) | 5.5% (peak 12.6%) | 86 MB | 205 MB |

Direct2D counters at 10 fps:

```
surfaces=137 path_draws=276 image_draws=137 presents=138
readbacks=0 uploads=0 texture_imports=8 gpu_copies=274 errors=0 media_reads=136
```

- `texture_imports=8` — one import per ring texture, reused across frames.
- `readbacks=0` — the presented frames never come back to the CPU.
- `media_reads=136` — GPU completion callbacks, not CPU pixel reads.
- Visual check: 4 distinct bar colours; **31 of 240** sampled pixels changed between two captures
  0.9 s apart.

The 30 fps cadence is lower than the timer (~22 fps) because a frame takes roughly 2–3 ms on the
UI thread plus the compositor interval; 10 fps is the target and it keeps up (~9.1 fps effective).

## What this proves

- `ferroui_direct2d::native_device()` gives the renderer's `ID3D11Device`; a BGRA texture created
  on it, wrapped as `D3D11Texture::from_texture`, leased as a `FrameLease`, and published to a
  `FrameMailbox` is presented by `TexturePresenter` with `D3D11TextureImporter`.
- The frame metadata device (LUID) and generation match the compositor interop, which the importer
  validates.
- The `TexturePresenter` is driven with `present_latest()` after each publish; it continues from
  the update-completion callback.

## Production follow-ups

- The ring is fixed at eight textures. It is safe for this gate, but production should retire a
  texture through the lease/GPU-completion path (or a pooled allocator) instead of trusting the
  rotation.
- The pattern is CPU-generated; the real path is VP8 decode → I420 → BGRA (SIMD) → the D3D11
  texture. Decode and conversion belong on a worker; only the upload/publish is UI-thread work.
- Device-loss recovery is a generation check and device rebuild; not yet exercised.
- The CPU `WriteableBitmap` + `Image` fallback (Android and last-resort desktop) is not measured
  here; it is the M7 Android path and optionally a comparison.
