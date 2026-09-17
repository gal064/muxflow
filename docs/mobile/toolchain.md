# Muxflow Mobile — Android toolchain

Everything below lives under `$HOME`. Nothing was installed with `sudo`, no
system package manager was used, and no shell rc file was modified. To get the
toolchain on your `PATH`, source the script that ships with the app:

```bash
source apps/mobile/scripts/env.sh
```

## What is installed, and where

| Component | Version | Location |
| --- | --- | --- |
| JDK (Eclipse Temurin, HotSpot) | `17.0.20.1+1` | `~/.local/jdk-17` |
| Android SDK command-line tools | `19.0` (`commandlinetools-linux-13114758`) | `~/Android/Sdk/cmdline-tools/latest` |
| Android SDK Platform-Tools (`adb`) | `37.0.1` | `~/Android/Sdk/platform-tools` |
| Android SDK Platform 36 | rev 2 | `~/Android/Sdk/platforms/android-36` |
| Android SDK Platform 35 | rev 2 | `~/Android/Sdk/platforms/android-35` |
| Android SDK Build-Tools 36 | `36.0.0` | `~/Android/Sdk/build-tools/36.0.0` |
| Android SDK Build-Tools 35 | `35.0.0` | `~/Android/Sdk/build-tools/35.0.0` |
| Android NDK | `27.1.12297006` | `~/Android/Sdk/ndk/27.1.12297006` |
| CMake | `3.22.1` | `~/Android/Sdk/cmake/3.22.1` |
| Android Emulator | `37.1.11` | `~/Android/Sdk/emulator` |
| Emulator system image (x86_64, API 35) | `system-images;android-35;google_apis;x86_64` rev 9 | `~/Android/Sdk/system-images/android-35/...` |

Platform 36 is the one the Expo SDK 57 template compiles against; platform 35
and its system image are there for the emulator. The NDK and CMake are pulled
in on the first build by the React Native 0.86 Gradle plugin, which compiles
`react-native-reanimated`, `react-native-gesture-handler` and the RN core from
C++ — they are listed because they are on disk and take ~3.5 GB, not because
anything in `apps/mobile` calls them directly.

## Environment variables

`apps/mobile/scripts/env.sh` exports exactly these:

```bash
JAVA_HOME=$HOME/.local/jdk-17
ANDROID_HOME=$HOME/Android/Sdk
ANDROID_SDK_ROOT=$ANDROID_HOME     # deprecated name, still read by some tools
PATH=$JAVA_HOME/bin:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH
```

The script is idempotent: sourcing it twice does not duplicate `PATH` entries,
and it defers to a `JAVA_HOME`, `ANDROID_HOME` or `ANDROID_SDK_ROOT` you
already have set rather than pointing you at this machine's copy.

## How it was installed

The two `curl` URLs below track a moving target: Adoptium's `latest/17/ga`
endpoint serves whatever the current 17 GA build is, and Google reissues the
command-line tools zip under the same build-numbered filename. Re-running them
later can therefore give a newer revision than the table above. That is fine —
record what you actually get.

```bash
# JDK 17 — Adoptium API redirects to the current 17 GA build
mkdir -p ~/.local/jdk-17
curl -L -o /tmp/jdk17.tar.gz \
  "https://api.adoptium.net/v3/binary/latest/17/ga/linux/x64/jdk/hotspot/normal/eclipse"
tar xzf /tmp/jdk17.tar.gz -C ~/.local/jdk-17 --strip-components=1

# Android command-line tools
mkdir -p ~/Android/Sdk/cmdline-tools
curl -L -o /tmp/cmdline-tools.zip \
  "https://dl.google.com/android/repository/commandlinetools-linux-13114758_latest.zip"
unzip -q /tmp/cmdline-tools.zip -d /tmp/cmdtools
mv /tmp/cmdtools/cmdline-tools ~/Android/Sdk/cmdline-tools/latest

# Licenses, non-interactively
source apps/mobile/scripts/env.sh
yes | sdkmanager --licenses

# SDK packages
sdkmanager --install \
  "platform-tools" \
  "platforms;android-36" "platforms;android-35" \
  "build-tools;36.0.0" "build-tools;35.0.0" \
  "emulator" "system-images;android-35;google_apis;x86_64"
```

`sdkmanager --licenses` writes the accepted hashes to `~/Android/Sdk/licenses/`;
Gradle reads them from there, so no license prompt appears during a build.

A newer command-line tools package exists. Downloading
`commandlinetools-linux-16111833_latest.zip` yields revision `1.0.15985488`,
which replaces `sdkmanager` with a new `android` CLI and prints a deprecation
banner on every invocation. The `13114758` filename still serves revision
`19.0` with the classic, scriptable `sdkmanager` (verified: `sdkmanager
--version` prints `19.0`, and `cmdline-tools/latest/source.properties` reads
`Pkg.Revision=19.0`), so that is what is installed.

## Fonts

`apps/mobile/assets/fonts` needs TTF; the desktop only ships woff2
(`apps/desktop/src/assets/fonts/*.woff2`) and this machine has no woff2→ttf
converter (`fonttools`/`woff2_decompress` are not installed and installing
either would need a system package). The TTFs were therefore taken straight
from the upstream release, which is the same source the desktop's woff2 files
came from:

```bash
curl -L -o /tmp/JetBrainsMono.zip \
  "https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip"
unzip -j /tmp/JetBrainsMono.zip \
  'fonts/ttf/JetBrainsMono-Regular.ttf' 'fonts/ttf/JetBrainsMono-Bold.ttf' \
  -d apps/mobile/assets/fonts
```

JetBrains Mono 2.304, SIL Open Font License 1.1
(`apps/mobile/assets/fonts/LICENSE.txt`).

## Building

```bash
source apps/mobile/scripts/env.sh
pnpm install                      # repo root
pnpm mobile:check                 # tsc --noEmit
pnpm mobile:test                  # vitest
pnpm mobile:apk                   # expo prebuild + gradlew assembleDebug
```

`android/` is generated by `expo prebuild` on demand and is **not** committed
(`apps/mobile/.gitignore`). Delete it and re-run `pnpm mobile:apk` any time the
Expo config changes.

The first Gradle run downloads Gradle 9.3.1, the Android Gradle Plugin, and the
NDK; budget ~15 minutes and ~5 GB. Later runs are incremental.

## Emulator

An AVD is not created by any script. To make one:

```bash
source apps/mobile/scripts/env.sh
avdmanager create avd -n muxflow-api35 \
  -k "system-images;android-35;google_apis;x86_64" -d pixel_6
emulator -avd muxflow-api35 -no-window -no-audio -gpu swiftshader_indirect &
adb wait-for-device
adb install apps/mobile/android/app/build/outputs/apk/debug/app-debug.apk
```
