use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Mutex, mpsc, oneshot};
use anyhow::Result;
use log::{debug, error, info};
use crate::tunnel::tunnel::Tunnel;
use tokio::time::{timeout, Duration};


const TCP_WRITE_TIMEOUT: u64 = 3;

pub struct TcpProxy {
    pub id: String,

    reader: Mutex<Option<ReadHalf<TcpStream>>>,
    raw: Mutex<Option<Arc<Mutex<TcpStream>>>>,

    write_queue: mpsc::UnboundedSender<Vec<u8>>,

    /// writer ready 只会 send 一次
    writer_ready_tx: Mutex<Option<oneshot::Sender<Arc<Mutex<WriteHalf<TcpStream>>>>>>,
}

impl TcpProxy {
    /// 创建 TcpProxy，立即可以接收写入数据（0-RTT）
    pub fn new_with_queue(id: String) -> Arc<Self> {
        let (write_tx, write_rx) = mpsc::unbounded_channel();
        let (writer_ready_tx, writer_ready_rx) = oneshot::channel();

        let proxy = Arc::new(Self {
            id: id.clone(),
            reader: Mutex::new(None),
            raw: Mutex::new(None),
            write_queue: write_tx,
            writer_ready_tx: Mutex::new(Some(writer_ready_tx)),
        });

        let proxy_id = id.clone();
        tokio::spawn(async move {
            Self::write_queue_processor(proxy_id, writer_ready_rx, write_rx).await;
        });

        proxy
    }
    
    /// 设置 TCP 连接（连接建立后调用）
    // pub async fn set_connection(mut self, stream: TcpStream) -> Result<Self> {
    //     // 从 tokio stream 提取同步版本
    //     let std_stream = stream.into_std()?;
    //     let std_stream_clone = std_stream.try_clone()?;
    //     let raw = TcpStream::from_std(std_stream_clone)?;
    //     let stream = TcpStream::from_std(std_stream)?;

    //     let (reader, writer) = tokio::io::split(stream);
        
    //     self.reader = Some(Mutex::new(reader));
    //     self.raw = Some(Arc::new(Mutex::new(raw)));
        
    //     // 通知写入处理器 writer 已就绪
    //     let writer_arc = Arc::new(Mutex::new(writer));
    //     if let Some(tx) = self.writer_ready_tx.take() {
    //         let _ = tx.send(writer_arc);
    //         info!("tcp proxy {} connection ready, write queue can now flush", self.id);
    //     }
        
    //     Ok(self)
    // }
     pub async fn set_connection(&self, stream: TcpStream) -> Result<()> {
        let std_stream = stream.into_std()?;
        let std_stream_clone = std_stream.try_clone()?;

        let raw = TcpStream::from_std(std_stream_clone)?;
        let stream = TcpStream::from_std(std_stream)?;

        let (reader, writer) = tokio::io::split(stream);

        // 设置 reader
        {
            let mut guard = self.reader.lock().await;
            if guard.is_some() {
                anyhow::bail!("reader already set");
            }
            *guard = Some(reader);
        }

        // 设置 raw
        {
            let mut guard = self.raw.lock().await;
            *guard = Some(Arc::new(Mutex::new(raw)));
        }

        // 通知 writer ready（只会成功一次）
        let writer_arc = Arc::new(Mutex::new(writer));
        let mut tx_guard = self.writer_ready_tx.lock().await;
        if let Some(tx) = tx_guard.take() {
            let _ = tx.send(writer_arc);
            info!("tcp proxy {} connection ready, write queue flushing", self.id);
        }

        Ok(())
    }
    
    /// 写入队列处理器
        async fn write_queue_processor(
        id: String,
        writer_ready_rx: oneshot::Receiver<Arc<Mutex<WriteHalf<TcpStream>>>>,
        mut write_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        let writer = match writer_ready_rx.await {
            Ok(w) => {
                info!("tcp proxy {} writer ready", id);
                w
            }
            Err(_) => {
                error!("tcp proxy {} writer_ready_rx closed", id);
                return;
            }
        };

        while let Some(data) = write_rx.recv().await {
            let mut guard = writer.lock().await;
            let result = timeout(
                Duration::from_secs(TCP_WRITE_TIMEOUT),
                guard.write_all(&data),
            )
            .await;

            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    error!("tcp proxy {} write err: {}", id, e);
                    break;
                }
                Err(_) => {
                    error!("tcp proxy {} write timeout", id);
                    break;
                }
            }
        }

        debug!("tcp proxy {} write processor exit", id);
    }

    pub async fn proxy_conn(self: Arc<Self>, tunnel: Arc<Tunnel>) {
        let mut buf = [0u8; 4096];

        loop {
            let n = {
                let mut guard = self.reader.lock().await;
                let reader = match guard.as_mut() {
                    Some(r) => r,
                    None => {
                        error!("proxy_conn called but reader not ready {}", self.id);
                        return;
                    }
                };

                match reader.read(&mut buf).await {
                    Ok(0) => {
                        debug!("tcp proxy eof {}", self.id);
                        return;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        error!("tcp proxy read err {} {}", self.id, e);
                        return;
                    }
                }
            };

            if let Err(e) = tunnel
                .on_proxy_session_data_from_proxy(&self.id, &buf[..n])
                .await
            {
                error!("send ws data err {}", e);
                return;
            }
        }
   }


    pub async fn write(&self, data: &[u8]) -> Result<()> {
        self.write_queue
            .send(data.to_vec())
            .map_err(|e| anyhow::anyhow!("write queue send failed: {}", e))?;
        Ok(())
    }

    async fn shutdown(&self) {
        if let Some(raw) = self.raw.lock().await.as_ref() {
            let mut g = raw.lock().await;
            let _ = g.shutdown().await;
        }
    }

    pub async fn close_by_server(&self) {
        self.shutdown().await;
    }

    pub async fn destroy(&self) {
        self.shutdown().await;
    }
}
