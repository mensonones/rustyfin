//! Quick Connect: entrar em um app (a TV, por exemplo) com um código de seis dígitos, autorizado por um aparelho já logado,
//! sem digitar usuário e senha com o controle remoto. O fluxo é o do Jellyfin:
//!
//! 1. o app pede `POST /QuickConnect/Initiate` e mostra o `Code`;
//! 2. o usuário digita o código num aparelho logado (`POST /QuickConnect/Authorize`, ou o painel);
//! 3. o app consulta `GET /QuickConnect/Connect?secret=...` até `Authenticated` ficar verdadeiro;
//! 4. o app troca o `Secret` por um token em `POST /Users/AuthenticateWithQuickConnect` (uso único).
//!
//! Os pedidos ficam só em memória, valem 10 minutos e têm limites (total e por endereço), porque `Initiate` não exige login.

use super::auth::{CurrentCreds, CurrentUser, user_dto};
use super::shows::normalize_id;
use crate::{config::AppConfig, db::DbPool, users, users::DeviceInfo};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

/// Quanto tempo um código vale.
const VALIDADE: Duration = Duration::from_secs(600);
/// Pedidos pendentes ao mesmo tempo, no servidor todo, e por endereço de rede.
const MAX_PEDIDOS: usize = 100;
const MAX_POR_IP: usize = 5;

#[derive(Clone)]
pub struct Pedido {
    pub codigo: String,
    pub segredo: String,
    criado: Instant,
    pub data: String,
    pub dispositivo: DeviceInfo,
    ip: IpAddr,
    /// O usuário que autorizou o código.
    usuario: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recusa {
    /// O servidor já tem pedidos demais.
    Cheio,
    /// Esse endereço já tem pedidos demais.
    ExcessoDoEndereco,
}

#[derive(Clone)]
pub struct QuickConnect {
    pedidos: Arc<Mutex<Vec<Pedido>>>,
    validade: Duration,
}

impl Default for QuickConnect {
    fn default() -> Self {
        QuickConnect { pedidos: Arc::default(), validade: VALIDADE }
    }
}

/// `YYYY-MM-DDTHH:MM:SS.0000000Z` (o formato de data da API do Jellyfin) a partir de segundos desde 1970.
fn iso8601(segundos: u64) -> String {
    let dias = i64::try_from(segundos / 86_400).unwrap_or(0);
    let resto = segundos % 86_400;
    // Dias desde 1970-01-01 para data civil (algoritmo de Howard Hinnant).
    let z = dias + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dia = doy - (153 * mp + 2) / 5 + 1;
    let mes = if mp < 10 { mp + 3 } else { mp - 9 };
    let ano = yoe + era * 400 + i64::from(mes <= 2);
    format!("{ano:04}-{mes:02}-{dia:02}T{:02}:{:02}:{:02}.0000000Z", resto / 3600, resto % 3600 / 60, resto % 60)
}

impl QuickConnect {
    fn travar(&self) -> std::sync::MutexGuard<'_, Vec<Pedido>> {
        let mut pedidos = self.pedidos.lock().unwrap_or_else(PoisonError::into_inner);
        let validade = self.validade;
        pedidos.retain(|p| p.criado.elapsed() < validade);
        pedidos
    }

    #[cfg(test)]
    fn com_validade(validade: Duration) -> Self {
        QuickConnect { pedidos: Arc::default(), validade }
    }

    /// Cria um pedido com um código de seis dígitos que ainda não está em uso.
    pub fn iniciar(&self, ip: IpAddr, dispositivo: DeviceInfo) -> Result<Pedido, Recusa> {
        let mut pedidos = self.travar();
        if pedidos.len() >= MAX_PEDIDOS {
            return Err(Recusa::Cheio);
        }
        if pedidos.iter().filter(|p| p.ip == ip).count() >= MAX_POR_IP {
            return Err(Recusa::ExcessoDoEndereco);
        }
        let codigo = loop {
            let c = format!("{:06}", Uuid::new_v4().as_u128() % 1_000_000);
            if pedidos.iter().all(|p| p.codigo != c) {
                break c;
            }
        };
        let agora = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let pedido = Pedido {
            codigo,
            segredo: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            criado: Instant::now(),
            data: iso8601(agora),
            dispositivo,
            ip,
            usuario: None,
        };
        pedidos.push(pedido.clone());
        Ok(pedido)
    }

    pub fn consultar(&self, segredo: &str) -> Option<Pedido> {
        self.travar().iter().find(|p| p.segredo == segredo).cloned()
    }

    /// Autoriza o código para o usuário. `false` se o código não existe (ou já venceu).
    pub fn autorizar(&self, codigo: &str, usuario: &str) -> bool {
        let mut pedidos = self.travar();
        match pedidos.iter_mut().find(|p| p.codigo == codigo.trim()) {
            Some(p) => {
                p.usuario = Some(usuario.to_owned());
                true
            }
            None => false,
        }
    }

