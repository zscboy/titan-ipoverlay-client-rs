use std::sync::Arc;
use std::time::{Instant};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use anyhow::Result;
use log::{debug, error};

use crate::tunnel::tunnel::Tunnel;

#[derive(Clone)]
pub struct UdpProxy {
    pub id: String,
    pub socket: Arc<UdpSocket>,
    pub timeout_secs: u64,

    /// The timestamp of the most recent read or write activity
    pub last_active: Arc<Mutex<Instant>>,
}

impl UdpProxy {
    pub fn new(id: String, socket: Arc<UdpSocket>, timeout_secs: u64) -> Self {
        Self {
            id,
            socket,
            timeout_secs,
            last_active: Arc::new(Mutex::new(Instant::now())),
        }
    }

    pub async fn destroy(&self) {
        let socket_to_close = self.socket.clone();
        drop(socket_to_close);
    }

    /// Write data and update last_active
    pub async fn write(&self, data: &[u8]) -> Result<()> {
        {
            let mut t = self.last_active.lock().await;
            *t = Instant::now();
        }

        self.socket.send(data).await?;
        Ok(())
    }

    // Idle timeout watchdog: close socket if no read/write occurs
    pub async fn close_udp_if_timeout(self: Arc<Self>) {
        let t = self.last_active.lock().await;
        if t.elapsed().as_secs() >= self.timeout_secs {
            debug!(
                "UdpProxy idle timeout id={} after {} secs",
                self.id, self.timeout_secs
            );

            // Close socket
            let socket_to_close = self.socket.clone();
            drop(socket_to_close);
        }
    }

    pub async fn serve(self: Arc<Self>, tunnel: Arc<Tunnel>) -> Result<()> {
        let mut buf = vec![0u8; 4096];
        loop {
            match self.socket.recv_from(&mut buf).await {
                Ok((n, _from)) => {
                    {
                        let mut t = self.last_active.lock().await;
                        *t = Instant::now();
                    }

                    if let Err(e) =
                        tunnel.on_proxy_udp_data_from_proxy(&self.id, &buf[..n]).await
                    {
                        error!("on_proxy_udp_data_from_proxy error: {}", e);
                    }
                }
                Err(e) => {
                    debug!("UdpProxy.serve recv_from error: {:?}", e);
                    break;
                }
            }
        }

        if let Err(e) = tunnel.on_proxy_udp_close(&self.id).await {
            error!("on_proxy_udp_close error: {}", e);
        }

         debug!("udp proxy {} close", self.id);

        Ok(())
    }
}
