//! Descoberta automática na rede local (protocolo do Jellyfin/Emby).
//!
//! Os clientes enviam em broadcast UDP para a porta 7359 a mensagem `who is JellyfinServer?`
//! e o servidor responde (unicast) com um JSON contendo o endereço HTTP.

use crate::config::AppConfig;
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

pub const DISCOVERY_PORT: u16 = 7359;

/// A mensagem é uma pergunta de descoberta? (Jellyfin e Emby usam textos diferentes.)
pub fn is_discovery_request(payload: &[u8]) -> bool {
    let text = String::from_utf8_lossy(payload).trim().to_lowercase();
    text.contains("who is jellyfinserver") || text.contains("who is embyserver")
}

/// Endereço que o cliente deve usar. `RUSTYFIN_PUBLIC_URL` tem prioridade (necessário no Docker,
/// onde o container não conhece o IP da máquina na rede); senão usa o IP local usado para
/// alcançar o cliente.
pub fn server_address(config: &AppConfig, local_ip: Option<std::net::IpAddr>) -> Option<String> {
    if let Some(url) = config.public_url.as_deref() {
        return Some(url.trim_end_matches('/').to_string());
    }
    local_ip
        .filter(|ip| !ip.is_unspecified())
        .map(|ip| format!("http://{}:{}", ip, config.port))
}

pub fn build_response(config: &AppConfig, address: &str) -> String {
    json!({
        "Address": address,
        "Id": config.server_id,
        "Name": crate::settings::server_name(),
        "EndpointAddress": null
    })
    .to_string()
}

/// IP local que o sistema usaria para falar com `peer` (sem enviar nada).
fn local_ip_for(peer: SocketAddr) -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect(peer).ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

pub fn spawn(config: Arc<AppConfig>) {
    if !config.discovery_enabled {
        info!("Descoberta automática desativada (RUSTYFIN_DISCOVERY=false).");
        return;
    }
    tokio::spawn(async move {
        let socket = match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)).await {
            Ok(s) => s,
            Err(e) => {
                warn!("Descoberta automática indisponível (UDP {}): {}", DISCOVERY_PORT, e);
                return;
            }
        };
        info!("Descoberta automática ativa (UDP {}).", DISCOVERY_PORT);
        if config.public_url.is_none() {
            info!("Dica: em Docker defina RUSTYFIN_PUBLIC_URL (ex.: http://192.168.1.10:8096) para o endereço anunciado.");
        }

        let mut buf = [0u8; 512];
        loop {
            let (len, peer) = match socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    debug!("Erro na descoberta: {}", e);
                    continue;
                }
            };
            if !is_discovery_request(&buf[..len]) {
                continue;
            }
            let Some(address) = server_address(&config, local_ip_for(peer)) else {
                warn!("Descoberta: não foi possível determinar o endereço para {}", peer);
                continue;
            };
            let reply = build_response(&config, &address);
            match socket.send_to(reply.as_bytes(), peer).await {
                Ok(_) => info!("Descoberta: respondi a {} com {}", peer, address),
                Err(e) => warn!("Descoberta: falha ao responder a {}: {}", peer, e),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AppConfig {
        let mut c = AppConfig::load();
        c.port = 8096;
        c.public_url = None;
        c
    }

    #[test]
    fn recognizes_requests() {
        assert!(is_discovery_request(b"who is JellyfinServer?"));
        assert!(is_discovery_request(b"Who is EmbyServer?\n"));
        assert!(!is_discovery_request(b"hello"));
        assert!(!is_discovery_request(b""));
    }

    #[test]
    fn address_prefers_public_url() {
        let mut c = config();
        let ip = Some("192.168.1.5".parse().unwrap());
        assert_eq!(server_address(&c, ip).as_deref(), Some("http://192.168.1.5:8096"));
        c.public_url = Some("http://meu-pc:8096/".into());
        assert_eq!(server_address(&c, ip).as_deref(), Some("http://meu-pc:8096"));
        c.public_url = None;
        assert_eq!(server_address(&c, Some("0.0.0.0".parse().unwrap())), None);
        assert_eq!(server_address(&c, None), None);
    }

    #[test]
    fn response_has_jellyfin_shape() {
        let c = config();
        let v: serde_json::Value = serde_json::from_str(&build_response(&c, "http://x:8096")).unwrap();
        assert_eq!(v["Address"], "http://x:8096");
        assert_eq!(v["Id"], c.server_id.as_str());
        assert!(v["EndpointAddress"].is_null());
    }
}
