#![cfg(feature = "android-lib")]
#[allow(unused_imports)]
use std::sync::Arc;
#[allow(unused_imports)]
use tokio::runtime::Runtime;
#[allow(unused_imports)]
use tokio::time::{sleep, Duration};
#[allow(unused_imports)]
use log::{info, error, LevelFilter};
#[allow(unused_imports)]
use env_logger;
use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};

mod platform;
mod tunnel;
use tunnel::{Tunnel, TunnelOptions, BootstrapMgr};

// Simple global runtime for demonstration
static mut RUNTIME: Option<Runtime> = None;

#[no_mangle]
pub extern "system" fn Java_com_example_titan_TunnelManager_startClient(
    mut env: JNIEnv,
    _class: JClass,
    app_dir: JString,
    uuid: JString,
    direct_url: JString,
) {
    let app_dir: String = env.get_string(&app_dir).unwrap().into();
    let uuid: String = env.get_string(&uuid).unwrap().into();
    let direct_url: String = env.get_string(&direct_url).unwrap().into();

    unsafe {
        if RUNTIME.is_none() {
            RUNTIME = Some(Runtime::new().unwrap());
        }
        
        let rt = RUNTIME.as_ref().unwrap();
        rt.spawn(async move {
            let opts = TunnelOptions {
                uuid: uuid.clone(),
                udp_timeout: 30,
                tcp_timeout: 10,
                bootstrap_mgr: None,
                direct_url: direct_url.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                vendor: "android".to_string(),
            };

            // Initialize bootstrap if needed
            let mut final_opts = opts;
            if direct_url.is_empty() {
                if let Ok(mgr) = BootstrapMgr::new(&app_dir).await {
                    final_opts.bootstrap_mgr = Some(Arc::new(mgr));
                }
            }

            if let Ok(tun) = Tunnel::new(final_opts).await {
                if let Ok(_) = tun.connect().await {
                    info!("Android: Tunnel connect success");
                    tun_serve(tun).await;
                }
            }
        });
    }
}

async fn tun_serve(tun: Arc<Tunnel>) {
    loop {
        let tun_clone = Arc::clone(&tun);
        if let Err(e) = tun_clone.serve().await {
            error!("tun serve error: {:?}", e);
        }

        if tun.is_destroyed().await {
            info!("Tunnel destroyed, exiting serve loop");
            return;
        }

        sleep(Duration::from_secs(3)).await;

        loop {
            let tun_clone = Arc::clone(&tun);
            match tun_clone.connect().await {
                Ok(_) => {
                    info!("tun connect success");
                    break;
                }
                Err(e) => {
                    error!("tun connect failed: {:?}, retrying in 10s...", e);
                    sleep(Duration::from_secs(10)).await;
                }
            }
        }
    }
}
