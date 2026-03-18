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

## 2. Project Modifications

### Update `Cargo.toml`
To build a shared library, you must add a `[lib]` section and optionally the `jni` dependency.

1. Add `crate-type`:
```toml
[lib]
name = "titan_ip_overlay"  # This will produce libtitan_ip_overlay.so
crate-type = ["cdylib", "rlib"]

[dependencies]
# ... other dependencies ...
jni = "0.21" # Optional: only if you need to call rust from Java/Kotlin
```

### Create `src/lib.rs` (if not already present)
If your project only has `src/main.rs`, you need to expose your functionality in `src/lib.rs`.

Example of a JNI entry point to start the client:
```rust
use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jstring;

#[no_mangle]
pub extern "system" fn Java_com_example_titan_TunnelManager_startClient(
    mut env: JNIEnv,
    _class: JClass,
    uuid: JString,
) {
    let uuid: String = env.get_string(&uuid).expect("Couldn't get java string!").into();
    
    // Call your internal startup logic here
    // Note: Since main.rs uses #[tokio::main], you'll need to spawn a runtime or use a static one.
}
```

---

## 3. Building

### Case A: Build Android Shared Library (.so)
If you want to use this in an Android App (via JNI), use the `android-lib` feature:

```bash
cargo ndk -t arm64-v8a build --release --lib --features android-lib
```

### Case B: Build Android Standalone Binary (bin)
If you want to run this in Termux or as a CLI tool on Android, omit the feature:

```bash
cargo ndk -t arm64-v8a build --release --bin titan-ipoverlay-client
```

### Output Location
- **.so**: `target/aarch64-linux-android/release/libtitan_ip_overlay.so`
- **bin**: `target/aarch64-linux-android/release/titan-ipoverlay-client`

---

## 4. Integration in Android Studio
1. Copy the `.so` files into your Android project's `app/src/main/jniLibs/` directory:
   - `jniLibs/arm64-v8a/libtitan_ip_overlay.so`
   - `jniLibs/armeabi-v7a/libtitan_ip_overlay.so`
2. Load the library in Java/Kotlin:
   ```kotlin
   System.loadLibrary("titan_ip_overlay")
   ```
3. Declare the `external` function:
   ```kotlin
   external fun startClient(uuid: String)
   ```
