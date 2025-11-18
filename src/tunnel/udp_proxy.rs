use std::sync::Arc;
use std::time::{Instant};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use anyhow::Result;
use log::{debug, error};
use tokio::sync::{Notify};

use crate::tunnel::tunnel::Tunnel;

#[derive(Clone)]
pub struct UdpProxy {
    pub id: String,
    pub socket: Arc<UdpSocket>,
    pub timeout_secs: u64,

    /// The timestamp of the most recent read or write activity
    pub last_active: Arc<Mutex<Instant>>,
    pub closed_notify: Arc<Notify>,
}

impl UdpProxy {
    pub fn new(id: String, socket: Arc<UdpSocket>, timeout_secs: u64) -> Self {
        Self {
            id,
            socket,
            timeout_secs,
            last_active: Arc::new(Mutex::new(Instant::now())),
            closed_notify: Arc::new(Notify::new()),
        }
    }

    pub async fn destroy(&self) {
        debug!("udp proxy {} destroy called", self.id);
        self.closed_notify.notify_waiters();
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
            self.closed_notify.notify_waiters();
        }
    }

    pub async fn serve(self: Arc<Self>, tunnel: Arc<Tunnel>) -> Result<()> {
        let mut buf = vec![0u8; 4096];
         loop {
            tokio::select! {
                recv_res = self.socket.recv_from(&mut buf) => {
                    match recv_res {
                        Ok((n, _from)) => {
                            // update last_active
                            let mut t = self.last_active.lock().await;
                            *t = Instant::now();

                            if let Err(e) = tunnel.on_proxy_udp_data_from_proxy(&self.id, &buf[..n]).await {
                                error!("on_proxy_udp_data_from_proxy error: {}", e);
                            }
                        }
                        Err(e) => {
                            debug!("udp proxy {} recv_from error: {:?}", self.id, e);
                            break;
                        }
                    }
                }

                _ = self.closed_notify.notified() => {
                    debug!("udp proxy {} closing via manual destroy", self.id);
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
