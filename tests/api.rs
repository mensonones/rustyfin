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
        quick_connect: true,
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
async fn conteudo_trocado_no_mesmo_caminho_mantem_assistido_e_favorito_e_zera_a_retomada() {
    let s = server("troca-de-conteudo").await;
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
    let id = scanner::item_id_for_path(caminho.to_str().unwrap());
    let token = login(&s.app, "Admin").await;
    for rota in [format!("/UserFavoriteItems/{id}"), format!("/UserPlayedItems/{id}")] {
        let (status, _) = call(&s.app, Method::POST, &rota, Some(&token), None).await;
        assert!(status.is_success(), "{rota}: {status}");
    }
    let (status, _) = call(
        &s.app, Method::POST, "/Sessions/Playing/Progress", Some(&token),
        Some(json!({ "ItemId": id, "PositionTicks": 50_000_000_i64 })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    std::fs::write(&caminho, b"outra edicao com duracao diferente").unwrap();
    assert_eq!(scanner::run_scan(&pool, &libs).await, 1);
    let itens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items").fetch_one(&pool).await.unwrap();
    assert_eq!(itens, 1, "o item é o mesmo, sem fila de revisão");
    let (_, item) = call(&s.app, Method::GET, &format!("/Items/{id}"), Some(&token), None).await;
    assert_eq!(item["Id"], id);
    assert_eq!(item["UserData"]["IsFavorite"], true);
    assert_eq!(item["UserData"]["Played"], true);
    assert_eq!(item["UserData"]["PlaybackPositionTicks"], 0, "a posição não vale para outro conteúdo");
}


#[tokio::test]
async fn arquivo_movido_antes_do_hash_mantem_o_historico_sem_a_posicao() {
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
    let token = login(&s.app, "Admin").await;
    let (status, _) = call(&s.app, Method::POST, &format!("/UserFavoriteItems/{id}"), Some(&token), None).await;
    assert!(status.is_success());
    let (status, _) = call(
        &s.app, Method::POST, "/Sessions/Playing/Progress", Some(&token),
        Some(json!({ "ItemId": id, "PositionTicks": 50_000_000_i64 })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // movido antes de o hash em segundo plano terminar: não dá para casar por conteúdo, só pelo título
    std::fs::rename(&origem, &destino).unwrap();
    assert_eq!(scanner::run_scan(&pool, &libs).await, 1);
    let (caminho, itens): (String, i64) = (
        sqlx::query_scalar("SELECT path FROM items WHERE id = ?").bind(&id).fetch_one(&pool).await.unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM items").fetch_one(&pool).await.unwrap(),
    );
    assert_eq!(std::path::Path::new(&caminho), destino);
    assert_eq!(itens, 1);
    let (_, item) = call(&s.app, Method::GET, &format!("/Items/{id}"), Some(&token), None).await;
    assert_eq!(item["UserData"]["IsFavorite"], true);
    assert_eq!(item["UserData"]["PlaybackPositionTicks"], 0);
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

const LEGENDA_SRT: &str = concat!(
    "1\n00:00:10,000 --> 00:00:12,000\nO dragão acordou\n\n",
    "2\n00:30:00,000 --> 00:30:03,000\nEmpunhe a espada\n\n",
    "3\n01:00:00,000 --> 01:00:04,000\nO castelo caiu\n",
);

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
    item_com_legenda(&pool, &s.dir, id, LEGENDA_SRT).await;
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

    // Selos do título com legenda externa e assinatura.
    let (_, filme) = call(&s.app, Method::GET, &format!("/admin/api/acervo?busca={id}"), None, None).await;
    let selos = &filme["itens"][0]["selos"];
    assert_eq!((selos["legenda"].clone(), selos["assinatura"].clone()), (json!(true), json!(true)));

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
    item_com_legenda(&pool, &s.dir, id, LEGENDA_SRT).await;
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

#[tokio::test]
async fn segmentos_de_pular_vem_dos_capitulos_e_os_detalhes_trazem_os_capitulos() {
    let s = server("segmentos").await;
    let pool = db::init_db(&test_config(&s.dir).database_url).await.unwrap();
    let capitulos = json!([
        { "n": "Recap", "t": 0 }, { "n": "Abertura", "t": 600_000_000_i64 }, { "n": "Capítulo 3", "t": 1_500_000_000_i64 },
        { "n": "Credits", "t": 13_000_000_000_i64 },
    ]);
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, duration_ticks, chapters_json) VALUES
         ('ep1', 'Ep 1', '/m/ep1.mkv', 'Movie', 14000000000, ?),
         ('semcap', 'Sem capítulos', '/m/semcap.mkv', 'Movie', 14000000000, '[]'),
         ('antigo', 'Nunca relido', '/m/antigo.mkv', 'Movie', 14000000000, NULL)",
    )
    .bind(capitulos.to_string())
    .execute(&pool)
    .await
    .unwrap();
    let token = login(&s.app, "Admin").await;

    let (status, v) = call(&s.app, Method::GET, "/MediaSegments/ep1", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["TotalRecordCount"], 3);
    let tipos: Vec<_> = v["Items"].as_array().unwrap().iter().map(|i| i["Type"].as_str().unwrap().to_string()).collect();
    assert_eq!(tipos, ["Recap", "Intro", "Outro"]);
    assert_eq!(v["Items"][1]["StartTicks"], 600_000_000_i64);
    assert_eq!(v["Items"][1]["EndTicks"], 1_500_000_000_i64);
    assert_eq!(v["Items"][2]["EndTicks"], 14_000_000_000_i64, "o último vai até o fim do vídeo");
    assert_eq!(v["Items"][0]["ItemId"], "ep1");

    // Filtro por tipo, repetido e em qualquer caixa.
    let (_, so_intro) = call(&s.app, Method::GET, "/MediaSegments/ep1?includeSegmentTypes=intro", Some(&token), None).await;
    assert_eq!(so_intro["TotalRecordCount"], 1);
    let (_, dois) = call(&s.app, Method::GET, "/MediaSegments/ep1?includeSegmentTypes=Intro&includeSegmentTypes=Outro", Some(&token), None).await;
    assert_eq!(dois["TotalRecordCount"], 2);

    // Sem capítulos ou nunca inspecionado: lista vazia; item inexistente: 404; sem token: 401.
    for id in ["semcap", "antigo"] {
        let (status, v) = call(&s.app, Method::GET, &format!("/MediaSegments/{id}"), Some(&token), None).await;
        assert_eq!((status, v["TotalRecordCount"].clone()), (StatusCode::OK, json!(0)));
    }
    let (status, _) = call(&s.app, Method::GET, "/MediaSegments/naoexiste", Some(&token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&s.app, Method::GET, "/MediaSegments/ep1", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Os detalhes do item trazem os capítulos (nome e início); sem capítulos, o campo não existe.
    let (_, item) = call(&s.app, Method::GET, "/Items/ep1", Some(&token), None).await;
    assert_eq!(item["Chapters"].as_array().unwrap().len(), 4);
    assert_eq!(item["Chapters"][1]["Name"], "Abertura");
    assert_eq!(item["Chapters"][1]["StartPositionTicks"], 600_000_000_i64);
    let (_, sem) = call(&s.app, Method::GET, "/Items/semcap", Some(&token), None).await;
    assert!(sem.get("Chapters").is_none());
}

#[tokio::test]
async fn preferencias_de_audio_e_legenda_viram_as_faixas_padrao_do_playback_info() {
    use rustyfin::models::MediaStream;
    let s = server("preferencias").await;
    let pool = db::init_db(&test_config(&s.dir).database_url).await.unwrap();
    let faixa = |tipo: &str, index: i32, idioma: &str, padrao: bool| MediaStream {
        language: Some(idioma.to_string()),
        is_default: padrao,
        ..MediaStream::basic(if tipo == "Subtitle" { "subrip" } else { "aac" }, tipo, index)
    };
    let streams = serde_json::to_string(&vec![
        MediaStream::basic("h264", "Video", 0),
        faixa("Audio", 1, "eng", true),
        faixa("Audio", 2, "por", false),
        faixa("Subtitle", 3, "eng", false),
        faixa("Subtitle", 4, "por", false),
    ])
    .unwrap();
    sqlx::query(
        "INSERT INTO items (id, name, path, media_type, container, video_codec, audio_codec, streams_json)
         VALUES ('f', 'Filme', '/m/f.mp4', 'Movie', 'mp4', 'h264', 'aac', ?)",
    )
    .bind(streams)
    .execute(&pool)
    .await
    .unwrap();
    let token = login(&s.app, "Admin").await;
    let perfil = json!({ "DeviceProfile": { "DirectPlayProfiles": [
        { "Type": "Video", "Container": "mp4", "VideoCodec": "h264", "AudioCodec": "aac" }
    ] } });
    let indices = |v: &Value| (v["MediaSources"][0]["DefaultAudioStreamIndex"].clone(), v["MediaSources"][0]["DefaultSubtitleStreamIndex"].clone());

    // Sem preferências: o servidor não indica nada, e o app segue o arquivo.
    let (_, v) = call(&s.app, Method::POST, "/Items/f/PlaybackInfo", Some(&token), Some(perfil.clone())).await;
    assert_eq!(indices(&v), (Value::Null, Value::Null));

    // Valores inválidos são recusados e nada muda.
    for corpo in [json!({ "SubtitleMode": "Sempre" }), json!({ "AudioLanguagePreference": "portugues" })] {
        let (status, _) = call(&s.app, Method::POST, "/Users/Configuration", Some(&token), Some(corpo)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // O app grava as preferências (o objeto UserConfiguration do Jellyfin); a leitura devolve o que foi salvo.
    let (status, _) = call(
        &s.app, Method::POST, "/Users/Configuration", Some(&token),
        Some(json!({ "AudioLanguagePreference": "pt-BR", "PlayDefaultAudioTrack": false,
                     "SubtitleLanguagePreference": "eng", "SubtitleMode": "Always", "DisplayMissingEpisodes": true })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, eu) = call(&s.app, Method::GET, "/Users/Me", Some(&token), None).await;
    assert_eq!(eu["Configuration"]["AudioLanguagePreference"], "pt-br");
    assert_eq!(eu["Configuration"]["PlayDefaultAudioTrack"], false);
    assert_eq!(eu["Configuration"]["SubtitleMode"], "Always");
    let (_, v) = call(&s.app, Method::POST, "/Items/f/PlaybackInfo", Some(&token), Some(perfil.clone())).await;
    assert_eq!(indices(&v), (json!(2), json!(3)), "áudio em português e legenda em inglês");

    // Pelo painel: legenda inteligente em português com o áudio original (inglês, a faixa padrão).
    let id = eu["Id"].as_str().unwrap();
    let (status, _) = call(
        &s.app, Method::POST, &format!("/admin/api/users/{id}/playback"), None,
        Some(json!({ "audioLanguage": "", "playDefaultAudio": true, "subtitleLanguage": "por", "subtitleMode": "Smart" })),
    ).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, v) = call(&s.app, Method::POST, "/Items/f/PlaybackInfo", Some(&token), Some(perfil.clone())).await;
    assert_eq!(indices(&v), (Value::Null, json!(4)));
    let (_, lista) = call(&s.app, Method::GET, "/admin/api/users", None, None).await;
    let admin = lista.as_array().unwrap().iter().find(|u| u["id"] == id).unwrap();
    assert_eq!((admin["subtitleMode"].clone(), admin["subtitleLanguage"].clone()), (json!("Smart"), json!("por")));

    // Outro usuário não altera as preferências de quem não é ele (a não ser um administrador).
    let (_, novo) = call(&s.app, Method::POST, "/Users/New", Some(&token), Some(json!({ "Name": "Visita" }))).await;
    let visita = login(&s.app, "Visita").await;
    let alvo = novo["Id"].as_str().unwrap();
    let (status, _) = call(&s.app, Method::POST, &format!("/Users/{id}/Configuration"), Some(&visita), Some(json!({ "SubtitleMode": "None" }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&s.app, Method::POST, &format!("/Users/{alvo}/Configuration"), Some(&visita), Some(json!({ "SubtitleMode": "None" }))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn quick_connect_leva_o_codigo_de_um_aparelho_logado_ate_o_login_da_tv() {
    let s = server("quick-connect").await;
    // A TV ainda não tem login.
    let (_, ligado) = call(&s.app, Method::GET, "/QuickConnect/Enabled", None, None).await;
    assert_eq!(ligado, json!(true));
    let (status, pedido) = call(&s.app, Method::POST, "/QuickConnect/Initiate", None, None).await;
    assert_eq!(status, StatusCode::OK, "{pedido}");
    let (codigo, segredo) = (pedido["Code"].as_str().unwrap().to_string(), pedido["Secret"].as_str().unwrap().to_string());
    assert_eq!(codigo.len(), 6);
    assert_eq!(pedido["Authenticated"], false);
    assert_eq!(pedido["AppName"], "teste");
    assert!(pedido["DateAdded"].as_str().unwrap().ends_with('Z'));

    // Enquanto ninguém autoriza, a consulta mostra pendente e o segredo não troca por token.
    let consulta = format!("/QuickConnect/Connect?secret={segredo}");
    let (_, p) = call(&s.app, Method::GET, &consulta, None, None).await;
    assert_eq!(p["Authenticated"], false);
    let troca = json!({ "Secret": segredo });
    let (status, _) = call(&s.app, Method::POST, "/Users/AuthenticateWithQuickConnect", None, Some(troca.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&s.app, Method::GET, "/QuickConnect/Connect?secret=inventado", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Autorizar exige login; um código desconhecido é 404.
    let autorizar = format!("/QuickConnect/Authorize?code={codigo}");
    let (status, _) = call(&s.app, Method::POST, &autorizar, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let admin = login(&s.app, "Admin").await;
    let (status, _) = call(&s.app, Method::POST, "/QuickConnect/Authorize?code=000000", Some(&admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, ok) = call(&s.app, Method::POST, &autorizar, Some(&admin), None).await;
    assert_eq!((status, ok), (StatusCode::OK, json!(true)));

    let (_, p) = call(&s.app, Method::GET, &consulta, None, None).await;
    assert_eq!(p["Authenticated"], true);
    let (status, resultado) = call(&s.app, Method::POST, "/Users/AuthenticateWithQuickConnect", None, Some(troca.clone())).await;
    assert_eq!(status, StatusCode::OK, "{resultado}");
    assert_eq!(resultado["User"]["Name"], "Admin");
    let token = resultado["AccessToken"].as_str().unwrap().to_string();
    let (status, eu) = call(&s.app, Method::GET, "/Users/Me", Some(&token), None).await;
    assert_eq!((status, eu["Name"].clone()), (StatusCode::OK, json!("Admin")));
    // Cada código serve a um único login.
    let (status, _) = call(&s.app, Method::POST, "/Users/AuthenticateWithQuickConnect", None, Some(troca)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&s.app, Method::GET, &consulta, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn quick_connect_para_outro_usuario_so_por_administrador_ou_pelo_painel() {
    let s = server("quick-connect-outro").await;
    let admin = login(&s.app, "Admin").await;
    let (_, novo) = call(&s.app, Method::POST, "/Users/New", Some(&admin), Some(json!({ "Name": "Visita" }))).await;
    let visita_id = novo["Id"].as_str().unwrap().to_string();
    let visita = login(&s.app, "Visita").await;
    let (_, eu_admin) = call(&s.app, Method::GET, "/Users/Me", Some(&admin), None).await;
    let admin_id = eu_admin["Id"].as_str().unwrap().to_string();

    let novo_pedido = || async {
        let (_, p) = call(&s.app, Method::POST, "/QuickConnect/Initiate", None, None).await;
        (p["Code"].as_str().unwrap().to_string(), p["Secret"].as_str().unwrap().to_string())
    };
    // Um usuário comum não autoriza para outro.
    let (codigo, _) = novo_pedido().await;
    let (status, _) = call(&s.app, Method::POST, &format!("/QuickConnect/Authorize?code={codigo}&userId={admin_id}"), Some(&visita), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // O administrador autoriza para a Visita, e a TV entra como ela.
    let (codigo, segredo) = novo_pedido().await;
    let (status, _) = call(&s.app, Method::POST, &format!("/QuickConnect/Authorize?code={codigo}&userId={visita_id}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, r) = call(&s.app, Method::POST, "/Users/AuthenticateWithQuickConnect", None, Some(json!({ "Secret": segredo }))).await;
    assert_eq!(r["User"]["Name"], "Visita");
    // O painel (rede local) também autoriza, para o usuário escolhido; usuário ou código desconhecido é 404.
    let (codigo, segredo) = novo_pedido().await;
    let (status, _) = call(&s.app, Method::POST, "/admin/api/quickconnect/authorize", None, Some(json!({ "code": codigo, "userId": admin_id }))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, r) = call(&s.app, Method::POST, "/Users/AuthenticateWithQuickConnect", None, Some(json!({ "Secret": segredo }))).await;
    assert_eq!(r["User"]["Name"], "Admin");
    let (status, _) = call(&s.app, Method::POST, "/admin/api/quickconnect/authorize", None, Some(json!({ "code": "000000", "userId": admin_id }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (codigo, _) = novo_pedido().await;
    let (status, _) = call(&s.app, Method::POST, "/admin/api/quickconnect/authorize", None, Some(json!({ "code": codigo, "userId": "inexistente" }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Fora da rede local, o painel não autoriza.
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/admin/api/quickconnect/authorize")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "code": "000000", "userId": admin_id }).to_string()))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo("203.0.113.7:5000".parse::<SocketAddr>().unwrap()));
    assert_eq!(s.app.clone().oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn quick_connect_desligado_recusa_tudo_e_o_pedido_por_endereco_tem_limite() {
    let s = server_com("quick-connect-off", |c| c.quick_connect = false).await;
    let (_, ligado) = call(&s.app, Method::GET, "/QuickConnect/Enabled", None, None).await;
    assert_eq!(ligado, json!(false));
    let (status, _) = call(&s.app, Method::POST, "/QuickConnect/Initiate", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let s = server("quick-connect-limite").await;
    for _ in 0..5 {
        let (status, _) = call(&s.app, Method::POST, "/QuickConnect/Initiate", None, None).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _) = call(&s.app, Method::POST, "/QuickConnect/Initiate", None, None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

