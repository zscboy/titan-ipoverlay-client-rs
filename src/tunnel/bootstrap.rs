use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{fs as tokio_fs, io::AsyncWriteExt, sync::RwLock, time::sleep};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use reqwest::Client;

const UPDATE_INTERVAL: u64 = 12 * 60 * 60; // 12 hours in seconds
const BOOTSTRAP_FILE: &str = "bootstrap.json";

// 相当于 Go 的 //go:embed
const EMBED_BOOTSTRAP_JSON: &str = include_str!("bootstrap.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub bootstraps: Vec<String>,
}

#[derive(Clone)]
pub struct BootstrapMgr {
    dir: PathBuf,
    bootstraps: Arc<RwLock<Vec<String>>>,
    client: Client,
}

impl BootstrapMgr {
    pub async fn new(dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let dir: PathBuf = dir.into();
        tokio_fs::create_dir_all(&dir).await?;

        let bootstrap_path = dir.join(BOOTSTRAP_FILE);

        let bytes = match tokio_fs::read(&bootstrap_path).await {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 不存在则写入内置 bootstrap.json
                let embedded = EMBED_BOOTSTRAP_JSON.as_bytes();
                let mut file = tokio_fs::File::create(&bootstrap_path).await?;
                file.write_all(embedded).await?;
                embedded.to_vec()
            }
            Err(e) => return Err(e.into()),
        };

        let cfg: Config = serde_json::from_slice(&bytes)?;
        let mgr = BootstrapMgr {
            dir,
            bootstraps: Arc::new(RwLock::new(cfg.bootstraps.clone())),
            client: Client::builder()
                .timeout(Duration::from_secs(3))
                .build()?,
        };

        mgr.get_bootstraps_from_server().await;

        // 定时更新
        let mgr_clone = mgr.clone();
        tokio::spawn(async move { mgr_clone.timed_update().await });

        Ok(mgr)
    }

    pub async fn bootstraps(&self) -> Vec<String> {
        self.bootstraps.read().await.clone()
    }

    async fn timed_update(self) {
        loop {
            sleep(Duration::from_secs(UPDATE_INTERVAL)).await;
            self.get_bootstraps_from_server().await;
        }
    }

    async fn get_bootstraps_from_server(&self) {
        let urls = self.bootstraps.read().await.clone();
        for url in urls {
            match self.http_get(&url).await {
                Ok(bytes) => {
                    let path = self.dir.join(BOOTSTRAP_FILE);
                    if let Err(e) = tokio_fs::write(&path, &bytes).await {
                        error!("Write bootstrap file failed: {}", e);
                        continue;
                    }

                    match serde_json::from_slice::<Config>(&bytes) {
                        Ok(cfg) => {
                            let mut bs = self.bootstraps.write().await;
                            *bs = cfg.bootstraps;
                            info!("Updated bootstraps from {}", url);
                            break;
                        }
                        Err(e) => {
                            error!("Unmarshal bootstrap JSON failed: {}", e);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    error!("HTTP get failed: {}, url: {}", e, url);
                    continue;
                }
            }
        }
    }

    async fn http_get(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let resp: reqwest::Response = self.client.get(url).send().await?;
        let status: reqwest::StatusCode = resp.status();
        let bytes = resp.bytes().await?;

        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "Status code {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            ));
        }
        Ok(bytes.to_vec())
    }
}
