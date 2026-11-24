# FerroUI Android M0 gate

Standalone Cargo project plus a minimal Gradle shell. It reuses FerroUI's own Java layer
(`org.ferroui.android.*`, copied from the `ferroui-android` crate) and packages a Rust `cdylib`.

## Layout

```
Cargo.toml                     cdylib ferroui_android_spike
src/lib.rs                     android_application! + the same list/input as the desktop gate
src/main/AndroidManifest.xml   FerroApplication + FerroMainActivity
src/main/java/org/ferroui/android/*.java   FerroUI's Java layer (MIT)
build.gradle.kts               com.android.application 8.13.2, compileSdk 35, minSdk 26
```

## Build

```powershell
$env:ANDROID_NDK_HOME = "$env:LOCALAPPDATA\Android\Sdk\ndk\26.1.10909125"
cargo ndk -t arm64-v8a -o src/main/jniLibs build --release
& "$env:ANDROID_NDK_HOME\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-strip.exe" `
    --strip-unneeded src\main\jniLibs\arm64-v8a\libferroui_android_spike.so
.\gradlew.bat assembleDebug
adb install -r build\outputs\apk\debug\ferroui-android-spike-debug.apk
adb shell am start -n org.ferroui.spike/org.ferroui.android.FerroMainActivity
```

Memory: `adb shell dumpsys meminfo org.ferroui.spike | Select-String "TOTAL PSS|TOTAL RSS"`.

## Result (2026-11-24, real TECNO device, Android 16)

| Criterion | Result |
|---|---|
| Builds with stable Rust + NDK, no framework fork | **pass**: `cargo ndk` with NDK 26.1, AGP 8.13.2 / Gradle 8.13, approximately 8 minutes for the first release build |
| Installs | **pass** (`adb install -r`) |
| Launches without crashing | **pass**: activity focused, no `FATAL EXCEPTION` / `UnsatisfiedLinkError` |
| Virtualization on the phone | **pass**: 10–16 `ListBoxItem` visuals realized for 5,000 items (`VirtualizingStackPanel`), scrolling |
| Input method | **pass for the platform path**: tapping the box shows the keyboard, `dumpsys input_method` reports `mInputShown=true` with FerroUI's `FerroInputConnection` served, and Back hides it. Committing typed text was not asserted by the script; a human check is pending |
| Rotation | **pass**: `rotation=1`, the tree re-laid out (controls need a Grid/docked layout at landscape height), no crash |
| Memory, fresh launch | **175–186 MB PSS / 209–227 MB RSS**, Graphics 53 MB, ~60 MB swapped, stable across samples |
| Release artifact | **51.4 MB APK** after `llvm-strip` on the release `.so`; the unstripped debug `.so` produced a 918 MB APK |

Notes:

- The spike's `ListBox` gets an explicit height because the test layout is a `StackPanel`. A real
  chat screen needs the list and the composer in a `Grid`/`DockPanel` so landscape and the keyboard
  do not push the composer off-screen.
- The 175–186 MB PSS is close to the 180 MB plan target and is measured with a debug Java shell.
  Release packaging and a leaner screen may bring it down; this is not a blocker, but Android will
  not be the memory showcase — desktop is.
- The `ferroui-android` Java layer already provides what the plan called the "thin Kotlin shell":
  application/activity/view, input connection, insets, storage helpers. Our own additions (SAF
  picker, MediaStore, call foreground service, installer intent) slot in beside it.