    /// Troca o segredo autorizado pelo usuário e apaga o pedido: cada código serve a um único login.
    pub fn consumir(&self, segredo: &str) -> Option<String> {
        let mut pedidos = self.travar();
        let posicao = pedidos.iter().position(|p| p.segredo == segredo && p.usuario.is_some())?;
        pedidos.remove(posicao).usuario
    }
}

fn resultado(p: &Pedido) -> Value {
    json!({
        "Authenticated": p.usuario.is_some(),
        "Secret": p.segredo,
        "Code": p.codigo,
        "DeviceId": p.dispositivo.device_id,
        "DeviceName": p.dispositivo.device,
        "AppName": p.dispositivo.client,
        "AppVersion": null,
        "DateAdded": p.data,
    })
}

type Estado = (Arc<AppConfig>, DbPool);

fn desligado() -> Response {
    (StatusCode::UNAUTHORIZED, "O Quick Connect está desligado neste servidor.").into_response()
}

/// `GET /QuickConnect/Enabled`.
pub async fn ativado(State((config, _)): State<Estado>) -> Json<bool> {
    Json(config.quick_connect)
}

/// `POST /QuickConnect/Initiate`: sem login. O app mostra o `Code` e guarda o `Secret`.
pub async fn iniciar(
    State((config, _)): State<Estado>,
    Extension(qc): Extension<QuickConnect>,
    ConnectInfo(endereco): ConnectInfo<SocketAddr>,
    CurrentCreds(creds): CurrentCreds,
) -> Response {
    if !config.quick_connect {
        return desligado();
    }
    match qc.iniciar(endereco.ip(), creds.device) {
        Ok(p) => Json(resultado(&p)).into_response(),
        Err(Recusa::Cheio) => {
            (StatusCode::SERVICE_UNAVAILABLE, "Há pedidos de Quick Connect demais. Tente em alguns minutos.").into_response()
        }
        Err(Recusa::ExcessoDoEndereco) => {
            (StatusCode::TOO_MANY_REQUESTS, "Pedidos demais deste aparelho. Tente em alguns minutos.").into_response()
        }
    }
}

/// `GET /QuickConnect/Connect?secret=...`: o app consulta até `Authenticated` ficar verdadeiro.
pub async fn conectar(
    State((config, _)): State<Estado>,
    Extension(qc): Extension<QuickConnect>,
    Query(consulta): Query<HashMap<String, String>>,
) -> Response {
    if !config.quick_connect {
        return desligado();
    }
    let segredo = consulta.iter().find(|(k, _)| k.eq_ignore_ascii_case("secret")).map_or("", |(_, v)| v.as_str());
    match qc.consultar(segredo) {
        Some(p) => Json(resultado(&p)).into_response(),
        None => (StatusCode::NOT_FOUND, "Pedido desconhecido ou vencido.").into_response(),
    }
}

/// Autoriza o `codigo` para `usuario`; o código desconhecido responde 404.
fn autorizar_para(qc: &QuickConnect, codigo: &str, usuario: &str) -> Response {
    if qc.autorizar(codigo, usuario) {
        Json(true).into_response()
    } else {
        (StatusCode::NOT_FOUND, "Código desconhecido ou vencido.").into_response()
    }
}

/// `POST /QuickConnect/Authorize?code=...&userId=...`: um aparelho logado autoriza o código para si mesmo; um
/// administrador pode autorizar para outro usuário (`userId`).
pub async fn autorizar(
    State((config, _)): State<Estado>,
    Extension(qc): Extension<QuickConnect>,
    CurrentUser(eu): CurrentUser,
    Query(consulta): Query<HashMap<String, String>>,
) -> Response {
    if !config.quick_connect {
        return desligado();
    }
    let valor = |chave: &str| consulta.iter().find(|(k, _)| k.eq_ignore_ascii_case(chave)).map(|(_, v)| v.as_str());
    let Some(codigo) = valor("code") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let alvo = valor("userId").map_or_else(|| eu.id.clone(), normalize_id);
    if alvo != eu.id && !eu.is_admin {
        return StatusCode::FORBIDDEN.into_response();
    }
    autorizar_para(&qc, codigo, &alvo)
}

