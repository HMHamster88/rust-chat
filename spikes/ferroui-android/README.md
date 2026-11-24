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
cargo ndk -t arm64-v8a -o src/main/jniLibs build
.\gradlew.bat assembleDebug
adb install -r build\outputs\apk\debug\ferroui-android-spike-debug.apk
adb shell monkey -p org.ferroui.spike -c android.intent.category.LAUNCHER 1
```

Memory on the device: `adb shell dumpsys meminfo org.ferroui.spike | Select-String TOTAL`.

## Gate

| Criterion | Result |
|---|---|
| Builds with stable Rust + NDK, no framework fork | **pass**: `cargo ndk` (NDK 26.1) + the `ferroui-android` Java layer + AGP 8.13.2 / Gradle 8.13 |
| Installs on the real device | **pass** on the TECNO (adb `install -r`, Android 16) |
| Launches without crashing | **pass**: process alive, its activity and window layers exist, no `FATAL EXCEPTION`/`UnsatisfiedLinkError` |
| IME, back button, rotation | blocked: the phone is on a secure keyguard, so the app window is behind the lock screen; needs the owner to unlock |
| 5,000-item list virtualization on the phone | not measured (same code as the desktop list, which virtualizes) |
| Memory on the phone | debug build: **164 MB PSS / 194 MB RSS** (much of it swapped while the app is behind the keyguard); release build pending |

The first APK is **918 MB** because it packages an unstripped debug Rust `cdylib`; the release
build (in progress) is the realistic size. This is a build-system artifact, not a framework cost.

