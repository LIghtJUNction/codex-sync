use crate::config::Config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::net::UdpSocket;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub node_id: String,
    pub name: String,
    pub address: String,
}

pub async fn responder(cfg: Config) -> Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, cfg.discovery_port)).await?;
    socket.set_broadcast(true)?;
    let mut buf = [0u8; 512];
    loop {
        let (n, from) = socket.recv_from(&mut buf).await?;
        if &buf[..n] == b"CODEX_SYNC_DISCOVER_V1" {
            let port = cfg.listen.rsplit(':').next().unwrap_or("8787");
            let peer = Peer {
                node_id: cfg.node_id(),
                name: cfg.node_name.clone(),
                address: format!("{}:{port}", from.ip()),
            };
            socket.send_to(&serde_json::to_vec(&peer)?, from).await?;
        }
    }
}

pub async fn discover(cfg: &Config, seconds: u64) -> Result<Vec<Peer>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_broadcast(true)?;
    socket
        .send_to(
            b"CODEX_SYNC_DISCOVER_V1",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), cfg.discovery_port),
        )
        .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut peers = HashMap::new();
    let mut buf = [0u8; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => {
                if let Ok(peer) = serde_json::from_slice::<Peer>(&buf[..n]) {
                    if peer.node_id != cfg.node_id() {
                        peers.insert(peer.node_id.clone(), peer);
                    }
                }
            }
            _ => break,
        }
    }
    Ok(peers.into_values().collect())
}
