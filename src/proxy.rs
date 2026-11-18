//! Servidor atrás de um proxy reverso (nginx, Caddy, Traefik...): descobre o IP **real** do cliente.
//!
//! Várias regras do servidor dependem de o cliente estar na rede local: o painel `/admin`, o login sem senha e as imagens
//! sem token. Atrás de um proxy, a conexão TCP sempre vem do proxy (muitas vezes `127.0.0.1` ou um IP de rede privada), e
//! sem cuidado **todo visitante da internet pareceria da rede local**. Por isso:
//!
//! - só os proxies listados em `RUSTYFIN_TRUSTED_PROXIES` podem informar o IP do cliente (`X-Forwarded-For`/`X-Real-IP`);
//! - uma requisição com cabeçalhos de proxy vinda de quem **não** é confiável é tratada como externa (falha fechada);
//! - um proxy confiável que não informa o IP também deixa a requisição como externa.
//!
//! O resultado substitui o endereço da conexão (`ConnectInfo`), então o restante do código não precisa saber de proxies.

use axum::{
    extract::{ConnectInfo, Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Endereço usado quando não dá para saber de onde o cliente veio: não é privado, então vale como "internet".
pub const EXTERNO: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// Cabeçalhos que só um proxy envia. Vindos de quem não é um proxy confiável, marcam a requisição como externa.
const CABECALHOS_DE_PROXY: [&str; 6] =
    ["x-forwarded-for", "x-real-ip", "forwarded", "x-forwarded-host", "x-forwarded-proto", "cf-connecting-ip"];

/// Um IP ou uma rede (`10.0.0.0/8`, `fd00::/8`, `127.0.0.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rede {
    base: IpAddr,
    prefixo: u8,
}

fn normalizar(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 @ IpAddr::V4(_) => v4,
    }
}

impl FromStr for Rede {
    type Err = String;

    fn from_str(texto: &str) -> Result<Self, String> {
        let texto = texto.trim();
        let (ip, prefixo) = match texto.split_once('/') {
            Some((ip, p)) => (ip, Some(p)),
            None => (texto, None),
        };
        let base = normalizar(ip.trim().parse::<IpAddr>().map_err(|_| format!("endereço inválido: {texto}"))?);
        let maximo = if base.is_ipv4() { 32 } else { 128 };
        let prefixo = match prefixo {
            Some(p) => p.trim().parse::<u8>().ok().filter(|p| *p <= maximo).ok_or_else(|| format!("máscara inválida: {texto}"))?,
            None => maximo,
        };
        Ok(Rede { base, prefixo })
    }
}

impl Rede {
    pub fn contem(&self, ip: IpAddr) -> bool {
        match (self.base, normalizar(ip)) {
            (IpAddr::V4(base), IpAddr::V4(ip)) => {
                let mascara = u32::MAX.checked_shl(32 - u32::from(self.prefixo)).unwrap_or(0);
                u32::from(base) & mascara == u32::from(ip) & mascara
            }
            (IpAddr::V6(base), IpAddr::V6(ip)) => {
                let mascara = u128::MAX.checked_shl(128 - u32::from(self.prefixo)).unwrap_or(0);
                u128::from(base) & mascara == u128::from(ip) & mascara
            }
            _ => false,
        }
    }
}

/// Lê `RUSTYFIN_TRUSTED_PROXIES` (separados por vírgula, espaço ou ponto e vírgula). Entradas inválidas vão para o segundo
/// valor: a configuração segue sem elas, ou seja, com menos confiança, nunca com mais.
pub fn ler_lista(texto: &str) -> (Vec<Rede>, Vec<String>) {
    let mut redes = Vec::new();
    let mut erros = Vec::new();
    for parte in texto.split([',', ';', ' ', '\n']).map(str::trim).filter(|p| !p.is_empty()) {
        match parte.parse::<Rede>() {
            Ok(r) => redes.push(r),
            Err(e) => erros.push(e),
        }
    }
    (redes, erros)
}

fn confia(confiaveis: &[Rede], ip: IpAddr) -> bool {
    confiaveis.iter().any(|r| r.contem(ip))
}

/// `1.2.3.4`, `1.2.3.4:5678`, `[::1]`, `[::1]:80` ou `::1`.
fn ip_da_entrada(texto: &str) -> Option<IpAddr> {
    let texto = texto.trim().trim_matches('"');
    texto
        .parse::<IpAddr>()
        .ok()
        .or_else(|| texto.parse::<SocketAddr>().ok().map(|s| s.ip()))
        .or_else(|| texto.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().ok())
        .map(normalizar)
}

/// O IP do cliente: o da conexão, ou, se a conexão vem de um proxy confiável, o que ele informou.
pub fn ip_efetivo(conexao: IpAddr, cabecalhos: &HeaderMap, confiaveis: &[Rede]) -> IpAddr {
    let conexao = normalizar(conexao);
    let com_cabecalho_de_proxy = CABECALHOS_DE_PROXY.iter().any(|c| cabecalhos.contains_key(*c));
    if !confia(confiaveis, conexao) {
        // Quem não é proxy confiável não pode dizer de onde o cliente veio. Se há sinais de proxy, a requisição pode ter
        // passado por um proxy não configurado: nunca a tratamos como da rede local.
        return if com_cabecalho_de_proxy { EXTERNO } else { conexao };
    }
    // Cadeia de `X-Forwarded-For`: do cliente (esquerda) até o último proxy (direita). Percorre da direita e para no primeiro
    // endereço que não é um proxy nosso; é o cliente de verdade (o que está à esquerda pode ser forjado).
    let cadeia: Vec<IpAddr> = cabecalhos
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|e| ip_da_entrada(e).unwrap_or(EXTERNO))
        .collect();
    if !cadeia.is_empty() {
        // Se todos são proxies nossos, o cliente está dentro da própria rede confiável: vale o primeiro.
        return cadeia.iter().rev().copied().find(|ip| !confia(confiaveis, *ip)).unwrap_or(cadeia[0]);
    }
    cabecalhos
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(ip_da_entrada)
        .unwrap_or(EXTERNO)
}

