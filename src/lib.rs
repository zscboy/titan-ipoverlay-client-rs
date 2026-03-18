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
use jni::sys::{jint, jstring, jboolean};

mod platform;
mod tunnel;
use tunnel::{Tunnel, TunnelOptions, BootstrapMgr};

// Simple global runtime and tunnel state for singleton management
static mut RUNTIME: Option<Runtime> = None;
static mut CURRENT_TUNNEL: Option<Arc<Tunnel>> = None;
static mut STARTING: bool = false;

#[no_mangle]
pub extern "system" fn Java_com_titan_IPService_startClient(
    mut env: JNIEnv,
    _class: JClass,
    app_dir: JString,
    uuid: JString,
) {
    let app_dir: String = env.get_string(&app_dir).unwrap().into();
    let uuid: String = env.get_string(&uuid).unwrap().into();
    let direct_url: String = "".to_string();

    unsafe {
        if STARTING {
            info!("Android: Tunnel is already starting, skipping startClient");
            return;
        }

        if RUNTIME.is_none() {
            #[cfg(target_os = "android")]
            android_logger::init_once(
                android_logger::Config::default()
                    .with_max_level(log::LevelFilter::Info)
                    .with_tag("titan_rust"),
            );

            RUNTIME = Some(Runtime::new().unwrap());
        }
        
        let rt = RUNTIME.as_ref().unwrap();

        if let Some(ref tun) = CURRENT_TUNNEL {
            if !rt.block_on(async { tun.is_destroyed().await }) {
                info!("Android: Tunnel is already running, skipping startClient");
                return;
            }
        }
        
        STARTING = true;
        
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

            match Tunnel::new(final_opts).await {
                Ok(tun) => {
                    unsafe {
                        CURRENT_TUNNEL = Some(Arc::clone(&tun));
                        STARTING = false;
                    }
                    
                    if let Ok(_) = tun.connect().await {
                        info!("Android: Tunnel connect success");
                        tun_serve(tun).await;
                    }
                }
                Err(e) => {
                    error!("Android: Tunnel init failed: {:?}", e);
                    unsafe { STARTING = false; }
                }
            }
        });
    }
}

#[no_mangle]
pub extern "system" fn Java_com_titan_IPService_isAlive(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    unsafe {
        if let Some(ref tun) = CURRENT_TUNNEL {
            if let Some(ref rt) = RUNTIME {
                return (!rt.block_on(async { tun.is_destroyed().await })) as jboolean;
            }
        }
        false as jboolean
    }
}

#[no_mangle]
pub extern "system" fn Java_com_titan_IPService_stopClient(
    _env: JNIEnv,
    _class: JClass,
) {
    unsafe {
        if let Some(ref tun) = CURRENT_TUNNEL {
            if let Some(ref rt) = RUNTIME {
                info!("Android: Stopping tunnel client");
                let _ = rt.block_on(async { tun.destroy().await });
            }
        }
        CURRENT_TUNNEL = None;
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
