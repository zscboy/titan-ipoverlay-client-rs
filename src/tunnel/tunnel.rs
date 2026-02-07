use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use anyhow::Result;
use tokio::sync::{Mutex, RwLock, watch};
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use reqwest::Client;
use log::{debug, error, info};
use prost::Message;
use futures_util::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use tokio_tungstenite::{WebSocketStream, MaybeTlsStream};
use tokio::net::TcpStream;
use url::Url;
use dashmap::DashMap;
use std::error::Error;
use crate::tunnel::{bootstrap::BootstrapMgr, tcp_proxy::TcpProxy, udp_proxy::UdpProxy};
use tokio::net::UdpSocket;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio::task::JoinHandle;

const KEEPALIVE_INTERVAL: u64 = 5;
const MAX_PONG_MISS: i32 = 15;
const WS_WRITE_TIMEOUT: u64 = 3;

pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/pb.rs"));
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Pop {
    #[serde(rename = "server_url")]
    pub url: String,
    #[serde(rename = "access_token")]
    pub token: String,
}

pub struct TunnelOptions {
    pub uuid: String,
    pub udp_timeout: u64,
    pub tcp_timeout: u64,
    pub bootstrap_mgr: Option<Arc<BootstrapMgr>>,
    pub direct_url: String,
    pub version: String,
}

pub struct Tunnel {
    uuid: String,
    ws_writer: Arc<Mutex<Option<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>>>>,
    ws_reader: Arc<Mutex<Option<SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>>>>,
    // write_lock: Mutex<()>,
    bootstrap_mgr: Option<Arc<BootstrapMgr>>,
    direct_url: String,
    waitpone: Arc<RwLock<i32>>,
    proxy_sessions: DashMap<String, Arc<TcpProxy>>,
    proxy_udps: DashMap<String, Arc<UdpProxy>>,
    is_destroy: Arc<RwLock<bool>>,
    udp_timeout: u64,
    tcp_timeout: u64,
    cancel_keepalive: Mutex<watch::Sender<bool>>,
    keepalive_task: Mutex<Option<JoinHandle<()>>>,
    cancel_ws_reader: watch::Sender<bool>,
    version: String,
    http_client: Client,
}

impl Tunnel {
    pub async fn new(opts: TunnelOptions) -> Result<Arc<Self>> {
        let (tx, _rx) = watch::channel(false);
        let (tx_reader, _rx2) = watch::channel(false);

        let t = Arc::new(Self {
            uuid: opts.uuid,
            ws_writer: Arc::new(Mutex::new(None)),
            ws_reader: Arc::new(Mutex::new(None)),
            // write_lock: Mutex::new(()),
            bootstrap_mgr: opts.bootstrap_mgr,
            direct_url: opts.direct_url,
            waitpone: Arc::new(RwLock::new(0)),
            proxy_sessions: DashMap::new(),
            proxy_udps: DashMap::new(),
            is_destroy: Arc::new(RwLock::new(false)),
            udp_timeout: opts.udp_timeout,
            tcp_timeout: opts.tcp_timeout,
            cancel_keepalive: Mutex::new(tx),
            keepalive_task: Mutex::new(None),
            cancel_ws_reader: tx_reader,
            version: opts.version,
            http_client: Client::builder().timeout(Duration::from_secs(5)).build()?,
        });

        Tunnel::start_udp_idle_watchdog(&t).await;
        Ok(t)
    }

    pub async fn connect(self: &Arc<Self>) -> Result<()> {
        let _ = self.cancel_ws_reader.send(false);
        let pop = self.get_pop().await?;
        let url = format!(
            "{}?id={}&os={}&version={}",
            pop.url, self.uuid, std::env::consts::OS, self.version
        );

        let ws_url = Url::parse(&url)?;
        let mut req = ws_url.clone().into_client_request()?;
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", pop.token).parse().unwrap(),
        );

        let (ws_stream, _resp) = connect_async(req)
            .await
            .map_err(|e| anyhow::anyhow!(format!("websocket dial err: {}", e)))?;

        let (writer, reader) = ws_stream.split();

        *self.ws_writer.lock().await = Some(writer);
        *self.ws_reader.lock().await = Some(reader);

        *self.waitpone.write().await = 0;