/// Troca o endereço da conexão pelo do cliente real. Vai por fora das demais camadas.
pub async fn resolver_cliente(State(confiaveis): State<Arc<[Rede]>>, mut req: Request, next: Next) -> Response {
    static AVISADO: AtomicBool = AtomicBool::new(false);
    if let Some(ConnectInfo(conexao)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() {
        let real = ip_efetivo(conexao.ip(), req.headers(), &confiaveis);
        if real != normalizar(conexao.ip()) {
            if real == EXTERNO
                && !confia(&confiaveis, conexao.ip())
                && !AVISADO.swap(true, Ordering::Relaxed)
            {
                tracing::warn!(
                    "Requisição de {} com cabeçalhos de proxy (X-Forwarded-For e afins), mas esse endereço não está em \
                     RUSTYFIN_TRUSTED_PROXIES: ela é tratada como vinda da internet (sem painel nem login sem senha). \
                     Se este é o seu proxy, adicione o endereço dele a RUSTYFIN_TRUSTED_PROXIES.",
                    conexao.ip()
                );
            }
            req.extensions_mut().insert(ConnectInfo(SocketAddr::new(real, conexao.port())));
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn redes(texto: &str) -> Vec<Rede> {
        let (r, e) = ler_lista(texto);
        assert!(e.is_empty(), "{e:?}");
        r
    }

    fn cab(pares: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pares {
            h.append(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn ip(t: &str) -> IpAddr {
        t.parse().unwrap()
    }

    #[test]
    fn redes_e_enderecos() {
        let r = redes("127.0.0.1, 172.16.0.0/12 ::1 fd00::/8");
        assert!(r[0].contem(ip("127.0.0.1")) && !r[0].contem(ip("127.0.0.2")));
        assert!(r[1].contem(ip("172.18.0.5")) && !r[1].contem(ip("172.32.0.1")));
        assert!(r[2].contem(ip("::1")));
        assert!(r[3].contem(ip("fd12::1")) && !r[3].contem(ip("fe80::1")));
        // IPv4 mapeado em IPv6 (o que um socket dual-stack mostra) casa com a rede IPv4.
        assert!(r[0].contem(ip("::ffff:127.0.0.1")));
        assert!(redes("0.0.0.0/0")[0].contem(ip("8.8.8.8")));
        let (_, erros) = ler_lista("10.0.0.0/33, abc, 10.0.0.1");
        assert_eq!(erros.len(), 2);
    }

    #[test]
    fn sem_proxy_vale_o_endereco_da_conexao() {
        let nada: [Rede; 0] = [];
        assert_eq!(ip_efetivo(ip("192.168.1.9"), &HeaderMap::new(), &nada), ip("192.168.1.9"));
    }

    #[test]
    fn proxy_nao_configurado_nunca_parece_rede_local() {
        // O nginx no mesmo PC (127.0.0.1) encaminha um visitante da internet; sem RUSTYFIN_TRUSTED_PROXIES.
        let h = cab(&[("x-forwarded-for", "203.0.113.7")]);
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &h, &[]), EXTERNO);
        // Um cliente da rede local que forja o cabeçalho só consegue parecer externo.
        assert_eq!(ip_efetivo(ip("192.168.1.9"), &cab(&[("x-real-ip", "192.168.1.1")]), &[]), EXTERNO);
    }

    #[test]
    fn proxy_confiavel_informa_o_cliente() {
        let r = redes("127.0.0.1, 10.0.0.0/8");
        // Um salto.
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &cab(&[("x-forwarded-for", "203.0.113.7")]), &r), ip("203.0.113.7"));
        // Cliente forjou a esquerda; vale o que o nosso proxy viu (o último não confiável da direita).
        let h = cab(&[("x-forwarded-for", "192.168.1.1, 203.0.113.7")]);
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &h, &r), ip("203.0.113.7"));
        // Dois proxies nossos em cadeia.
        let h = cab(&[("x-forwarded-for", "203.0.113.7, 10.0.0.2")]);
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &h, &r), ip("203.0.113.7"));
        // Vários cabeçalhos, porta e IPv6 entre colchetes.
        let h = cab(&[("x-forwarded-for", "1.1.1.1"), ("x-forwarded-for", "[2001:db8::1]:5000")]);
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &h, &r), ip("2001:db8::1"));
        // Só X-Real-IP.
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &cab(&[("x-real-ip", "203.0.113.9")]), &r), ip("203.0.113.9"));
        // Cliente da própria rede confiável (todos os saltos são nossos): vale o primeiro.
        let h = cab(&[("x-forwarded-for", "10.0.0.50, 10.0.0.2")]);
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &h, &r), ip("10.0.0.50"));
    }

    #[test]
    fn proxy_confiavel_sem_informar_o_cliente_vira_externo() {
        let r = redes("127.0.0.1");
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &HeaderMap::new(), &r), EXTERNO);
        assert_eq!(ip_efetivo(ip("127.0.0.1"), &cab(&[("x-forwarded-for", "lixo")]), &r), EXTERNO);
    }
}
