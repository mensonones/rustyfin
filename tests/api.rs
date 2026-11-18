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
        anonymous_images: true,
        admin_password: None,
        public_url: None,
        trusted_proxies: Vec::new(),
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
    server_com(name, |_| {}).await
}

async fn server_com(name: &str, ajustar: impl FnOnce(&mut AppConfig)) -> Server {
    let dir = std::env::temp_dir().join(format!("rustyfin-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = test_config(&dir);
    ajustar(&mut config);
    let config = Arc::new(config);
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


/// Cria um item com vídeo e legenda SRT externa reais, já com hash e `content_mtime`, como depois da varredura e do cálculo
/// do hash em segundo plano.
async fn item_com_legenda(pool: &db::DbPool, dir: &std::path::Path, id: &str, legenda: &str) -> (PathBuf, PathBuf) {
    let video = dir.join(format!("{id}.mp4"));
    std::fs::write(&video, b"video").unwrap();
    let srt = dir.join(format!("{id}.srt"));
    std::fs::write(&srt, legenda).unwrap();
    let mtime = std::fs::metadata(&video)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string();
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, subtitle_path, content_sha256, content_mtime, library_id) \
         VALUES (?, 'Filme', ?, 'Movie', ?, x'0102030405060708', ?, 'lib1')",
    )
    .bind(id)
    .bind(video.to_str().unwrap())
    .bind(srt.to_str().unwrap())
    .bind(mtime)
    .execute(pool)
    .await
    .unwrap();
    (video, srt)
}

const LEGENDA_CENAS: &str = concat!(
    "1\n00:00:10,000 --> 00:00:12,000\nO dragão acordou\n\n",
    "2\n00:30:00,000 --> 00:30:03,000\nEmpunhe a espada\n\n",
    "3\n01:00:00,000 --> 01:00:04,000\nO castelo caiu\n",
);

#[tokio::test]
async fn indexar_cenas_le_o_srt_e_a_busca_acompanha_o_progresso() {
    let s = server("cenas-indexacao").await;
    let config = Arc::new(test_config(&s.dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    item_com_legenda(&pool, &s.dir, id, LEGENDA_CENAS).await;

    let (status, indice) = call(&s.app, Method::POST, &format!("/admin/api/cenas/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(indice["trechos"], 3);

    let token = login(&s.app, "Admin").await;
    let busca = |q: &'static str, spoilers: bool| {
        let uri = format!("/api/cenas?q={q}&spoilers={spoilers}");
        let (app, token) = (s.app.clone(), token.clone());
        async move { call(&app, Method::GET, &uri, Some(&token), None).await }
    };
    // Sem assistir nada, só quem libera os spoilers vê as falas.
    let (_, escondida) = busca("espada", false).await;
    assert_eq!(escondida.as_array().unwrap().len(), 0, "sem progresso nada é liberado");
    let (_, com_spoiler) = busca("espada", true).await;
    assert_eq!(com_spoiler.as_array().unwrap().len(), 1);
    assert_eq!(com_spoiler[0]["previa"], "Empunhe a espada");
    assert_eq!(com_spoiler[0]["inicioTicks"], 30 * 60 * 10_000_000_i64);
    // A busca ignora maiúsculas.
    let (_, maiuscula) = busca("ESPADA", true).await;
    assert_eq!(maiuscula.as_array().unwrap().len(), 1);

    // Assistido até os 40 min: a fala dos 30 min aparece, a dos 60 min não.
    let (status, _) = call(
        &s.app, Method::POST, "/Sessions/Playing/Progress", Some(&token),
        Some(json!({ "ItemId": id, "PositionTicks": 40 * 60 * 10_000_000_i64 })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(busca("espada", false).await.1.as_array().unwrap().len(), 1);
    assert_eq!(busca("dragão", false).await.1.as_array().unwrap().len(), 1);
    assert_eq!(busca("castelo", false).await.1.as_array().unwrap().len(), 0);

    // Título marcado como assistido: todas as falas ficam disponíveis.
    let (status, _) = call(&s.app, Method::POST, &format!("/UserPlayedItems/{id}"), Some(&token), None).await;
    assert!(status.is_success());
    assert_eq!(busca("castelo", false).await.1.as_array().unwrap().len(), 1);

    // Termos muito curtos são recusados.
    let (status, _) = call(&s.app, Method::GET, "/api/cenas?q=a", Some(&token), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // E a busca exige login.
    let (status, _) = call(&s.app, Method::GET, "/api/cenas?q=espada", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reindexar_cenas_substitui_o_indice_e_legenda_alterada_nao_devolve_o_antigo() {
    let s = server("cenas-reindexacao").await;
    let config = Arc::new(test_config(&s.dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    let id = "fedcba9876543210fedcba9876543210";
    let (_, srt) = item_com_legenda(&pool, &s.dir, id, LEGENDA_CENAS).await;
    let (status, _) = call(&s.app, Method::POST, &format!("/admin/api/cenas/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);

    let token = login(&s.app, "Admin").await;
    let (_, visivel) = call(&s.app, Method::GET, "/api/cenas?q=espada&spoilers=true", Some(&token), None).await;
    assert_eq!(visivel.as_array().unwrap().len(), 1);

    // Trocar a legenda invalida o índice: a busca não devolve o texto antigo até reindexar.
    std::fs::write(&srt, "1\n00:00:05,000 --> 00:00:06,000\nOutra fala\n").unwrap();
    let depois = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::OpenOptions::new().write(true).open(&srt).unwrap().set_modified(depois).unwrap();
    let (_, velho) = call(&s.app, Method::GET, "/api/cenas?q=espada&spoilers=true", Some(&token), None).await;
    assert_eq!(velho.as_array().unwrap().len(), 0, "legenda alterada não pode devolver o índice antigo");
    let (status, reindexado) = call(&s.app, Method::POST, &format!("/admin/api/cenas/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reindexado["trechos"], 1);
    let (_, nova) = call(&s.app, Method::GET, "/api/cenas?q=outra&spoilers=true", Some(&token), None).await;
    assert_eq!(nova.as_array().unwrap().len(), 1);
    let (_, antiga) = call(&s.app, Method::GET, "/api/cenas?q=espada&spoilers=true", Some(&token), None).await;
    assert_eq!(antiga.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn indexar_cenas_e_audio_exigem_titulo_existente_e_com_assinatura() {
    let s = server("indices-erros").await;
    let config = Arc::new(test_config(&s.dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    // Título que não existe.
    let (status, _) = call(&s.app, Method::POST, "/admin/api/cenas/00000000000000000000000000000000", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&s.app, Method::POST, "/admin/api/acervo/audio/00000000000000000000000000000000", None, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Título recém-varrido, sem o hash calculado: as duas ações esperam.
    let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let video = s.dir.join("novo.mp4");
    std::fs::write(&video, b"x").unwrap();
    let srt = s.dir.join("novo.srt");
    std::fs::write(&srt, LEGENDA_CENAS).unwrap();
    sqlx::query("INSERT INTO items (id, name, path, media_type, subtitle_path, library_id) VALUES (?, 'Novo', ?, 'Movie', ?, 'lib1')")
        .bind(id)
        .bind(video.to_str().unwrap())
        .bind(srt.to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = call(&s.app, Method::POST, &format!("/admin/api/cenas/{id}"), None, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&s.app, Method::POST, &format!("/admin/api/acervo/audio/{id}"), None, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn versoes_preparadas_comecam_vazias_e_nao_aceitam_chaves_desconhecidas() {
    let s = server("preparos").await;
    let (status, lista) = call(&s.app, Method::GET, "/admin/api/preparos", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lista["sugestoes"].as_array().unwrap().len(), 0);
    assert!(lista["limiteBytes"].as_u64().unwrap() > 0);
    let (status, _) = call(&s.app, Method::POST, "/admin/api/preparos/chave-que-nao-existe", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}


#[tokio::test]
async fn as_fontes_do_painel_sao_publicas_e_estaticas() {
    let s = server("fontes").await;
    for arquivo in ["geist-latin.woff2", "geist-mono-latin.woff2"] {
        let (status, bytes) = corpo(&s.app, Method::GET, &format!("/assets/fontes/{arquivo}"), None, None).await;
        assert_eq!(status, StatusCode::OK, "{arquivo}");
        assert_eq!(&bytes[..4], b"wOF2", "{arquivo} deve ser um woff2");
    }
    let (status, _) = corpo(&s.app, Method::GET, "/assets/fontes/outra-coisa.woff2", None, None).await;
    assert_ne!(status, StatusCode::OK, "só as duas fontes embutidas são servidas");
}

#[tokio::test]
async fn aba_acervo_lista_com_selos_filtros_busca_e_paginacao() {
    let s = server("acervo").await;
    let config = Arc::new(test_config(&s.dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    item_com_legenda(&pool, &s.dir, id, LEGENDA_CENAS).await;
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, series_name, season_number, episode_number, \
         library_id, probed) VALUES ('ep1', 'Piloto', '/x/serie/s01e01.mkv', 'Episode', 'Série X', 1, 1, 'lib2', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO libraries (id, name, collection_type) VALUES ('lib2', 'Séries', 'tvshows')")
        .execute(&pool)
        .await
        .unwrap();
    for n in 0..60 {
        sqlx::query("INSERT INTO items (id, name, path, media_type, library_id) VALUES (?, ?, ?, 'Movie', 'lib1')")
            .bind(format!("filme{n:02}"))
            .bind(format!("Lote {n:02}"))
            .bind(format!("/x/lote/{n}.mkv"))
            .execute(&pool)
            .await
            .unwrap();
    }

    // O painel exige o mesmo acesso das outras rotas de administração.
    let (status, lista) = call(&s.app, Method::GET, "/admin/api/acervo", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lista["total"], 62);
    assert_eq!(lista["porPagina"], 50);
    assert_eq!(lista["itens"].as_array().unwrap().len(), 50);
    assert_eq!(lista["resumo"]["filmes"], 61);
    assert_eq!(lista["resumo"]["episodios"], 1);
    assert_eq!(lista["resumo"]["semAssinatura"], 61);
    assert_eq!(lista["resumo"]["naoAnalisados"], 61);

    // Segunda página com o que sobrou.
    let (_, segunda) = call(&s.app, Method::GET, "/admin/api/acervo?pagina=2", None, None).await;
    assert_eq!(segunda["itens"].as_array().unwrap().len(), 12);

    // O tamanho da página vem do cliente (limitado), e a página fora do alcance volta vazia.
    let (_, curta) = call(&s.app, Method::GET, "/admin/api/acervo?porPagina=25&pagina=3", None, None).await;
    assert_eq!((curta["porPagina"].clone(), curta["itens"].as_array().unwrap().len()), (json!(25), 12));
    let (_, minimo) = call(&s.app, Method::GET, "/admin/api/acervo?porPagina=1", None, None).await;
    assert_eq!(minimo["porPagina"], 10);
    let (_, maximo) = call(&s.app, Method::GET, "/admin/api/acervo?porPagina=9999", None, None).await;
    assert_eq!(maximo["porPagina"], 200);
    assert_eq!(maximo["itens"].as_array().unwrap().len(), 62);
    let (_, alem) = call(&s.app, Method::GET, "/admin/api/acervo?pagina=99", None, None).await;
    assert_eq!(alem["itens"].as_array().unwrap().len(), 0);

    // Busca por nome, por série e por caminho, sem diferenciar maiúsculas.
    let (_, por_serie) = call(&s.app, Method::GET, "/admin/api/acervo?busca=S%C3%89RIE%20x", None, None).await;
    assert_eq!(por_serie["total"], 1);
    assert_eq!(por_serie["itens"][0]["nome"], "Piloto");
    assert_eq!(por_serie["itens"][0]["biblioteca"], "Séries");
    let (_, por_caminho) = call(&s.app, Method::GET, "/admin/api/acervo?busca=lote/7", None, None).await;
    assert_eq!(por_caminho["total"], 1);

    // Selos do título com legenda externa e assinatura, ainda sem cenas indexadas.
    let (_, filme) = call(&s.app, Method::GET, &format!("/admin/api/acervo?busca={id}"), None, None).await;
    let selos = &filme["itens"][0]["selos"];
    assert_eq!((selos["legenda"].clone(), selos["assinatura"].clone(), selos["cenas"].clone()),
        (json!(true), json!(true), json!(false)));
    let (_, indexado) = call(&s.app, Method::POST, &format!("/admin/api/cenas/{id}"), None, None).await;
    assert_eq!(indexado["trechos"], 3);
    let (_, filme) = call(&s.app, Method::GET, &format!("/admin/api/acervo?busca={id}"), None, None).await;
    assert_eq!(filme["itens"][0]["selos"]["cenas"], true);

    // A capa sai por uma rota do painel, sem token: usa a imagem local do título.
    let capa = s.dir.join("capa.jpg");
    std::fs::write(&capa, b"imagem-de-teste").unwrap();
    sqlx::query("UPDATE items SET poster_path = ? WHERE id = ?")
        .bind(capa.to_str().unwrap())
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let (status, bytes) = corpo(&s.app, Method::GET, &format!("/admin/api/acervo/{id}/capa"), None, None).await;
    assert_eq!((status, bytes.as_ref()), (StatusCode::OK, b"imagem-de-teste".as_ref()));
    let (status, _) = corpo(&s.app, Method::GET, "/admin/api/acervo/inexistente/capa", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Filtros de problema e de biblioteca.
    let (_, sem_assinatura) = call(&s.app, Method::GET, "/admin/api/acervo?problema=assinatura", None, None).await;
    assert_eq!(sem_assinatura["total"], 61);
    let (_, sem_legenda) = call(&s.app, Method::GET, "/admin/api/acervo?problema=legenda&biblioteca=lib2", None, None).await;
    assert_eq!(sem_legenda["total"], 1);
    let (status, _) = call(&s.app, Method::GET, "/admin/api/acervo?problema=inventado", None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&s.app, Method::GET, "/admin/api/acervo?biblioteca=nao-existe", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Requisição `GET` sem token, vinda do endereço informado.
async fn get_anonimo(app: &axum::Router, uri: &str, de: &str) -> StatusCode {
    let mut req = Request::builder().method(Method::GET).uri(uri).body(Body::empty()).unwrap();
    let addr: SocketAddr = format!("{de}:40000").parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn capas_abrem_para_a_rede_local_sem_token_mas_nao_para_fora_nem_com_a_opcao_desligada() {
    // O Moonfin na TV Samsung carrega as capas com <img>, que não envia token.
    let s = server("capas-anonimas").await;
    let config = Arc::new(test_config(&s.dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    item_com_legenda(&pool, &s.dir, id, LEGENDA_CENAS).await;
    let capa = s.dir.join("capa.jpg");
    std::fs::write(&capa, b"imagem-de-teste").unwrap();
    sqlx::query("UPDATE items SET poster_path = ? WHERE id = ?")
        .bind(capa.to_str().unwrap())
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let uri = format!("/Items/{id}/Images/Primary?maxWidth=0");

    assert_eq!(get_anonimo(&s.app, &uri, "192.168.1.50").await, StatusCode::OK, "rede local");

    // Cache: o cliente guarda a imagem por um dia e revalida com o ETag, que responde 304.
    let pedir = |etag: Option<String>| {
        let mut req = Request::builder().method(Method::GET).uri(&uri);
        if let Some(e) = etag {
            req = req.header(header::IF_NONE_MATCH, e);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo("192.168.1.50:40000".parse::<SocketAddr>().unwrap()));
        let app = s.app.clone();
        async move { app.oneshot(req).await.unwrap() }
    };
    let primeira = pedir(None).await;
    assert_eq!(primeira.headers()[header::CACHE_CONTROL], "private, max-age=86400");
    let etag = primeira.headers()[header::ETAG].to_str().unwrap().to_owned();
    let revalidada = pedir(Some(etag)).await;
    assert_eq!(revalidada.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(revalidada.headers()[header::CACHE_CONTROL], "private, max-age=86400");
    assert_eq!(get_anonimo(&s.app, "/Items/inexistente/Images/Backdrop/0", "192.168.1.50").await, StatusCode::NOT_FOUND);
    assert_eq!(get_anonimo(&s.app, &uri, "203.0.113.9").await, StatusCode::UNAUTHORIZED, "fora da rede local");
    // Só imagens: o resto continua exigindo token, mesmo da rede local.
    assert_eq!(get_anonimo(&s.app, &format!("/Items/{id}"), "192.168.1.50").await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        get_anonimo(&s.app, &format!("/Videos/{id}/stream"), "192.168.1.50").await,
        StatusCode::UNAUTHORIZED
    );

    // Com `RUSTYFIN_IMAGENS_ANONIMAS=false` as imagens voltam a exigir token.
    let mut fechada = test_config(&s.dir);
    fechada.anonymous_images = false;
    let app = api::build_router(Arc::new(fechada), pool.clone(), Arc::new(tokio::sync::Notify::new()));
    assert_eq!(get_anonimo(&app, &uri, "192.168.1.50").await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn desligar_so_aceita_o_proprio_computador() {
    // O ícone da bandeja do Windows pede o desligamento gracioso ao servidor; outro aparelho da rede não pode.
    let s = server("desligar").await;
    let pedir = |de: &str| {
        let mut req = Request::builder().method(Method::POST).uri("/admin/api/desligar").body(Body::empty()).unwrap();
        let addr: SocketAddr = format!("{de}:40000").parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        let app = s.app.clone();
        async move { app.oneshot(req).await.unwrap().status() }
    };
    assert_eq!(pedir("192.168.1.50").await, StatusCode::FORBIDDEN, "outro aparelho da rede local");
    assert_eq!(pedir("203.0.113.9").await, StatusCode::FORBIDDEN, "fora da rede local");
    assert_eq!(pedir("127.0.0.1").await, StatusCode::ACCEPTED, "o próprio computador");
}

#[tokio::test]
async fn legendas_desatualizadas_nao_gastam_o_limite_da_busca_de_cenas() {
    let s = server("cenas-limite").await;
    let config = Arc::new(test_config(&s.dir));
    let pool = db::init_db(&config.database_url).await.unwrap();
    // O título A tem 100 falas com o termo, todas antes das do B na ordem da busca, mas a legenda dele muda depois de
    // indexada. Antes, as 80 primeiras linhas eram todas do A, descartadas, e a busca devolvia zero resultados.
    let (a, b) = ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let mut legenda_a = String::new();
    for n in 0..100 {
        legenda_a.push_str(&format!("{}\n00:{:02}:00,000 --> 00:{:02}:02,000\nA espada {n}\n\n", n + 1, n % 60, n % 60));
    }
    let (_, srt_a) = item_com_legenda(&pool, &s.dir, a, &legenda_a).await;
    item_com_legenda(&pool, &s.dir, b, "1\n02:00:00,000 --> 02:00:03,000\nA espada do outro titulo\n").await;
    for id in [a, b] {
        let (status, _) = call(&s.app, Method::POST, &format!("/admin/api/cenas/{id}"), None, None).await;
        assert_eq!(status, StatusCode::OK);
    }
    let token = login(&s.app, "Admin").await;
    let (_, antes) = call(&s.app, Method::GET, "/api/cenas?q=espada&spoilers=true", Some(&token), None).await;
    assert_eq!(antes.as_array().unwrap().len(), 80, "com as duas legendas em dia o limite é de 80 falas");

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    std::fs::write(&srt_a, "1\n00:00:01,000 --> 00:00:02,000\nLegenda trocada\n").unwrap();
    let (_, depois) = call(&s.app, Method::GET, "/api/cenas?q=espada&spoilers=true", Some(&token), None).await;
    let depois = depois.as_array().unwrap();
    assert_eq!(depois.len(), 1, "as falas do título desatualizado saem, e as do outro título continuam");
    assert_eq!(depois[0]["previa"], "A espada do outro titulo");
}

#[tokio::test]
async fn biblioteca_de_anime_aparece_como_serie_para_os_apps() {
    let s = server("anime").await;
    let pasta = s.dir.join("animes");
    std::fs::create_dir_all(&pasta).unwrap();
    let uri = format!(
        "/Library/VirtualFolders?name=Animes&collectionType=anime&paths={}",
        pasta.to_string_lossy().replace('\\', "/").replace(' ', "%20")
    );
    let (status, _) = call(&s.app, Method::POST, &uri, None, None).await;
    assert!(status.is_success(), "criar a biblioteca de anime: {status}");

    // O painel enxerga o tipo de verdade; os apps do Jellyfin só conhecem "tvshows".
    let (_, pastas) = call(&s.app, Method::GET, "/Library/VirtualFolders", None, None).await;
    let anime = pastas.as_array().unwrap().iter().find(|p| p["Name"] == "Animes").unwrap();
    assert_eq!(anime["RustyfinType"], "anime");
    assert_eq!(anime["CollectionType"], "tvshows");

    let token = login(&s.app, "Admin").await;
    let (_, vistas) = call(&s.app, Method::GET, "/UserViews", Some(&token), None).await;
    let vista = vistas["Items"].as_array().unwrap().iter().find(|v| v["Name"] == "Animes").unwrap();
    assert_eq!(vista["CollectionType"], "tvshows");
}

/// GET como se a conexão viesse de `peer`, com cabeçalhos extras (os de um proxy reverso).
async fn get_de(app: &axum::Router, uri: &str, peer: &str, cabecalhos: &[(&str, &str)]) -> StatusCode {
    let mut req = Request::builder().method(Method::GET).uri(uri);
    for (k, v) in cabecalhos {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn proxy_nao_configurado_nao_abre_o_painel_para_a_internet() {
    let s = server("proxy-aberto").await;
    // Acesso direto de localhost e da rede local: o painel abre.
    assert_eq!(get_de(&s.app, "/admin/api/status", "127.0.0.1:5000", &[]).await, StatusCode::OK);
    assert_eq!(get_de(&s.app, "/admin/api/status", "192.168.1.9:5000", &[]).await, StatusCode::OK);
    // O nginx no mesmo PC encaminha um visitante da internet, e ninguém configurou RUSTYFIN_TRUSTED_PROXIES.
    let de_fora = [("x-forwarded-for", "203.0.113.7")];
    assert_eq!(get_de(&s.app, "/admin/api/status", "127.0.0.1:5000", &de_fora).await, StatusCode::FORBIDDEN);
    // Forjar o cabeçalho a partir da rede local só piora a situação de quem forja.
    assert_eq!(get_de(&s.app, "/admin/api/status", "192.168.1.9:5000", &[("x-real-ip", "192.168.1.1")]).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn proxy_confiavel_repassa_o_ip_real_do_cliente() {
    let s = server_com("proxy-confiavel", |c| {
        c.trusted_proxies = rustyfin::proxy::ler_lista("192.168.1.10, 172.16.0.0/12").0;
    })
    .await;
    let proxy = "192.168.1.10:5000";
    // Visitante da internet: sem painel.
    assert_eq!(get_de(&s.app, "/admin/api/status", proxy, &[("x-forwarded-for", "203.0.113.7")]).await, StatusCode::FORBIDDEN);
    // Mesmo com um IP "da rede local" forjado à esquerda, vale o que o proxy viu.
    let forjado = [("x-forwarded-for", "192.168.1.1, 203.0.113.7")];
    assert_eq!(get_de(&s.app, "/admin/api/status", proxy, &forjado).await, StatusCode::FORBIDDEN);
    // Cliente da rede local que passa pelo proxy: o painel abre.
    assert_eq!(get_de(&s.app, "/admin/api/status", proxy, &[("x-forwarded-for", "192.168.1.77")]).await, StatusCode::OK);
    // O proxy confiável que não informa o cliente: falha fechada.
    assert_eq!(get_de(&s.app, "/admin/api/status", proxy, &[("x-forwarded-proto", "https")]).await, StatusCode::FORBIDDEN);
    // Quem não está na lista continua sem poder informar nada.
    assert_eq!(
        get_de(&s.app, "/admin/api/status", "192.168.1.99:5000", &[("x-forwarded-for", "192.168.1.77")]).await,
        StatusCode::FORBIDDEN
    );
    // Acesso direto (sem proxy) segue funcionando.
    assert_eq!(get_de(&s.app, "/admin/api/status", "127.0.0.1:5000", &[]).await, StatusCode::OK);
}

#[tokio::test]
async fn login_sem_senha_so_vale_para_a_rede_local_mesmo_atras_do_proxy() {
    let s = server_com("proxy-login", |c| {
        c.trusted_proxies = rustyfin::proxy::ler_lista("192.168.1.10").0;
    })
    .await;
    let entrar = |cliente: &'static str| {
        let app = s.app.clone();
        async move {
            let corpo = json!({ "Username": "Admin", "Pw": "" }).to_string();
            let mut req = Request::builder()
                .method(Method::POST)
                .uri("/Users/AuthenticateByName")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, r#"MediaBrowser Client="t", Device="d", DeviceId="i", Version="1""#)
                .header("x-forwarded-for", cliente)
                .body(Body::from(corpo))
                .unwrap();
            req.extensions_mut().insert(ConnectInfo("192.168.1.10:5000".parse::<SocketAddr>().unwrap()));
            app.oneshot(req).await.unwrap().status()
        }
    };
    assert_eq!(entrar("192.168.1.30").await, StatusCode::OK, "da rede local, sem senha");
    assert_ne!(entrar("203.0.113.7").await, StatusCode::OK, "da internet, o usuário sem senha não entra");
}
