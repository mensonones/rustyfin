//! Testes de integração: sobem o roteador real com um SQLite em arquivo temporário e fazem
//! requisições como um app Jellyfin (sem rede, via `tower::ServiceExt::oneshot`).

use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use rustyfin::{api, config::AppConfig, db, users};
use serde_json::{Value, json};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tower::ServiceExt;

struct Server {
    app: axum::Router,
    dir: PathBuf,
}

fn test_config(dir: &std::path::Path) -> AppConfig {
    AppConfig {
        server_id: "a1b2c3d4e5f6478980123456789abcdef".into(),
        server_name: "Servidor de Teste".into(),
        version: "10.9.0".into(),
        host: "127.0.0.1".into(),
        port: 0,
        database_url: format!("sqlite://{}?mode=rwc", dir.join("test.db").display()),
        media_directories: Vec::new(),
        cache_dir: dir.join("cache"),
        scan_interval_secs: 0,
        watch_enabled: false,
        discovery_enabled: false,
        admin_password: None,
        public_url: None,
        ffmpeg_path: "ffmpeg".into(),
        max_transcodes: 1,
        video_encoder: "libx264".into(),
        tmdb_api_key: None,
        tmdb_language: "pt-BR".into(),
        tmdb_base_url: "http://127.0.0.1:9/3".into(),
        tmdb_image_base: "http://127.0.0.1:9/t/p".into(),
    }
}

