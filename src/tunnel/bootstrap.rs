use anyhow::Result;
use std::path::Path;
use serde::Deserialize;
use std::fs;

#[derive(Debug)]
pub struct BootstrapMgr {
    bootstraps: Vec<String>,
}

impl BootstrapMgr {
    pub async fn new(app_dir: &str) -> Result<Self> {
        let path = Path::new(app_dir).join("bootstraps.json");
        if path.exists() {
            let content = fs::read_to_string(path)?;
            #[derive(Deserialize)]
            struct Cfg { bootstraps: Vec<String> }
            let cfg: Cfg = serde_json::from_str(&content)?;
            Ok(Self { bootstraps: cfg.bootstraps })
        } else {
            Ok(Self { bootstraps: vec![] })
        }
    }

    pub fn bootstraps(&self) -> Vec<String> {
        self.bootstraps.clone()
    }
}
