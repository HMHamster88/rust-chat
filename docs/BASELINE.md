# Baseline: the Kotlin client this rewrite replaces

Measured 2026-11-24 on the development machine (Windows 11, AMD64, RTX 3070, 32 GB RAM),
using the packaged portable build at
`kotlin-chat/build/dist/kotlin-chat/kotlin-chat.exe` (version 0.6.2).

## Method

1. Start `kotlin-chat.exe` (a jpackage launcher that spawns the JVM).
2. Wait 10 s for the connect screen to settle.
3. Sum `WorkingSet64`, `PrivateMemorySize64` and threads over the launcher and its child, and
   read the CPU time once per 2 s for 10 s.
4. Stop both processes.

The measurement is the **connect screen**: no server connection, no call, no share. A connected
call and a screen share will be measured once the Rust client can do them (M4/M5) and added here.

## Result

| Metric | Value |
|---|---|
| Processes | 2 (launcher + JVM) |
| Working set | 408–420 MB |
| Private bytes | 542–554 MB |
| Threads | 79 |
| Idle CPU | ~0.09 core (≈9% of one core) over the last 8 s |
| CPU time at t=10 s | ~11.2 s (includes JVM startup) |

The Gradle daemon on the same machine was 573 MB working set / 647 MB private and is **not**
part of these numbers.

## What this means

- The tail is not in the protocol or the media engine: a window showing a connect form already
  costs ~410 MB and keeps a core busy at ~9%.
- The rewrite targets, measured the same way on the same machine:

| Target | Goal |
|---|---|
| Server idle | ≤ 30 MB RSS |
| Desktop connect screen | ≤ 180 MB RSS, <1% CPU |
| Desktop, 4-peer voice | ≤ 250 MB RSS, <8% CPU |
| Desktop, 720p10 share | <25% of one core for encode |
| Android, connected + voice | ≤ 200 MB PSS |

`docs/PLAN.md` §10 describes the scripts that will measure these on every release.

## Rust spike candidates (2026-11-24)

Measured with `scripts/measure.ps1`, same machine, window with a 5,000-item list and a multiline
input. The process tree includes `conhost.exe`; the app alone is slightly below these values.

| Candidate | Working set | Private | Idle CPU |
|---|---:|---:|---:|
| FerroUI 0.5, Direct2D backend | 133.3 MB | 108.5 MB | ~0.0% |
| FerroUI 0.5, Skia + ANGLE | 152.3 MB | 144.6 MB | ~0.1% |
| FerroUI, 50,000 items (Direct2D) | 134.5 MB | 114.4 MB | ~0.0% |

The 50k-vs-0 delta (+8 MB) shows the list virtualizes. Details in
`../spikes/ferroui/README.md`.