async fn server(name: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("rustyfin-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = Arc::new(test_config(&dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    users::ensure_seed_admin(&pool).await.unwrap();
    let app = api::build_router(config, pool, Arc::new(tokio::sync::Notify::new()));
    Server { app, dir }
}

async fn call(app: &axum::Router, method: Method, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let mut auth = String::from(r#"MediaBrowser Client="teste", Device="it", DeviceId="it1", Version="1""#);
    if let Some(t) = token {
        auth.push_str(&format!(r#", Token="{t}""#));
    }
    req = req.header(header::AUTHORIZATION, auth);
    let body = match body {
        Some(b) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let mut req = req.body(body).unwrap();
    let addr: SocketAddr = "192.168.1.50:40000".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn login(app: &axum::Router, name: &str) -> String {
    let (st, v) = call(app, Method::POST, "/Users/AuthenticateByName", None, Some(json!({ "Username": name, "Pw": "" }))).await;
    assert_eq!(st, StatusCode::OK, "login de {name}: {v}");
    v["AccessToken"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn public_info_is_open_and_names_the_server() {
    let s = server("info").await;
    let (st, v) = call(&s.app, Method::GET, "/System/Info/Public", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["ProductName"], "Rustyfin");
    assert!(v["Id"].is_string());
}

#[tokio::test]
async fn library_routes_need_a_token() {
    let s = server("auth").await;
    let (st, _) = call(&s.app, Method::GET, "/Items?Recursive=true", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(&s.app, Method::GET, "/Items?Recursive=true", Some("token-que-nao-existe"), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let token = login(&s.app, "Admin").await;
    let (st, v) = call(&s.app, Method::GET, "/Items?Recursive=true", Some(&token), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["TotalRecordCount"], 0);
}

#[tokio::test]
async fn hidden_profiles_leave_the_login_list_but_can_still_sign_in() {
    let s = server("hidden").await;
    let admin = login(&s.app, "Admin").await;
    let (st, user) = call(&s.app, Method::POST, "/Users/New", Some(&admin), Some(json!({ "Name": "Visita" }))).await;
    assert_eq!(st, StatusCode::OK, "{user}");
    let id = user["Id"].as_str().unwrap().to_string();

    let names = |v: &Value| -> Vec<String> { v.as_array().unwrap().iter().map(|u| u["Name"].as_str().unwrap().to_string()).collect() };
    let (_, list) = call(&s.app, Method::GET, "/Users/Public", None, None).await;
    assert!(names(&list).contains(&"Visita".to_string()));

    let (st, _) = call(&s.app, Method::POST, &format!("/Users/{id}/Policy"), Some(&admin), Some(json!({ "IsHidden": true }))).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, list) = call(&s.app, Method::GET, "/Users/Public", None, None).await;
    assert!(!names(&list).contains(&"Visita".to_string()));
    login(&s.app, "Visita").await; // continua entrando digitando o nome
}

#[tokio::test]
async fn revoked_sessions_stop_working_and_tokens_never_leak() {
    let s = server("sessions").await;
    let admin = login(&s.app, "Admin").await;
    let phone = login(&s.app, "Admin").await;

    let (st, list) = call(&s.app, Method::GET, "/admin/api/sessions", None, None).await;
    assert_eq!(st, StatusCode::OK);
    let sessions = list.as_array().unwrap();
    assert!(sessions.len() >= 2);
    assert!(!list.to_string().contains(&phone), "a listagem não pode expor tokens");

    let id = sessions[0]["id"].as_i64().unwrap();
    let (st, _) = call(&s.app, Method::DELETE, &format!("/admin/api/sessions/{id}"), None, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    // exatamente uma das duas sessões deixou de valer
    let alive = [&admin, &phone];
    let mut ok = 0;
    for t in alive {
        if call(&s.app, Method::GET, "/UserViews", Some(t), None).await.0 == StatusCode::OK {
            ok += 1;
        }
    }
    assert_eq!(ok, 1);
}


#[tokio::test]
async fn midia_protegida_rejeita_requisicoes_sem_token() {
    let s = server("midia").await;
    let token = login(&s.app, "Admin").await;
    for rota in [
        "/Items/abc/Images/Primary",
        "/Items/abc/Images/Backdrop/0",
        "/Videos/abc/hls/0123456789abcdef/seg_00000.ts",
        "/Videos/abc/hls/0123456789abcdef/init.mp4",
    ] {
        let (status, _) = call(&s.app, Method::GET, rota, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{rota}");
        let com_token = format!("{rota}?api_key={token}");
        let (status, _) = call(&s.app, Method::GET, &com_token, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{rota}");
    }
    let (status, _) = call(&s.app, Method::GET, "/Users/abc/Images/Primary", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}


#[tokio::test]
async fn diagnostico_usa_perfil_observado_sem_alterar_a_ultima_decisao() {
    let s = server("diagnostico").await;
    let pool = db::init_db(&test_config(&s.dir).database_url).await.unwrap();
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, container, video_codec, audio_codec) \
         VALUES ('a', 'Filme exemplo', '/media/a.mp4', 'Movie', 'mp4', 'h264', 'aac')",
    )
    .execute(&pool).await.unwrap();
    let (_, perfis) = call(&s.app, Method::GET, "/admin/api/reproducao/perfis", None, None).await;
    assert_eq!(perfis, json!([]));
    let token = login(&s.app, "Admin").await;
    let pedido = json!({ "DeviceProfile": { "DirectPlayProfiles": [
        { "Type": "Video", "Container": "mp4", "VideoCodec": "h264", "AudioCodec": "aac" }
    ] } });
    let (status, playback) = call(&s.app, Method::POST, "/Items/a/PlaybackInfo", Some(&token), Some(pedido)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(playback["MediaSources"][0]["SupportsDirectPlay"], true);
    let (_, titulos) = call(&s.app, Method::GET, "/admin/api/reproducao/titulos?busca=exemplo", None, None).await;
    assert_eq!(titulos[0]["id"], "a");
    let (_, perfis) = call(&s.app, Method::GET, "/admin/api/reproducao/perfis", None, None).await;
    assert_eq!(perfis.as_array().unwrap().len(), 1);
    assert!(!perfis.to_string().contains(&token));
    let perfil = perfis[0]["id"].clone();
    let rota = "/admin/api/reproducao/simular";
    let pedido = json!({ "itemId": "a", "perfis": [perfil] });
    let (status, resultado) = call(&s.app, Method::POST, rota, None, Some(pedido.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resultado["resultados"][0]["resultado"]["modo"], "direct_play");
    assert_eq!(resultado["faixas"][0]["codec"], "aac");
    let mut invalido = pedido;
    invalido["legendaIndex"] = json!(999);
    let (status, _) = call(&s.app, Method::POST, rota, None, Some(invalido)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, depois) = call(&s.app, Method::GET, "/admin/api/reproducao/perfis", None, None).await;
    assert_eq!(depois, perfis);
}