        self.start_keepalive().await;

        info!("Tunnel.Connect, new tun {}", url);
        Ok(())
    }

    pub async fn start_keepalive(self: &Arc<Self>) {
        // ① 停掉旧的 keepalive
        if let Some(old) = self.keepalive_task.lock().await.take() {
            info!("stopping old keepalive");
            let _ = self.cancel_keepalive.lock().await.send(true);
            let _ = old.await; // ⭐ 等待真正退出
        }

        // ② 创建新的 cancel channel
        let (tx, _rx) = watch::channel(false);
        *self.cancel_keepalive.lock().await = tx;

        // ③ 重置 pong 计数
        *self.waitpone.write().await = 0;

        // ④ 启动新的 keepalive
        let me = Arc::clone(self);
        let handle = tokio::spawn(async move {
            me.keepalive_loop().await;
        });

        *self.keepalive_task.lock().await = Some(handle);

        info!("keepalive started");
    }



    async fn get_pop(&self) -> Result<Pop> {
        let access_points = if !self.direct_url.is_empty() {
            vec![self.direct_url.clone()]
        } else {
            self.get_accesspoint().await
        };

        if access_points.is_empty() {
            return Err(anyhow::anyhow!("no access point found"));
        }

        //  debug!("access_points {}", string::fr);
        for ap in access_points {
            let server_url = format!("{}?nodeid={}", ap, self.uuid);
            match self.http_get(&server_url).await {
                Ok(bytes) => {
                    if let Ok(pop) = serde_json::from_slice::<Pop>(&bytes) {
                        return Ok(pop);
                    } else {
                        debug!("parse pop json err");
                    }
                }
                Err(e) => {
                    debug!("htt_get err {} url: {}", e, server_url);
                    continue;
                }
            }
        }
        Err(anyhow::anyhow!("no pop found"))
    }

    pub async fn get_accesspoint(&self) -> Vec<String> {
        // bootstrap URLs
        // let bootstrap_urls = self.bootstrap_mgr.bootstraps().await;
        //  if let Some(mgr) = &self.bootstrap_mgr { mgr.bootstraps().await } else { vec![] }
        let bootstrap_urls = if let Some(mgr) = &self.bootstrap_mgr {
            mgr.bootstraps().await
        } else {
            Vec::new()
        };

        for bootstrap_url in bootstrap_urls {
            // HTTP GET
            let bytes = match self.http_get(&bootstrap_url).await {
                Ok(b) => b,
                Err(e) => {
                    error!("Tunnel.get_access_point http_get error: {}, url: {}", e, bootstrap_url);
                    continue;
                }
            };

            // JSON struct
            #[derive(serde::Deserialize)]
            struct Config {
                #[serde(rename = "accesspoints")]
                access_points: Vec<String>,
            }

            // Parse JSON
            let cfg: Config = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    error!("Tunnel.get_access_point unmarshal error: {}", e);
                    continue;
                }
            };

            return cfg.access_points;
        }

        Vec::new()
    }

    async fn http_get(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.http_client.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!(format!("StatusCode {}", resp.status())));
        }
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn destroy(self: &Arc<Self>) -> Result<()> {
        *self.is_destroy.write().await = true;

        let _ = self.cancel_ws_reader.send(true);

        if let Some(task) = self.keepalive_task.lock().await.take() {
            let _ = self.cancel_keepalive.lock().await.send(true);
            let _ = task.await;
        }

        Ok(())
    }

    pub async fn is_destroyed(&self) -> bool {
        *self.is_destroy.read().await
    }

    pub async fn serve(self: Arc<Self>) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut shutdown = self.cancel_ws_reader.subscribe();

        // 先 take 出 reader，循环直接用 owned 值
        let ws_reader_opt = { self.ws_reader.lock().await.take() };

        if let Some(mut ws_reader) = ws_reader_opt {
            loop {
                tokio::select! {
                    _ = shutdown.changed() => {
                        info!("serve received shutdown");
                        break;
                    }

                    ws_msg = ws_reader.next() => {
                        match ws_msg {
                            Some(Ok(WsMessage::Binary(bin))) => {
                                {
                                    *self.waitpone.write().await = 0;
                                }
                                if let Err(e) = self.on_tunnel_msg(&bin).await {
                                    error!("on_tunnel_msg err: {:?}", e);
                                }
                            }
                            Some(Ok(WsMessage::Ping(p))) => {
                                {
                                    *self.waitpone.write().await = 0;
                                }
                                let _ = self.write_pong(&p).await;
                            }
                            Some(Ok(WsMessage::Pong(_))) => {
                                *self.waitpone.write().await = 0;
                            }
                            Some(Err(e)) => {
                                error!("ws read error: {:?}", e);
                                break;
                            }
                            None => {
                                info!("ws reader closed");
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // ws_reader.close().await;

        if let Some(mut ws) = self.ws_writer.lock().await.take() {
            let _ = ws.close().await;
        }

        let _ = self.cancel_ws_reader.send(true);

        self.on_close().await;

        info!("tunnel {} serve exit", self.uuid);
        Ok(())
    }

    async fn on_tunnel_msg(self: &Arc<Self>, message: &[u8]) -> Result<(), Box<dyn Error + Send + Sync>> {
        let msg: pb::Message = pb::Message::decode(message)?;
        match pb::MessageType::try_from(msg.r#type).unwrap() {
            pb::MessageType::ProxySessionCreate => self.on_proxy_session_create(msg).await?,
            pb::MessageType::ProxySessionData => self.on_proxy_session_data_from_tunnel(msg).await?,
            pb::MessageType::ProxySessionHalfClose => self.on_proxy_session_half_close_from_tunnel(msg).await?,
            pb::MessageType::ProxySessionClose => self.on_proxy_session_close_from_tunnel(msg).await?,
            pb::MessageType::ProxyUdpData => self.on_proxy_udp_data_from_tunnel(msg).await?,
            _ => error!("onTunnelMsg unsupported message type {:?}", msg.r#type),
        }
        Ok(())
    }

    async fn on_proxy_session_create(self: &Arc<Self>, msg: pb::Message) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("on_proxy_session_create session_id:{}", msg.session_id);
        self.clone().create_proxy_session(msg).await?;
        Ok(())
    }

    async fn create_proxy_session(
        self: Arc<Self>,
        msg: pb::Message,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {

        // 1️⃣ 如果 session 已存在，直接回复
        if self.proxy_sessions.contains_key(&msg.session_id) {
            return self
                .create_proxy_session_reply(&msg.session_id, None)
                .await;
        }

        let session_id = msg.session_id.clone();

        // 2️⃣ 0-RTT：立即创建 TcpProxy（已是 Arc）
        let proxy = TcpProxy::new_with_queue(session_id.clone());

        // 3️⃣ 立刻放入 session 表，后续 data 可以直接写队列
        self.proxy_sessions
            .insert(session_id.clone(), proxy.clone());

        info!(
            "tcp proxy {} created with write queue (0-RTT), connecting in background",
            session_id
        );

        // 4️⃣ 后台异步建连
        let tunnel = self.clone();
        let payload = msg.payload.clone();

        tokio::spawn({
            let proxy = proxy.clone();
            let session_id = session_id.clone();
            let tunnel = tunnel.clone();

            async move {
                // -------- 解析目标地址 --------
                let dest_addr = match pb::DestAddr::decode(payload.as_ref()) {
                    Ok(addr) => addr,
                    Err(e) => {
                        error!("decode dest_addr failed: {}", e);
                        let tunnel1 = tunnel.clone();
                        let _ = tunnel1
                            .create_proxy_session_reply(&session_id, Some(Box::new(e)))
                            .await;

                        let tunnel2 = tunnel.clone();
                        let _ = tunnel2.on_proxy_conn_close_from_proxy(&session_id).await;
                        return;
                    }
                };

                // -------- 建立 TCP 连接（带超时）--------
                let conn = match timeout(
                    Duration::from_secs(tunnel.tcp_timeout),
                    TcpStream::connect(dest_addr.addr.clone()),
                )
                .await
                {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(e)) => {
                        error!("tcp connect failed {}: {}", session_id, e);
                        let tunnel1 = tunnel.clone();
                        let _ = tunnel1
                            .create_proxy_session_reply(&session_id, Some(Box::new(e)))
                            .await;

                        let tunnel2 = tunnel.clone();
                        let _ = tunnel2.on_proxy_conn_close_from_proxy(&session_id).await;
                        return;
                    }
                    Err(e) => {
                        error!("tcp connect timeout {}: {}", session_id, e);
                        let err = std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            e.to_string(),
                        );
                        let tunnel1 = tunnel.clone();
                        let _ = tunnel1
                            .create_proxy_session_reply(&session_id, Some(Box::new(err)))
                            .await;

                        let tunnel2 = tunnel.clone();
                        let _ = tunnel2.on_proxy_conn_close_from_proxy(&session_id).await;
                        return;
                    }
                };

                info!(
                    "tcp connection established for {}, addr={}",
                    session_id, dest_addr.addr
                );

                // -------- 注入连接（关键改动点）--------
                if let Err(e) = proxy.set_connection(conn).await {
                    error!("set_connection failed {}: {}", session_id, e);
                    let err = std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    );
                    let tunnel1 = tunnel.clone();
                    let _ = tunnel1
                            .create_proxy_session_reply(&session_id, Some(Box::new(err)))
                            .await;

                    let tunnel2 = tunnel.clone();
                    let _ = tunnel2.on_proxy_conn_close_from_proxy(&session_id).await;
                    return;
                }

                // -------- 建连成功，回复 OK --------
                let tunnel_clone = tunnel.clone();
                if let Err(e) = tunnel_clone
                    .create_proxy_session_reply(&session_id, None)
                    .await
                {
                    error!(
                        "send create_proxy_session_reply failed {}: {}",
                        session_id, e
                    );
                    let tunnel1 = tunnel.clone();
                    let _ = tunnel1.on_proxy_conn_close_from_proxy(&session_id).await;
                    return;
                }

                // -------- 进入 TCP -> WS 转发循环 --------
                proxy.proxy_conn(tunnel).await;
            }
        });

        // 5️⃣ 立即返回（0-RTT）
        Ok(())
    }


    async fn create_proxy_session_reply(self: Arc<Self>, session_id: &str, err: Option<Box<dyn Error + Send + Sync>>) -> Result<(), Box<dyn Error + Send + Sync>> {
        let reply = pb::CreateSessionReply { success: err.is_none(), err_msg: err.as_ref().map_or("".to_string(), |e| e.to_string()) };
        let mut buf = Vec::new();
        reply.encode(&mut buf)?;
        let msg = pb::Message { r#type: pb::MessageType::ProxySessionCreate as i32, session_id: session_id.to_string(), payload: buf };
        let mut data = Vec::new();
        msg.encode(&mut data)?;
        self.write(&data).await?;
        Ok(())
    }

    async fn on_proxy_session_data_from_tunnel(self: &Arc<Self>, msg: pb::Message) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(proxy) = self.proxy_sessions.get(&msg.session_id) {
            proxy.write(&msg.payload).await?;
        } else { return Err(format!("session {} not found", msg.session_id).into()); }
        Ok(())
    }

    async fn on_proxy_session_half_close_from_tunnel(self: &Arc<Self>, msg: pb::Message) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("on_proxy_session_half_close_from_tunnel session_id:{}", msg.session_id);
        if let Some(proxy) = self.proxy_sessions.get(&msg.session_id) {
             proxy.half_close().await?;
        } else { return Err(format!("session {} not found", msg.session_id).into()); }
        Ok(())
    }

    async fn on_proxy_session_close_from_tunnel(self: &Arc<Self>, msg: pb::Message) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("on_proxy_session_close_from_tunnel session_id:{}", msg.session_id);
        if let Some((_, proxy)) = self.proxy_sessions.remove(&msg.session_id) {
            proxy.close_by_server().await;
        } else {
            info!("session {} not found when close from tunnel", msg.session_id);
        }
        Ok(())
    }

    async fn on_proxy_udp_data_from_tunnel(self: &Arc<Self>, msg: pb::Message) -> Result<(), Box<dyn Error + Send + Sync>> {
        let udp_data = pb::UdpData::decode(msg.payload.as_ref())?;
        let id = msg.session_id.clone();

        if let Some(proxy) = self.proxy_udps.get(&id) {
            proxy.write(&udp_data.data).await?;
            return Ok(());
        }

        let raddr: std::net::SocketAddr = match udp_data.addr.parse() {
            Ok(a) => a,
            Err(_) => {
                match tokio::net::lookup_host(&udp_data.addr).await {
                    Ok(mut addrs) => {
                        match addrs.next() {
                            Some(a) => a,
                            None => {
                                error!("DNS lookup returned no result for {}", udp_data.addr);
                                return Err(anyhow::anyhow!("invalid udp addr").into());
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to resolve domain {}: {}", udp_data.addr, e);
                        return Err(anyhow::anyhow!("invalid udp addr").into());
                    }
                }
            }
        };

        let conn: UdpSocket = UdpSocket::bind("0.0.0.0:0").await?;
        conn.connect(raddr).await?;

        // UdpProxy { id: id.clone(), socket: Arc::new(conn), timeout_secs: self.udp_timeout }
        let proxy_udp = Arc::new(UdpProxy::new(id.clone(), Arc::new(conn), self.udp_timeout ));
        proxy_udp.write(&udp_data.data).await?;
        self.proxy_udps.insert(id.clone(), proxy_udp.clone());

        info!("Tunnel.on_proxy_udp_data_from_tunnel new udp:{}, id:{}, total udp:{}", udp_data.addr, proxy_udp.id.clone(), self.proxy_udps.len());

        let tunnel_clone = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = proxy_udp.serve(tunnel_clone).await {
                error!("UDPProxy serve failed: {}", e);
            }
        });

        Ok(())
    }

      // 给 proxy 调用，发送 TCP session 数据回 tunnel
    pub async fn on_proxy_session_data_from_proxy(self: &Arc<Self>, session_id: &str, data: &[u8]) -> Result<(), Box<dyn Error + Send + Sync>> {
        let msg = pb::Message {
            r#type: pb::MessageType::ProxySessionData as i32,
            session_id: session_id.to_string(),
            payload: data.to_vec(),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf)?;
        
        if let Err(e) = self.write(&buf).await {
            error!("on_proxy_session_data_from_proxy {} write: {}", session_id, e);
        }

        Ok(())
    }

    pub async fn on_proxy_conn_half_close_from_proxy(self: &Arc<Self>, session_id: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("Tunnel.on_proxy_conn_half_close_from_proxy session id:{}", session_id);
        let msg = pb::Message {
            r#type: pb::MessageType::ProxySessionHalfClose as i32,
            session_id: session_id.to_string(),
            payload: vec![],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf)?;
        self.write(&buf).await?;
        Ok(())
    }


    // 给 proxy 调用，通知 tunnel 某个 session 关闭
    pub async fn on_proxy_conn_close_from_proxy(self: &Arc<Self>, session_id: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("Tunnel.on_proxy_conn_close_from_proxy session id:{}", session_id);
        let msg = pb::Message {
            r#type: pb::MessageType::ProxySessionClose as i32,
            session_id: session_id.to_string(),
            payload: vec![],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf)?;
        self.write(&buf).await?;

        if self.proxy_sessions.remove(session_id).is_none() {
             info!("session {} not found when close from proxy", session_id);
        }
        Ok(())
    }

    pub async fn on_proxy_udp_close_from_proxy(self: &Arc<Self>, session_id: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("Tunnel.on_proxy_udp_close_from_proxy session id:{}", session_id);
        self.proxy_udps.remove(session_id);
        Ok(())
    }


    // 给 proxy 调用，发送 UDP 数据回 tunnel
    pub async fn on_proxy_udp_data_from_proxy(self: &Arc<Self>, session_id: &str, data: &[u8]) -> Result<(), Box<dyn Error + Send + Sync>> {
        let msg = pb::Message {
            r#type: pb::MessageType::ProxyUdpData as i32,
            session_id: session_id.to_string(),
            payload: data.to_vec(),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf)?;
        self.write(&buf).await?;
        Ok(())
    }

    async fn write(&self, data: &[u8]) -> Result<()> {
        let mut guard = self.ws_writer.lock().await;
        let ws = guard.as_mut().ok_or_else(|| anyhow::anyhow!("ws_writer is none"))?;

        let result = timeout(Duration::from_secs(WS_WRITE_TIMEOUT),
            ws.send(WsMessage::Binary(data.to_vec()))
        ).await;

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                error!("ws write failed: {}", e);
                Err(anyhow::anyhow!(e))
            }
            Err(_) => {
                error!("ws write timeout");
                Err(anyhow::anyhow!("ws send timeout"))
            }
        }
    }

    async fn write_ping(&self, data: &[u8]) -> Result<()> {
        let mut guard = self.ws_writer.lock().await;
        let ws = guard.as_mut().ok_or_else(|| anyhow::anyhow!("ws_writer is none"))?;

        let result = timeout(Duration::from_secs(WS_WRITE_TIMEOUT),
             ws.send(WsMessage::Ping(data.to_vec()))
        ).await;

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                error!("write_ping failed: {}", e);
                Err(anyhow::anyhow!(e))
            }
            Err(_) => {
                error!("write_ping timeout");
                Err(anyhow::anyhow!("write_ping timeout"))
            }
        }
    }

    async fn write_pong(&self, data: &[u8]) -> Result<()> {
        let mut guard = self.ws_writer.lock().await;
        let ws = guard.as_mut().ok_or_else(|| anyhow::anyhow!("ws_writer is none"))?;

        let result = timeout(Duration::from_secs(WS_WRITE_TIMEOUT),
             ws.send(WsMessage::Pong(data.to_vec()))
        ).await;

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                error!("write_pong failed: {}", e);
                Err(anyhow::anyhow!(e))
            }
            Err(_) => {
                error!("write_pong timeout");
                Err(anyhow::anyhow!("write_pong timeout"))
            }
        }
    }

    async fn on_close(&self) { self.clear_proxys().await; }

    async fn clear_proxys(&self) {
        for k in self.proxy_sessions.iter().map(|e| e.key().clone()).collect::<Vec<_>>() {
            if let Some((_, proxy)) = self.proxy_sessions.remove(&k) { proxy.destroy().await; }
        }
        for k in self.proxy_udps.iter().map(|e| e.key().clone()).collect::<Vec<_>>() {
            if let Some((_, proxy)) = self.proxy_udps.remove(&k) { proxy.destroy().await; }
        }
    }

    async fn keepalive_loop(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(Duration::from_secs(KEEPALIVE_INTERVAL));
        let mut rx = self.cancel_keepalive.lock().await.subscribe();
        let mut rx_ws = self.cancel_ws_reader.subscribe();

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    // 这里做 ping 检查
                    if *self.is_destroy.read().await {
                        info!("tunnel destroyed, exit keepalive_loop");
                        return;
                    }

                    let is_timeout = {
                        let mut w = self.waitpone.write().await;
                        if *w > MAX_PONG_MISS {
                            true
                        } else {
                            *w += 1;
                            if *w > MAX_PONG_MISS / 3 {
                                debug!("keepalive,delay {}", *w)
                            }
                            false
                        }
                    }; // ← 写锁在这里释放

                    if is_timeout {
                        info!("keepalive timeout, close websocket");
                        let _ = self.cancel_ws_reader.send(true);
                        continue;
                    }

                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        .to_le_bytes();
                    
                    if let Err(e) = self.write_ping(&now).await {
                        error!("failed to send ping: {:?}", e);
                    }

                }

                _ = rx.changed() => {
                    // 收到取消通知
                    if *rx.borrow() {
                        info!("keepalive canceled");
                        return;
                    }
                }

                _ = rx_ws.changed() => {
                    if *rx_ws.borrow() {
                        info!("keepalive loop exit due to ws closed");
                        return;
                    }
                }
            }
        }
    }

    pub async fn start_udp_idle_watchdog(this: &Arc<Self>) {
        let tunnel = Arc::clone(this);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(tunnel.udp_timeout/2));
            loop {
                ticker.tick().await;

                if *tunnel.is_destroy.read().await {
                    break;
                }

                let mut to_remove = Vec::new();
                for entry in tunnel.proxy_udps.iter() {
                    let proxy = entry.value();
                    if proxy.check_idle_timeout().await {
                        to_remove.push(entry.key().clone());
                    }
                }

                for id in to_remove {
                    if let Some(proxy) = tunnel.proxy_udps.get(&id) {
                        let _ = proxy.destroy().await;
                    }

                    
                }
            }
        });
    }

}
