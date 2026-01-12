use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Mutex, mpsc, oneshot};
use anyhow::Result;
use log::{debug, error, info};
use crate::tunnel::tunnel::Tunnel;
use tokio::time::{timeout, Duration};


const TCP_WRITE_TIMEOUT: u64 = 3;

enum WriteMsg {
    Data(Vec<u8>),
    HalfClose,
}

pub struct TcpProxy {
    pub id: String,

    reader: Mutex<Option<ReadHalf<TcpStream>>>,
    raw: Mutex<Option<Arc<Mutex<TcpStream>>>>,

    write_queue: mpsc::UnboundedSender<WriteMsg>,

    /// writer ready 只会 send 一次
    writer_ready_tx: Mutex<Option<oneshot::Sender<Arc<Mutex<WriteHalf<TcpStream>>>>>>,

    is_half_closed_by_self: Mutex<bool>
    // 用于优雅关闭 reader
    // reader_notify: Notify,

    // is_close: Mutex<bool>,
}

impl TcpProxy {
    /// 创建 TcpProxy，立即可以接收写入数据（0-RTT）
    pub fn new_with_queue(id: String) -> Arc<Self> {
        let (write_tx, write_rx) = mpsc::unbounded_channel::<WriteMsg>();
        let (writer_ready_tx, writer_ready_rx) = oneshot::channel();

        let proxy = Arc::new(Self {
            id: id.clone(),
            reader: Mutex::new(None),
            raw: Mutex::new(None),
            write_queue: write_tx,
            writer_ready_tx: Mutex::new(Some(writer_ready_tx)),
            is_half_closed_by_self: Mutex::new(false),
            // reader_notify: Notify::new(),
            // is_close: Mutex::new(false),
        });

        let proxy_id = id.clone();
        tokio::spawn(async move {
            Self::write_queue_processor(proxy_id, writer_ready_rx, write_rx).await;
        });

        proxy
    }
    
     pub async fn set_connection(&self, stream: TcpStream) -> Result<()> {
        let std_stream: std::net::TcpStream = stream.into_std()?;
        let std_stream_clone = std_stream.try_clone()?;

        let raw = TcpStream::from_std(std_stream_clone)?;
        let stream: TcpStream = TcpStream::from_std(std_stream)?;

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
        mut write_rx: mpsc::UnboundedReceiver<WriteMsg>,
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

        while let Some(msg) = write_rx.recv().await {
            match msg {
                WriteMsg::Data(data) => {
                    let mut guard = writer.lock().await;
                    let result = timeout(
                        Duration::from_secs(TCP_WRITE_TIMEOUT),
                        guard.write_all(&data),
                    )
                    .await;

                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            error!("write_queue_processor {} write err: {}", id, e);
                            break;
                        }
                        Err(_) => {
                            error!("write_queue_processor {} write timeout", id);
                            break;
                        }
                    }
                }

                WriteMsg::HalfClose => {
                    info!("write_queue_processor {} half_close (shutdown write)", id);
                    let mut guard: tokio::sync::MutexGuard<'_, WriteHalf<TcpStream>> = writer.lock().await;
                    let _ = guard.shutdown().await;
                    break;
                }
            }
        }

        debug!("tcp proxy {} write_queue_processor exit", id);
    }


    pub async fn proxy_conn(self: Arc<Self>, tunnel: Arc<Tunnel>) {
    let mut buf = [0u8; 4096];

    loop {

        let n = tokio::select! {
            res = async {
                let mut guard = self.reader.lock().await;
                let reader = guard.as_mut().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::Other, "reader not ready")
                })?;
                reader.read(&mut buf).await
            } => {
                match res {
                    Ok(0) => {
                        let self_closed = *self.is_half_closed_by_self.lock().await;
                        if !self_closed {
                            debug!("tcp proxy {} remote half-close", self.id);
                            if let Err(e) = tunnel.on_proxy_conn_half_close_from_proxy(&self.id).await {
                                error!("on_proxy_conn_half_close_from_proxy err {}", e);
                            }
                        } else {
                            debug!("tcp proxy {} read 0 due to self shutdown, ignore", self.id);
                            if let Err(e) = tunnel.on_proxy_conn_close_from_proxy(&self.id).await
                            {
                                error!("on_proxy_conn_close_from_proxy err {}", e);
                                return;
                            }
                        }
                        return;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        error!("tcp proxy {} read err {}", self.id, e);
                        if let Err(e) = tunnel.on_proxy_conn_close_from_proxy(&self.id).await
                        {
                            error!("on_proxy_conn_close_from_proxy err {}", e);
                            return;
                        }
                        return;
                    }
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


    pub async fn half_close(&self) -> Result<()> {
        {
            let mut guard = self.is_half_closed_by_self.lock().await;
            *guard = true;
        }
        self.write_queue
            .send(WriteMsg::HalfClose)
            .map_err(|e| anyhow::anyhow!("half_close send failed: {}", e))?;

        Ok(())
    }

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        self.write_queue
            .send(WriteMsg::Data(data.to_vec()))
            .map_err(|e| anyhow::anyhow!("write queue send failed: {}", e))?;
        Ok(())
    }

    async fn shutdown(&self) {
        if let Err(e) = self.half_close().await {
            error!("shutdown {} half_close failed: {}", self.id, e);
        }

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
