use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::time::{timeout, Duration};
use anyhow::Result;
use log::{debug, error};
use crate::tunnel::tunnel::Tunnel;

#[derive(Clone)]
pub struct UdpProxy {
    pub id: String,
    pub socket: Arc<UdpSocket>,
    pub timeout_secs: u64,
}

impl UdpProxy {
    pub fn new(id: String, socket: Arc<UdpSocket>, timeout_secs: u64) -> Self {
        Self { id, socket, timeout_secs }
    }

    pub async fn destroy(&self) {
        debug!("udp proxy destroy id={}", self.id);
    }

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let dur = Duration::from_secs(self.timeout_secs);
        let send_future = self.socket.send(data);
        match timeout(dur, send_future).await {
            Ok(Ok(_n)) => Ok(()),
            Ok(Err(e)) => {
                error!("UdpProxy.write error: {:?}", e);
                Err(anyhow::anyhow!(e.to_string()))
            }
            Err(_) => {
                error!("UdpProxy.write timeout after {}s", self.timeout_secs);
                Err(anyhow::anyhow!("timeout"))
            }
        }
    }

    pub async fn serve(self: Arc<Self>, tunnel: Arc<Tunnel>) -> Result<()> {
        let mut buf = vec![0u8; 4096];
        loop {
            let dur = Duration::from_secs(self.timeout_secs);
            match timeout(dur, self.socket.recv_from(&mut buf)).await {
                Ok(Ok((n, _from))) => {
                    tunnel.on_proxy_udp_data_from_proxy(&self.id, &buf[..n]).await;
                }
                Ok(Err(e)) => {
                    debug!("UdpProxy.serve ReadFromUDP: {:?}", e);
                    return Ok(());
                }
                Err(_) => {
                    debug!("UdpProxy.serve timeout after {}s", self.timeout_secs);
                    return Ok(());
                }
            }
        }
    }
}
