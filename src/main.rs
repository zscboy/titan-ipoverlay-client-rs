use clap::{Arg, Command};
use std::sync::Arc;
use tokio::signal;
use log::{info, error, LevelFilter};
use env_logger;
use tokio::time::{sleep, Duration};

mod tunnel;
use tunnel::{Tunnel, TunnelOptions, BootstrapMgr};

#[tokio::main]
async fn main() {
    let matches = Command::new("titan-ipoverlay-client")
        .about("vms client")
        .version(env!("CARGO_PKG_VERSION"))
        .arg(Arg::new("app-dir").long("app-dir").default_value("").help("--app-dir='./'"))
        .arg(Arg::new("direct-url").long("direct-url").default_value("").help("--direct-url=http://localhost:41005/node/pop"))
        .arg(Arg::new("uuid").long("uuid").required(true).help("--uuid ..."))
        .arg(Arg::new("udp-timeout").long("udp-timeout").default_value("60").help("--udp-timeout 60"))
        .arg(Arg::new("tcp-timeout").long("tcp-timeout").default_value("3").help("--tcp-timeout 3"))
        .arg(Arg::new("debug").long("debug").action(clap::ArgAction::SetTrue).help("--debug"))
        .get_matches();

    if matches.get_flag("debug") {
        env_logger::builder().filter_level(LevelFilter::Debug).init();
    } else {
        env_logger::builder().filter_level(LevelFilter::Info).init();
    }

    let app_dir = matches.get_one::<String>("app-dir").unwrap().clone();
    let direct_url = matches.get_one::<String>("direct-url").unwrap().clone();
    let uuid = matches.get_one::<String>("uuid").unwrap().clone();
    let udp_timeout: u64 = matches.get_one::<String>("udp-timeout").unwrap().parse().unwrap_or(60);
    let tcp_timeout: u64 = matches.get_one::<String>("tcp-timeout").unwrap().parse().unwrap_or(3);

    let mut opts = TunnelOptions {
        uuid: uuid.clone(),
        udp_timeout,
        tcp_timeout,
        bootstrap_mgr: None,
        direct_url: direct_url.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    if direct_url.is_empty() {
        match BootstrapMgr::new(&app_dir).await {
            Ok(mgr) => {
                if mgr.bootstraps().await.is_empty() {
                    error!("no bootstrap nodes found");
                    return;
                }
                opts.bootstrap_mgr = Some(Arc::new(mgr));
            }
            Err(e) => {
                error!("bootstrap init error: {:?}", e);
                return;
            }
        }
    }

    let tun = match Tunnel::new(opts).await {
        Ok(t) => t,
        Err(e) => {
            error!("failed to create tunnel: {:?}", e);
            return;
        }
    };

    if let Err(e) = tun.connect().await {
        error!("connect error: {:?}", e);
        return;
    }

    info!("Start ip overlay success");

    // spawn the serve loop
    let tun_clone = Arc::clone(&tun);
    tokio::spawn(async move {
        // let _ = tun_clone.serve().await;
        tun_serve(tun_clone).await
    });

    // wait for ctrl-c
    match signal::ctrl_c().await {
        Ok(_) => {
            info!("Received Ctrl+C, shutting down...");
        }
        Err(e) => {
            error!("Failed to listen for ctrl_c: {:?}", e);
        }
    }

    let _ = tun.destroy().await;
}


async fn tun_serve(tun: Arc<Tunnel>) {
    loop {
        // Serve 一次
        let tun_clone = Arc::clone(&tun);
        if let Err(e) = tun_clone.serve().await {
            error!("tun serve error: {:?}", e);
        }

        // 如果已经被销毁，就退出
        if tun.is_destroyed().await {
            info!("Tunnel destroyed, exiting serve loop");
            return;
        }

        // 连接失败则不断重试
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