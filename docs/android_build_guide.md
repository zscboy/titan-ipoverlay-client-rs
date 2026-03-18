# Titan IP Overlay Client - Android Compilation Guide

This guide explains how to compile the `titan-ipoverlay-client-rs` project into an Android shared library (`.so` file).

## 1. Environment Setup

### Install Rust Android Targets
Open your terminal and add the necessary targets for Android architectures:
```bash
rustup target add aarch64-linux-android      # For 64-bit ARM (most modern phones)
rustup target add armv7-linux-androideabi    # For 32-bit ARM
rustup target add i686-linux-android         # For 32-bit x86 (emulators)
rustup target add x86_64-linux-android       # For 64-bit x86 (emulators)
```

### Install Android NDK
You need the Android NDK (Native Development Kit) installed on your system.
1. Download it via Android Studio (SDK Manager -> SDK Tools -> NDK (Side by side)).
2. Set the `ANDROID_NDK_HOME` environment variable to the path where your NDK is installed.
   - Example on Windows: `C:\Users\YourName\AppData\Local\Android\Sdk\ndk\26.1.10909125`

### Install `cargo-ndk`
This tool simplifies the process of passing the NDK toolchain to `cargo`.
```bash
cargo install cargo-ndk
```

---

## 2. JNI Interfaces (src/lib.rs)

The library provides a thread-safe singleton manager for the tunnel. You should only call `startClient` once per lifecycle.

For detailed Java implementation and the bridge class, see:
[doc/jni_interface_reference.md](jni_interface_reference.md)

---

## 3. Building

### Output for Android App (JNI .so)
To build for common architectures, run these commands:

```bash
# ARM 64-bit (Physical phones)
cargo ndk -t aarch64-linux-android build --release --features android-lib

# ARM 32-bit (Older phones)
cargo ndk -t armv7-linux-androideabi build --release --features android-lib

# x86 (Emulator 32-bit)
cargo ndk -t i686-linux-android build --release --features android-lib

# x86_64 (Emulator 64-bit)
cargo ndk -t x86_64-linux-android build --release --features android-lib
```

### Output Location
The compiled `.so` files will be located in:
`target/<target-triple>/release/libtitan_ip_overlay.so`

---

## 4. Android Studio Integration

### Permissions (AndroidManifest.xml)
Add these permissions to your `AndroidManifest.xml` to allow network access:

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />

<application
    android:usesCleartextTraffic="true"
    ... >
```

### jniLibs Directory Structure
Copy the generated `.so` files into your Android project under `app/src/main/jniLibs/`:

```text
app/src/main/jniLibs/
├── arm64-v8a/libtitan_ip_overlay.so
├── armeabi-v7a/libtitan_ip_overlay.so
├── x86/libtitan_ip_overlay.so
└── x86_64/libtitan_ip_overlay.so
```

### Proguard Rules
If you enable obfuscation (minifyEnabled true), add these rules to `proguard-rules.pro`:
```proguard
-keep class com.titan.** { *; }
-keepclassmembers class com.titan.** {
    native <methods>;
}
```

---

## 5. Reference Project
A complete working example is available in the `helloword/` directory. It includes:
- Correct Gradle configuration for JNI.
- UI buttons for connecting/stopping the tunnel.
- Real-time status polling for the tunnel.
- Correct directory structure for resources and manifests.
