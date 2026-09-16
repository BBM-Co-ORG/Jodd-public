# Building Jodd for Android locally

This is the local SDK/NDK setup for cross-compiling Jodd's Rust core (and,
later, building a full debug APK) for Android on macOS. Every command below
was run and verified on this machine on 2026-09-10.

## Step 1: Confirm the SDK/NDK is installed

```bash
ls /opt/homebrew/share/android-commandlinetools/ndk
```

Expected: `27.3.13750724` — the same version `.github/workflows/release.yml`
pins. If it is missing, install it:

```bash
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools
yes | sdkmanager --sdk_root=$ANDROID_HOME --licenses
sdkmanager --sdk_root=$ANDROID_HOME "platform-tools" "platforms;android-36" "build-tools;36.0.0" "ndk;27.3.13750724"
```

## Step 2: The env block

`tauri android build` and a plain `cargo check --target aarch64-linux-android`
both need the NDK's cross toolchain named explicitly:

```bash
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools
export NDK=$ANDROID_HOME/ndk/27.3.13750724/toolchains/llvm/prebuilt/darwin-x86_64/bin
export CC_aarch64_linux_android=$NDK/aarch64-linux-android24-clang
export CXX_aarch64_linux_android=$NDK/aarch64-linux-android24-clang++
export AR_aarch64_linux_android=$NDK/llvm-ar
export RANLIB_aarch64_linux_android=$NDK/llvm-ranlib
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$NDK/aarch64-linux-android24-clang
```

If the `darwin-x86_64` path does not exist, list
`$ANDROID_HOME/ndk/27.3.13750724/toolchains/llvm/prebuilt/` and use what is
there — the NDK ships one prebuilt directory whose name is the host, and on
Apple silicon it is still `darwin-x86_64` (Rosetta-free universal binaries
under an x86-named folder). The `24` in the clang wrapper names is `minSdk`
from `src-tauri/gen/android/app/build.gradle.kts`; it is not the NDK version
and must not be changed to match one.

**Why the env block is this long.** `tauri android build` invokes cargo
itself and never sets `AR`/`RANLIB`, so `openssl-src` (pulled in by
SQLCipher) falls back to whatever `ranlib` is on `PATH` — not the cross one —
and dies with `aarch64-linux-android-ranlib: not found`.
`.github/workflows/release.yml` sets the same four variables per ABI for the
same reason. Desktop builds never hit it because the host toolchain already
has a working `ranlib`.

## Step 3: Cross-compile check

```bash
cargo check -p jodd --target aarch64-linux-android
```

Expected: finishes without error (the first run compiles vendored OpenSSL
for the target and takes several minutes; a warm run is well under two
minutes).

## Step 4: Build a debug APK

The gate that shows the iCloud sign-in button on Android may still be
closed in `src/lib/stores/platform.ts` (`ICLOUD_PLATFORMS`) depending on
where the iCloud-on-Android work stands — check that file before assuming
the button will appear.

```bash
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools
export NDK_HOME=$ANDROID_HOME/ndk/27.3.13750724
export NDK=$NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin
export CC_aarch64_linux_android=$NDK/aarch64-linux-android24-clang
export CXX_aarch64_linux_android=$NDK/aarch64-linux-android24-clang++
export AR_aarch64_linux_android=$NDK/llvm-ar
export RANLIB_aarch64_linux_android=$NDK/llvm-ranlib
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$NDK/aarch64-linux-android24-clang
npm run tauri android build -- --debug --target aarch64
```

The debug APK is around 491 MB because `build.gradle.kts` keeps JNI debug
symbols, and test phones have run out of storage over this before. Strip and
repackage:

```bash
llvm-strip --strip-debug target/aarch64-linux-android/debug/libjodd_lib.so
(cd src-tauri/gen/android && ./gradlew assembleUniversalDebug \
  -x rustBuildArm64Debug -x rustBuildArmDebug -x rustBuildX86Debug -x rustBuildX86_64Debug)
```

Skipping the rust tasks is load-bearing — without `-x`, cargo rebuilds the
unstripped `.so` over the stripped one.

```bash
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
adb logcat -c && adb logcat | grep -i jodd
```
