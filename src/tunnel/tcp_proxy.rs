use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::Mutex;
use anyhow::Result;
use log::{debug, error};
use crate::tunnel::tunnel::Tunnel;

pub struct TcpProxy {
    pub id: String,
    reader: Mutex<ReadHalf<TcpStream>>,
    writer: Mutex<WriteHalf<TcpStream>>,
    raw: Arc<Mutex<TcpStream>>, // ✅ 保留原始连接，供 shutdown 使用
}

impl TcpProxy {
    pub async fn new(id: String, stream: TcpStream) -> Result<Self> {
        // 先从 tokio stream 提取同步版本
        let std_stream = stream.into_std()?;
        // 克隆出一个完整的 stream 以备 shutdown 使用
        let std_stream_clone = std_stream.try_clone()?;
        // 再转换回 tokio 异步版本
        let raw = TcpStream::from_std(std_stream_clone)?;
        let stream = TcpStream::from_std(std_stream)?; // 原 stream 再恢复回异步版

        let (reader, writer) = tokio::io::split(stream);

        Ok(Self {
            id,
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            raw: Arc::new(Mutex::new(raw)),
        })
    }

    pub async fn proxy_conn(self: Arc<Self>, tunnel: Arc<Tunnel>) {
        let mut buf = [0u8; 4096];
        loop {
            let n = {
                let mut reader = self.reader.lock().await;
                match reader.read(&mut buf).await {
                    Ok(0) => {
                        debug!("tcp proxy read eof id={}", self.id);
                        break;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        error!("tcp proxy read err id={} err={:?}", self.id, e);
                        break;
                    }
                }
            };
            if let Err(e) = tunnel.on_proxy_session_data_from_proxy(&self.id, &buf[..n]).await {
                log::error!("on_proxy_session_data_from_proxy error: {}", e);
            }
        }
         if let Err(e)  = tunnel.on_proxy_conn_close(&self.id).await {
            log::error!("on_proxy_conn_close error: {}", e);
         }
        self.shutdown().await;
    }

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().await;
        writer
            .write_all(data)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    async fn shutdown(&self) {
        let mut raw = self.raw.lock().await;
        let _ = raw.shutdown().await;
    }

    pub async fn close_by_server(&self) {
        self.shutdown().await;
    }

    pub async fn destroy(&self) {
        self.shutdown().await;
    }
}