/// `POST /admin/api/quickconnect/authorize` (painel, só rede local): `{ "code": "123456", "userId": "..." }`.
pub async fn autorizar_pelo_painel(
    State(st): State<super::admin::AdminState>,
    Extension(qc): Extension<QuickConnect>,
    Json(corpo): Json<Value>,
) -> Response {
    if !st.config.quick_connect {
        return desligado();
    }
    let pool = &st.pool;
    let codigo = corpo.get("code").and_then(Value::as_str).unwrap_or("");
    let usuario = normalize_id(corpo.get("userId").and_then(Value::as_str).unwrap_or(""));
    match users::get(pool, &usuario).await {
        Ok(Some(u)) if !u.is_disabled => autorizar_para(&qc, codigo, &usuario),
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `POST /Users/AuthenticateWithQuickConnect` com `{ "Secret": "..." }`: troca o segredo autorizado por um token.
pub async fn autenticar(
    State((config, pool)): State<Estado>,
    Extension(qc): Extension<QuickConnect>,
    CurrentCreds(creds): CurrentCreds,
    Json(corpo): Json<Value>,
) -> Response {
    if !config.quick_connect {
        return desligado();
    }
    let segredo = corpo.get("Secret").and_then(Value::as_str).unwrap_or("");
    let Some(id) = qc.consumir(segredo) else {
        return (StatusCode::UNAUTHORIZED, "O código ainda não foi autorizado, ou venceu.").into_response();
    };
    let usuario = match users::get(&pool, &id).await {
        Ok(Some(u)) if !u.is_disabled => u,
        Ok(_) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match users::create_token(&pool, &usuario.id, &creds.device).await {
        Ok(token) => Json(json!({
            "User": user_dto(&config, &usuario),
            "AccessToken": token,
            "ServerId": config.server_id,
        }))
        .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(ultimo: u8) -> IpAddr {
        IpAddr::from([192, 168, 1, ultimo])
    }

    #[test]
    fn data_no_formato_do_jellyfin() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.0000000Z");
        assert_eq!(iso8601(1_795_097_019), "2026-11-19T14:03:39.0000000Z");
        assert_eq!(iso8601(951_782_400 + 86_399), "2000-02-29T23:59:59.0000000Z");
    }

    #[test]
    fn codigo_de_seis_digitos_autorizacao_e_uso_unico() {
        let qc = QuickConnect::default();
        let p = qc.iniciar(ip(10), DeviceInfo::default()).unwrap();
        assert_eq!(p.codigo.len(), 6);
        assert!(p.codigo.chars().all(|c| c.is_ascii_digit()) && p.segredo.len() == 64);
        // Sem autorização, o segredo não vale nada.
        assert!(qc.consumir(&p.segredo).is_none());
        assert!(qc.consultar(&p.segredo).unwrap().usuario.is_none());
        assert!(!qc.autorizar("000000x", "u1"));
        assert!(qc.autorizar(&format!(" {} ", p.codigo), "u1"));
        assert_eq!(qc.consultar(&p.segredo).unwrap().usuario.as_deref(), Some("u1"));
        assert_eq!(qc.consumir("outro-segredo"), None);
        assert_eq!(qc.consumir(&p.segredo).as_deref(), Some("u1"));
        assert!(qc.consumir(&p.segredo).is_none(), "uso único");
        assert!(qc.consultar(&p.segredo).is_none());
    }

    #[test]
    fn pedidos_vencem() {
        let qc = QuickConnect::com_validade(Duration::from_millis(30));
        let p = qc.iniciar(ip(10), DeviceInfo::default()).unwrap();
        assert!(qc.consultar(&p.segredo).is_some());
        std::thread::sleep(Duration::from_millis(60));
        assert!(qc.consultar(&p.segredo).is_none());
        assert!(!qc.autorizar(&p.codigo, "u1"));
    }

    #[test]
    fn limites_por_endereco_e_no_total() {
        let qc = QuickConnect::default();
        for _ in 0..MAX_POR_IP {
            qc.iniciar(ip(10), DeviceInfo::default()).unwrap();
        }
        assert_eq!(qc.iniciar(ip(10), DeviceInfo::default()).err(), Some(Recusa::ExcessoDoEndereco));
        // Outro endereço ainda pode, até o teto do servidor.
        let mut feitos = MAX_POR_IP;
        let mut ultimo = 11_u8;
        while feitos < MAX_PEDIDOS {
            for _ in 0..MAX_POR_IP.min(MAX_PEDIDOS - feitos) {
                qc.iniciar(ip(ultimo), DeviceInfo::default()).unwrap();
                feitos += 1;
            }
            ultimo += 1;
        }
        assert_eq!(qc.iniciar(ip(250), DeviceInfo::default()).err(), Some(Recusa::Cheio));
    }

    #[test]
    fn codigos_pendentes_sao_unicos() {
        let qc = QuickConnect::default();
        let mut codigos = std::collections::HashSet::new();
        for n in 0..MAX_PEDIDOS {
            let p = qc.iniciar(IpAddr::from([10, 0, (n / 200) as u8, (n % 200) as u8]), DeviceInfo::default()).unwrap();
            assert!(codigos.insert(p.codigo));
        }
    }
}
