# JNI Interface Reference

This document provides the standard Java bridge implementation for the Titan IP Overlay client.

## Java Bridge Class (IPService.java)

To interface with the Rust library, create a class named `IPService` in the `com.titan` package.

```java
package com.titan;

import android.util.Log;

public class IPService {
    static {
        try {
            // Load the native library
            System.loadLibrary("titan_ip_overlay");
        } catch (UnsatisfiedLinkError e) {
            Log.e("IPService", "Native library load failed: " + e.getMessage());
        }
    }
    
    /**
     * Start the tunnel client.
     * @param appDir The directory where the application stores data.
     * @param uuid   The unique identifier for this client.
     */
    public native void startClient(String appDir, String uuid);

    /**
     * Check if the tunnel is currently active and connected.
     * @return true if alive, false otherwise.
     */
    public native boolean isAlive();

    /**
     * Stop the running tunnel client and cleanup resources.
     */
    public native void stopClient();

    // --- Singleton Implementation ---
    private static IPService instance;
    private IPService() {}
    public static synchronized IPService getInstance() {
        if (instance == null) {
            instance = new IPService();
        }
        return instance;
    }
}
```

## Key Implementation Details

1.  **Package Name**: The package MUST be `com.titan`.
2.  **Class Name**: The class MUST be `IPService`.
3.  **Singleton Pattern**: The Rust side manages a single instance of the tunnel. Use the Java singleton pattern to ensure you don't attempt to start multiple instances from the same process.
4.  **Logging**: Rust logs are forwarded to Android Logcat with the tag `titan_rust`.
5.  **Threading**: `startClient` is non-blocking on the Java side; the tunnel runs on a background Tokio runtime in Rust. 
