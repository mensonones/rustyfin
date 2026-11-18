//! Testes de integração: sobem o roteador real com um SQLite em arquivo temporário e fazem
//! requisições como um app Jellyfin (sem rede, via `tower::ServiceExt::oneshot`).

use axum::{
    body::{Body, Bytes, to_bytes},
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use rustyfin::{api, config::AppConfig, db, libraries::Library, scanner, users};
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
        max_transcodes: Some(1),
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
    let (status, bytes) = corpo(app, method, uri, token, body).await;
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn corpo(
    app: &axum::Router, method: Method, uri: &str, token: Option<&str>, body: Option<Value>,
) -> (StatusCode, Bytes) {
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
    (status, bytes)
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


#[tokio::test]
async fn mover_arquivo_preserva_id_posicao_e_favorito_sem_mesclar_copia() {
    let s = server("arquivo-movido").await;
    let pool = db::init_db(&test_config(&s.dir).database_url).await.unwrap();
    let midia = s.dir.join("midia");
    std::fs::create_dir_all(midia.join("destino")).unwrap();
    let origem = midia.join("Filme exemplo.mp4");
    let destino = midia.join("destino").join("Filme exemplo.mp4");
    std::fs::write(&origem, b"conteudo identico depois da mudanca").unwrap();
    let bibliotecas = [
        Library {
            id: "filmes".into(), name: "Filmes".into(), collection_type: "movies".into(),
            paths: vec![midia.to_str().unwrap().into()],
        },
        Library {
            id: "arquivo".into(), name: "Arquivo".into(), collection_type: "movies".into(),
            paths: vec![midia.join("destino").to_str().unwrap().into()],
        },
    ];
    assert_eq!(scanner::run_scan(&pool, &bibliotecas).await, 1);
    // A varredura adia o hash do conteúdo; em produção ele é calculado em segundo plano logo depois.
    scanner::completar_hashes(&pool).await;
    let id = scanner::item_id_for_path(origem.to_str().unwrap());
    let token = login(&s.app, "Admin").await;
    let posicao = 19_300_000_000_i64;
    let (status, _) = call(
        &s.app, Method::POST, "/Sessions/Playing/Progress", Some(&token),
        Some(json!({ "ItemId": id, "PositionTicks": posicao })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &s.app, Method::POST, &format!("/UserFavoriteItems/{id}"), Some(&token), None,
    ).await;
    assert!(status.is_success());

    std::fs::rename(&origem, &destino).unwrap();
    assert_eq!(scanner::run_scan(&pool, &bibliotecas).await, 1);
    let (status, item) = call(
        &s.app, Method::GET, &format!("/Items/{id}"), Some(&token), None,
    ).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item["Id"], id);
    assert_eq!(item["UserData"]["PlaybackPositionTicks"], posicao);
    assert_eq!(item["UserData"]["IsFavorite"], true);
    let biblioteca: String = sqlx::query_scalar("SELECT library_id FROM items WHERE id = ?")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(biblioteca, "arquivo");

    std::fs::copy(&destino, &origem).unwrap();
    assert_eq!(scanner::run_scan(&pool, &bibliotecas).await, 2);
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, path FROM items")
        .fetch_all(&pool).await.unwrap();
    assert!(rows.iter().any(|(i, p)| *i == id && std::path::Path::new(p) == destino));
    assert!(rows.iter().any(|(i, p)| *i != id && std::path::Path::new(p) == origem));
}


#[tokio::test]
async fn ajuste_srt_persiste_e_a_faixa_original_pode_ser_restaurada() {
    let s = server("ajuste-srt").await;
    let config = Arc::new(test_config(&s.dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    let legenda = s.dir.join("filme.srt");
    let original = concat!(
        "1\n00:00:01,000 --> 00:00:02,000\nPrimeiro\n\n",
        "2\n00:01:01,000 --> 00:01:02,000\nSegundo\n",
    );
    std::fs::write(&legenda, original).unwrap();
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, subtitle_path, content_sha256) \
         VALUES ('abc', 'Filme', '/media/filme.mp4', 'Movie', ?, x'0102030405060708')",
    ).bind(legenda.to_str().unwrap()).execute(&pool).await.unwrap();
    let (_, faixas) = call(&s.app, Method::GET, "/admin/api/legendas/abc", None, None).await;
    let indice = faixas[0]["indice"].as_i64().unwrap();
    let rota = format!("/admin/api/legendas/abc/{indice}");
    let (status, dados) = call(&s.app, Method::GET, &rota, None, None).await;
    assert_eq!(status, StatusCode::OK);
    let pedido = json!({
        "versao": dados["versao"],
        "ajuste": { "modo": "doisPontos", "originalAMs": 1000, "corrigidoAMs": 2000,
                     "originalBMs": 61000, "corrigidoBMs": 63000 }
    });
    let rota_previa = format!("{rota}/previa");
    let (_, previa) = call(&s.app, Method::POST, &rota_previa, None, Some(pedido.clone())).await;
    assert_eq!(previa["amostras"][0]["inicioCorrigidoMs"], 2000);
    assert_eq!(previa["amostras"][1]["inicioCorrigidoMs"], 63_000);
    let (status, _) = call(&s.app, Method::PUT, &rota, None, Some(pedido.clone())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let app = api::build_router(config, pool, Arc::new(tokio::sync::Notify::new()));
    let token = login(&app, "Admin").await;
    let stream = format!("/Videos/abc/abc/Subtitles/{indice}/0/Stream");
    let (status, bytes) = corpo(&app, Method::GET, &format!("{stream}.srt"), Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let texto = std::str::from_utf8(&bytes).unwrap();
    assert!(texto.contains("00:00:02,000 -->"));
    assert!(texto.contains("00:01:03,000 -->"));
    let (_, bytes) = corpo(&app, Method::GET, &format!("{stream}.vtt"), Some(&token), None).await;
    assert!(std::str::from_utf8(&bytes).unwrap().contains("00:01:03.000 -->"));
    assert_eq!(std::fs::read_to_string(&legenda).unwrap(), original);

    let trocada = original.replace("Segundo", "Substituído");
    std::fs::write(&legenda, &trocada).unwrap();
    let (status, _) = call(&app, Method::PUT, &rota, None, Some(pedido)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, bytes) = corpo(&app, Method::GET, &format!("{stream}.srt"), Some(&token), None).await;
    assert_eq!(bytes.as_ref(), trocada.as_bytes());

    std::fs::write(&legenda, original).unwrap();
    let (status, _) = call(&app, Method::DELETE, &rota, None, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, bytes) = corpo(&app, Method::GET, &format!("{stream}.srt"), Some(&token), None).await;
    assert_eq!(bytes.as_ref(), original.as_bytes());
}


#[tokio::test]
async fn revisao_de_substituicao_transfere_marcadores_sem_sobrescrever_posicao() {
    let s = server("revisao-acervo").await;
    let pool = db::init_db(&test_config(&s.dir).database_url).await.unwrap();
    let midia = s.dir.join("midia");
    std::fs::create_dir(&midia).unwrap();
    let caminho = midia.join("Filme (2024).mp4");
    std::fs::write(&caminho, b"edicao anterior").unwrap();
    let libs = [Library {
        id: "filmes".into(), name: "Filmes".into(), collection_type: "movies".into(),
        paths: vec![midia.to_str().unwrap().into()],
    }];
    assert_eq!(scanner::run_scan(&pool, &libs).await, 1);
    scanner::completar_hashes(&pool).await;
    let antigo = scanner::item_id_for_path(caminho.to_str().unwrap());
    let token = login(&s.app, "Admin").await;
    for rota in [format!("/UserFavoriteItems/{antigo}"), format!("/UserPlayedItems/{antigo}")] {
        let (status, _) = call(&s.app, Method::POST, &rota, Some(&token), None).await;
        assert!(status.is_success(), "{rota}: {status}");
    }

    std::fs::write(&caminho, b"outra edicao com duracao diferente").unwrap();
    assert_eq!(scanner::run_scan(&pool, &libs).await, 1);
    let (status, fila) = call(&s.app, Method::GET, "/admin/api/acervo/revisoes", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fila.as_array().unwrap().len(), 1);
    assert_eq!(fila[0]["id"], antigo);
    assert_eq!(fila[0]["motivo"], "substituido");
    let novo = fila[0]["destinos"][0]["itemId"].as_str().unwrap();
    assert_ne!(novo, antigo);

    let (status, _) = call(
        &s.app, Method::POST, "/Sessions/Playing/Progress", Some(&token),
        Some(json!({ "ItemId": novo, "PositionTicks": 50_000_000_i64 })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let rota = format!("/admin/api/acervo/revisoes/{antigo}");
    let (status, _) = call(
        &s.app, Method::POST, &rota, None,
        Some(json!({ "acao": "transferir", "itemId": "outro" })),
    ).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &s.app, Method::POST, &rota, None,
        Some(json!({ "acao": "transferir", "itemId": novo })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, item) = call(
        &s.app, Method::GET, &format!("/Items/{novo}"), Some(&token), None,
    ).await;
    assert_eq!(item["UserData"]["IsFavorite"], true);
    assert_eq!(item["UserData"]["Played"], true);
    assert_eq!(item["UserData"]["PlaybackPositionTicks"], 50_000_000_i64);
    assert_eq!(scanner::run_scan(&pool, &libs).await, 1);
    let (_, fila) = call(&s.app, Method::GET, "/admin/api/acervo/revisoes", None, None).await;
    assert_eq!(fila, json!([]));
}


#[tokio::test]
async fn retomada_mapeia_3210_para_3255_e_protege_posicao_atualizada() {
    let s = server("retomada").await;
    let pool = db::init_db(&test_config(&s.dir).database_url).await.unwrap();
    let token = login(&s.app, "Admin").await;
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, duration_ticks, content_sha256)
         VALUES ('nova', 'Edição nova', '/media/nova.mp4', 'Movie', 25000000000, X'01');
         INSERT INTO media_reviews (id, title, path, duration_ticks, reason) VALUES ('antiga', 'Edição antiga', '/media/antiga.mp4',
                                          24000000000, 'possivel_edicao');
         INSERT INTO media_review_targets VALUES ('antiga', '/media/nova.mp4', X'01');
         INSERT INTO media_review_state
         SELECT 'antiga', id, 0, 1, 19300000000 FROM users WHERE name = 'Admin';",
    ).execute(&pool).await.unwrap();
    let rota = "/admin/api/acervo/revisoes/antiga";
    let (status, previa) = call(
        &s.app, Method::POST, &format!("{rota}/previa"), None,
        Some(json!({
            "itemId": "nova",
            "mapa": { "anteriorA": 0, "novoA": 450_000_000_i64,
                      "anteriorB": 24_000_000_000_i64, "novoB": 24_450_000_000_i64 }
        })),
    ).await;
    assert_eq!(status, StatusCode::OK);
    let posicao = &previa["posicoes"][0];
    assert_eq!(posicao["sugerida"], 19_750_000_000_i64);
    let mut pedido = json!({
        "acao": "transferir", "itemId": "nova",
        "retomadas": [{ "perfilId": posicao["perfilId"], "posicaoTicks": posicao["sugerida"],
                        "posicaoEsperadaTicks": 0 }]
    });
    let (status, _) = call(
        &s.app, Method::POST, "/Sessions/Playing/Progress", Some(&token),
        Some(json!({ "ItemId": "nova", "PositionTicks": 50_000_000_i64 })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&s.app, Method::POST, rota, None, Some(pedido.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT);
    pedido["retomadas"][0]["posicaoEsperadaTicks"] = json!(50_000_000_i64);
    let (status, _) = call(&s.app, Method::POST, rota, None, Some(pedido)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, item) = call(&s.app, Method::GET, "/Items/nova", Some(&token), None).await;
    assert_eq!(item["UserData"]["PlaybackPositionTicks"], 19_750_000_000_i64);
    assert_eq!(item["UserData"]["IsFavorite"], true);
}


#[tokio::test]
async fn busca_de_cenas_respeita_progresso_e_biblioteca() {
    let s = server("cenas").await;
    let pool = db::init_db(&test_config(&s.dir).database_url).await.unwrap();
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, content_sha256, library_id)
         VALUES ('abc', 'Filme', '/media/filme.mkv', 'Movie', X'01', 'privada');
         INSERT INTO cenas_fontes VALUES ('abc', X'01', '', 0);
         INSERT INTO cenas_legenda VALUES ('abc', 1, 10000000, 20000000, 'Fala inicial', 'fala inicial'),
                                        ('abc', 1, 30000000, 40000000, 'Fala final', 'fala final');",
    ).execute(&pool).await.unwrap();
    let rota = "/api/cenas?q=FALA";
    let (status, _) = call(&s.app, Method::GET, rota, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let token = login(&s.app, "Admin").await;
    let (_, antes) = call(&s.app, Method::GET, rota, Some(&token), None).await;
    assert_eq!(antes, json!([]));
    call(
        &s.app, Method::POST, "/Sessions/Playing/Progress", Some(&token),
        Some(json!({"ItemId": "abc", "PositionTicks": 35_000_000_i64})),
    ).await;
    let (_, cenas) = call(&s.app, Method::GET, rota, Some(&token), None).await;
    assert_eq!(cenas.as_array().unwrap().len(), 1);
    assert_eq!(cenas[0]["previa"], "Fala inicial");

    let (_, usuario) = call(
        &s.app, Method::POST, "/Users/New", Some(&token), Some(json!({"Name": "Visitante"})),
    ).await;
    let id = usuario["Id"].as_str().unwrap();
    let (status, _) = call(
        &s.app, Method::POST, &format!("/Users/{id}/Policy"), Some(&token),
        Some(json!({"EnableAllFolders": false, "EnabledFolders": []})),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let visitante = login(&s.app, "Visitante").await;
    let sem_filtro = format!("{rota}&spoilers=true");
    let (_, cenas) = call(&s.app, Method::GET, &sem_filtro, Some(&visitante), None).await;
    assert_eq!(cenas, json!([]));
}

#[tokio::test]
async fn arquivo_movido_antes_do_hash_vai_para_a_revisao_em_vez_de_perder_o_historico() {
    let s = server("movido-antes-do-hash").await;
    let pool = db::init_db(&test_config(&s.dir).database_url).await.unwrap();
    let midia = s.dir.join("midia");
    std::fs::create_dir_all(midia.join("destino")).unwrap();
    let origem = midia.join("Filme exemplo (2020).mp4");
    let destino = midia.join("destino").join("Filme exemplo (2020).mp4");
    std::fs::write(&origem, b"conteudo do filme").unwrap();
    let libs = [Library {
        id: "filmes".into(), name: "Filmes".into(), collection_type: "movies".into(),
        paths: vec![midia.to_str().unwrap().into()],
    }];
    assert_eq!(scanner::run_scan(&pool, &libs).await, 1);
    let id = scanner::item_id_for_path(origem.to_str().unwrap());
    // movido antes de o hash em segundo plano terminar: não dá para casar por conteúdo
    std::fs::rename(&origem, &destino).unwrap();
    assert_eq!(scanner::run_scan(&pool, &libs).await, 1);
    let (status, fila) = call(&s.app, Method::GET, "/admin/api/acervo/revisoes", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fila.as_array().unwrap().len(), 1, "{fila}");
    assert_eq!(fila[0]["id"], id);
    assert_eq!(fila[0]["motivo"], "possivel_edicao");
}


#[tokio::test]
async fn ajuste_srt_espera_o_hash_do_video_em_vez_de_salvar_algo_que_seria_descartado() {
    let s = server("ajuste-srt-sem-hash").await;
    let config = Arc::new(test_config(&s.dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    let legenda = s.dir.join("filme.srt");
    std::fs::write(&legenda, "1\n00:00:01,000 --> 00:00:02,000\nPrimeiro\n").unwrap();
    // Item recém-varrido: o hash do conteúdo ainda não foi calculado em segundo plano.
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, subtitle_path) \
         VALUES ('semhash', 'Filme', '/media/filme.mp4', 'Movie', ?)",
    ).bind(legenda.to_str().unwrap()).execute(&pool).await.unwrap();
    let (_, faixas) = call(&s.app, Method::GET, "/admin/api/legendas/semhash", None, None).await;
    let indice = faixas[0]["indice"].as_i64().unwrap();
    let rota = format!("/admin/api/legendas/semhash/{indice}");
    let (status, _) = call(&s.app, Method::GET, &rota, None, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "sem o hash do vídeo não há assinatura estável");
    // O status do painel informa quantas assinaturas ainda estão pendentes.
    let (_, painel) = call(&s.app, Method::GET, "/admin/api/status", None, None).await;
    assert_eq!(painel["scan"]["signaturesPending"], 1);
    // Quando o hash é calculado, a mesma faixa passa a funcionar.
    sqlx::query("UPDATE items SET content_sha256 = x'0a0b0c0d' WHERE id = 'semhash'").execute(&pool).await.unwrap();
    let (status, _) = call(&s.app, Method::GET, &rota, None, None).await;
    assert_eq!(status, StatusCode::OK);
}
