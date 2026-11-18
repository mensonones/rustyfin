//! Metadados de anime pelo `AniList` (API GraphQL pública, sem chave nem conta).
//!
//! Só as séries das bibliotecas do tipo "anime" são consultadas, uma busca por título a cada 7 dias no máximo, e **só o
//! que falta** é preenchido: o que o TMDB já trouxe (por exemplo, a sinopse em português) não é trocado. O `AniList` não
//! entrega dados por episódio, então os episódios seguem como estão. Para não enviar títulos a um serviço externo, defina
//! `RUSTYFIN_ANILIST=0`; `RUSTYFIN_ANILIST_URL` troca o endereço (testes).

use crate::{
    config::AppConfig,
    db::DbPool,
    metadata::{self, ErroHttp, Extra},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tracing::{info, warn};

const URL_PADRAO: &str = "https://graphql.anilist.co";
/// Quanto esperar para consultar de novo uma série sem correspondência (ou ainda incompleta).
const REVALIDAR_SEGUNDOS: i64 = 7 * 24 * 3600;
/// Séries consultadas por varredura. O `AniList` aceita cerca de 90 pedidos por minuto.
const SERIES_POR_CICLO: usize = 40;
const ESPERA_ENTRE_PEDIDOS: Duration = Duration::from_millis(800);
/// Nota mínima do casamento por título para aceitar um resultado.
const PONTOS_MINIMOS: i32 = 40;

const CONSULTA: &str = "query ($busca: String) { Page(perPage: 10) { media(search: $busca, type: ANIME, \
    sort: SEARCH_MATCH) { id format seasonYear startDate { year } episodes description(asHtml: false) genres \
    averageScore synonyms title { romaji english native } coverImage { extraLarge large } bannerImage \
    studios(isMain: true) { nodes { name } } } } }";

/// Uma obra do `AniList`, já reduzida ao que o Rustyfin usa.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Obra {
    pub id: i64,
    /// Títulos e sinônimos (romaji, inglês, original e alternativos).
    pub titulos: Vec<String>,
    /// `TV`, `TV_SHORT`, `MOVIE`, `OVA`, `ONA`, `SPECIAL`...
    pub formato: Option<String>,
    pub ano: Option<i32>,
    pub sinopse: Option<String>,
    pub generos: Vec<String>,
    /// Nota de 0 a 10 (o `AniList` usa de 0 a 100).
    pub nota: Option<f64>,
    pub capa: Option<String>,
    pub banner: Option<String>,
    pub estudios: Vec<String>,
}

/// Minúsculas e só letras e números: "Sousou no Frieren!" e "sousou no frieren" ficam iguais.
fn normalizar(texto: &str) -> String {
    texto.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Tira o HTML que o `AniList` deixa na sinopse (`<br>`, `<i>`, entidades).
fn limpar_sinopse(bruto: &str) -> String {
    let mut sem_tags = String::with_capacity(bruto.len());
    let mut dentro = false;
    let mut tag = String::new();
    for c in bruto.chars() {
        match c {
            '<' => {
                dentro = true;
                tag.clear();
            }
            '>' if dentro => {
                dentro = false;
                if tag.trim_start_matches('/').to_lowercase().starts_with("br") {
                    sem_tags.push('\n');
                }
            }
            _ if dentro => tag.push(c),
            _ => sem_tags.push(c),
        }
    }
    let texto = sem_tags
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    texto.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_string()
}

fn texto(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(ToOwned::to_owned)
}

/// Lê a resposta da consulta (`data.Page.media`).
pub fn ler_resposta(v: &Value) -> Vec<Obra> {
    let Some(lista) = v.pointer("/data/Page/media").and_then(Value::as_array) else {
        return Vec::new();
    };
    lista
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_i64()?;
            let mut titulos: Vec<String> = ["romaji", "english", "native"]
                .iter()
                .filter_map(|k| m.pointer(&format!("/title/{k}")).and_then(texto))
                .collect();
            titulos.extend(m.get("synonyms").and_then(Value::as_array).into_iter().flatten().filter_map(texto));
            Some(Obra {
                id,
                titulos,
                formato: m.get("format").and_then(texto),
                ano: m
                    .get("seasonYear")
                    .and_then(Value::as_i64)
                    .or_else(|| m.pointer("/startDate/year").and_then(Value::as_i64))
                    .and_then(|a| i32::try_from(a).ok()),
                sinopse: m.get("description").and_then(texto).map(|d| limpar_sinopse(&d)).filter(|d| !d.is_empty()),
                generos: m
                    .get("genres")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(texto)
                    .collect(),
                nota: m.get("averageScore").and_then(Value::as_f64).map(|n| (n / 10.0 * 10.0).round() / 10.0),
                capa: m
                    .pointer("/coverImage/extraLarge")
                    .and_then(texto)
                    .or_else(|| m.pointer("/coverImage/large").and_then(texto)),
                banner: m.get("bannerImage").and_then(texto),
                estudios: m
                    .pointer("/studios/nodes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.get("name").and_then(texto))
                    .collect(),
            })
        })
        .collect()
}

/// Pontos do casamento entre o nome da série (a pasta) e uma obra: título igual vale 100, um contido no outro 40.
fn pontuar(nome: &str, ano: Option<i32>, obra: &Obra) -> i32 {
    let alvo = normalizar(nome);
    if alvo.is_empty() {
        return 0;
    }
    let mut pontos = 0;
    for titulo in &obra.titulos {
        let t = normalizar(titulo);
        if t.is_empty() {
            continue;
        }
        if t == alvo {
            pontos = pontos.max(100);
        } else if alvo.chars().count() >= 4 && t.chars().count() >= 4 && (t.contains(&alvo) || alvo.contains(&t)) {
            pontos = pontos.max(40);
        }
    }
    if pontos == 0 {
        return 0;
    }
    match (ano, obra.ano) {
        (Some(a), Some(b)) if a == b => pontos += 20,
        (Some(_), Some(_)) => pontos -= 15,
        _ => {}
    }
    if matches!(obra.formato.as_deref(), Some("TV" | "TV_SHORT" | "ONA")) {
        pontos += 5;
    }
    pontos
}

/// A obra que melhor casa com a série, ou nenhuma se nada passar do mínimo (melhor sem dados que com os de outro anime).
pub fn escolher<'a>(nome: &str, ano: Option<i32>, obras: &'a [Obra]) -> Option<&'a Obra> {
    let mut melhor: Option<(&Obra, i32)> = None;
    for obra in obras {
        let pontos = pontuar(nome, ano, obra);
        if pontos >= PONTOS_MINIMOS && melhor.is_none_or(|(_, p)| pontos > p) {
            melhor = Some((obra, pontos));
        }
    }
    melhor.map(|(o, _)| o)
}

fn agora() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

async fn buscar(url: &str, nome: &str) -> Result<Vec<Obra>, ErroHttp> {
    let corpo = json!({ "query": CONSULTA, "variables": { "busca": nome } });
    Ok(ler_resposta(&metadata::post_json(url, &corpo).await?))
}

/// Baixa uma imagem do `AniList` para o cache; devolve o caminho do arquivo.
async fn imagem(cache: &Path, obra: i64, tipo: &str, url: &str) -> Option<String> {
    let dir = cache.join("anilist");
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let destino = dir.join(format!("{obra}_{tipo}.jpg"));
    if tokio::fs::metadata(&destino).await.is_ok() || metadata::baixar_arquivo(url, &destino).await {
        Some(destino.to_string_lossy().into_owned())
    } else {
        None
    }
}

type SerieAtual = (Option<String>, Option<f64>, Option<String>, Option<i32>, Option<String>, Option<String>, Option<String>);

/// Grava na série só o que ainda falta e registra a consulta.
async fn gravar(pool: &DbPool, cache: &Path, serie: &str, obra: Option<&Obra>) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO series (id) VALUES (?)").bind(serie).execute(pool).await?;
    let Some(obra) = obra else {
        sqlx::query("UPDATE series SET anilist_checked_at = ? WHERE id = ?").bind(agora()).bind(serie).execute(pool).await?;
        return Ok(());
    };
    let (sinopse, nota, generos, ano, poster, fundo, extra): SerieAtual = sqlx::query_as(
        "SELECT overview, rating, genres, year, poster, backdrop, extra FROM series WHERE id = ?",
    )
    .bind(serie)
    .fetch_one(pool)
    .await?;

    let vazio = |t: &Option<String>| t.as_deref().is_none_or(|s| s.trim().is_empty());
    let sem_generos = generos
        .as_deref()
        .and_then(|g| serde_json::from_str::<Vec<String>>(g).ok())
        .is_none_or(|g| g.is_empty());
    let sinopse = if vazio(&sinopse) { obra.sinopse.clone() } else { sinopse };
    let nota = nota.or(obra.nota);
    let generos = if sem_generos && !obra.generos.is_empty() { serde_json::to_string(&obra.generos).ok() } else { generos };
    let ano = ano.or(obra.ano);
    let poster = if vazio(&poster) {
        match obra.capa.as_deref() {
            Some(url) => imagem(cache, obra.id, "capa", url).await,
            None => None,
        }
    } else {
        poster
    };
    let fundo = if vazio(&fundo) {
        match obra.banner.as_deref() {
            Some(url) => imagem(cache, obra.id, "banner", url).await,
            None => None,
        }
    } else {
        fundo
    };
    let mut extra_atual = extra.as_deref().and_then(|e| serde_json::from_str::<Extra>(e).ok()).unwrap_or_default();
    if extra_atual.studios.is_empty() && !obra.estudios.is_empty() {
        extra_atual.studios.clone_from(&obra.estudios);
        extra_atual.v = metadata::EXTRA_VERSION;
    }
    let extra = serde_json::to_string(&extra_atual).ok().or(extra);

    sqlx::query(
        "UPDATE series SET overview = ?, rating = ?, genres = ?, year = ?, poster = ?, backdrop = ?, extra = ?,
            anilist_id = ?, anilist_checked_at = ? WHERE id = ?",
    )
    .bind(sinopse)
    .bind(nota)
    .bind(generos)
    .bind(ano)
    .bind(poster)
    .bind(fundo)
    .bind(extra)
    .bind(obra.id)
    .bind(agora())
    .bind(serie)
    .execute(pool)
    .await?;
    Ok(())
}

/// Preenche os metadados que faltam nas séries das bibliotecas de anime. Chamada a cada varredura.
pub async fn enriquecer(pool: &DbPool, config: &AppConfig) {
    if std::env::var("RUSTYFIN_ANILIST").is_ok_and(|v| matches!(v.trim(), "0" | "false" | "off")) {
        return;
    }
    let url = std::env::var("RUSTYFIN_ANILIST_URL").unwrap_or_else(|_| URL_PADRAO.to_string());
    enriquecer_em(pool, &config.cache_dir, &url).await;
}

async fn enriquecer_em(pool: &DbPool, cache: &Path, url: &str) {
    let pendentes: Result<Vec<(String, String, Option<i32>)>, _> = sqlx::query_as(
        "SELECT i.series_id, MAX(i.series_name), MAX(i.production_year)
         FROM items i JOIN libraries l ON l.id = i.library_id
         LEFT JOIN series s ON s.id = i.series_id
         WHERE l.collection_type = 'anime' AND i.media_type = 'Episode'
           AND i.series_id IS NOT NULL AND i.series_name IS NOT NULL
           AND (s.anilist_checked_at IS NULL OR s.anilist_checked_at < ?)
           AND (s.id IS NULL OR s.anilist_id IS NULL OR s.overview IS NULL OR s.poster IS NULL OR s.backdrop IS NULL)
         GROUP BY i.series_id ORDER BY MAX(i.series_name) LIMIT ?",
    )
    .bind(agora() - REVALIDAR_SEGUNDOS)
    .bind(i64::try_from(SERIES_POR_CICLO).unwrap_or(40))
    .fetch_all(pool)
    .await;
    let pendentes = match pendentes {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => return,
        Err(e) => {
            warn!("AniList: falha ao listar séries de anime: {e}");
            return;
        }
    };
    info!("AniList: buscando metadados de {} séries de anime...", pendentes.len());
    let mut achadas = 0;
    for (n, (serie, nome, ano)) in pendentes.iter().enumerate() {
        if n > 0 {
            tokio::time::sleep(ESPERA_ENTRE_PEDIDOS).await;
        }
        let obras = match buscar(url, nome).await {
            Ok(o) => o,
            Err(ErroHttp::Limite) => {
                warn!("AniList: limite de pedidos atingido; as demais séries ficam para a próxima varredura.");
                return;
            }
            Err(ErroHttp::SemRede(e)) => {
                warn!("AniList: sem conexão, tentando na próxima varredura ({e})");
                return;
            }
            Err(ErroHttp::Status(s)) => {
                warn!("AniList: HTTP {s} para '{nome}'");
                // Marca como consultada: não insiste em um título que o serviço recusa.
                let _ = gravar(pool, cache, serie, None).await;
                continue;
            }
        };
        let obra = escolher(nome, *ano, &obras);
        achadas += usize::from(obra.is_some());
        if let Err(e) = gravar(pool, cache, serie, obra).await {
            warn!("AniList: falha ao salvar '{nome}': {e}");
        }
    }
    info!("AniList: {achadas} séries com correspondência.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resposta() -> Value {
        json!({ "data": { "Page": { "media": [
            { "id": 154_587, "format": "TV", "seasonYear": 2023, "startDate": { "year": 2023 }, "episodes": 28,
              "description": "A elfa <i>Frieren</i> &amp; amigos.<br><br>Fim.", "genres": ["Adventure", "Fantasy"],
              "averageScore": 91, "synonyms": ["Frieren"],
              "title": { "romaji": "Sousou no Frieren", "english": "Frieren: Beyond Journey's End", "native": "葬送のフリーレン" },
              "coverImage": { "extraLarge": "https://s4.anilist.co/capa.jpg", "large": "https://s4.anilist.co/c.jpg" },
              "bannerImage": "https://s4.anilist.co/banner.jpg",
              "studios": { "nodes": [ { "name": "Madhouse" } ] } },
            { "id": 2, "format": "MOVIE", "seasonYear": 2010, "title": { "romaji": "Frieren no Filme", "english": null, "native": null },
              "synonyms": [], "genres": [], "studios": { "nodes": [] } }
        ] } } })
    }

    #[test]
    fn le_a_resposta_do_anilist() {
        let obras = ler_resposta(&resposta());
        assert_eq!(obras.len(), 2);
        let f = &obras[0];
        assert_eq!(f.id, 154_587);
        assert_eq!(f.ano, Some(2023));
        assert_eq!(f.nota, Some(9.1));
        assert_eq!(f.generos, ["Adventure", "Fantasy"]);
        assert_eq!(f.estudios, ["Madhouse"]);
        assert_eq!(f.sinopse.as_deref(), Some("A elfa Frieren & amigos.\n\nFim."));
        assert!(f.titulos.contains(&"Frieren: Beyond Journey's End".to_string()) && f.titulos.contains(&"Frieren".to_string()));
        assert_eq!(f.capa.as_deref(), Some("https://s4.anilist.co/capa.jpg"));
        assert!(ler_resposta(&json!({ "errors": [] })).is_empty());
    }

    #[test]
    fn escolhe_pelo_titulo_e_pelo_ano_sem_aceitar_o_que_nao_casa() {
        let obras = ler_resposta(&resposta());
        // O título em inglês, sem pontuação e em outras letras, casa; o ano confirma.
        assert_eq!(escolher("Frieren - Beyond Journey's End", Some(2023), &obras).map(|o| o.id), Some(154_587));
        // Um sinônimo curto também casa.
        assert_eq!(escolher("frieren", None, &obras).map(|o| o.id), Some(154_587));
        // O ano diferente derruba a nota de um casamento parcial para abaixo do mínimo.
        assert_eq!(escolher("Sousou no", Some(1999), &obras), None);
        // Nada parecido: melhor sem dados do que com os de outro anime.
        assert_eq!(escolher("One Piece", None, &obras), None);
        assert_eq!(escolher("", None, &obras), None);
    }

    #[test]
    fn sinopse_sem_html() {
        assert_eq!(limpar_sinopse("Linha<br>dois <b>negrito</b> &quot;ok&quot;"), "Linha\ndois negrito \"ok\"");
    }

    /// Servidor HTTP mínimo: responde ao POST com `json`, e a qualquer GET com bytes de imagem.
    async fn servidor(json: String) -> String {
        let escuta = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let porta = escuta.local_addr().unwrap().port();
        // As imagens da resposta apontam para este mesmo servidor.
        let json = json.replace("{BASE}", &format!("http://127.0.0.1:{porta}"));
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut s, _)) = escuta.accept().await else { return };
                let json = json.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0_u8; 16384];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let pedido = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let (tipo, corpo) = if pedido.starts_with("POST") {
                        ("application/json", json.into_bytes())
                    } else {
                        ("image/jpeg", b"imagem".to_vec())
                    };
                    let cabecalho =
                        format!("HTTP/1.1 200 OK\r\nContent-Type: {tipo}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", corpo.len());
                    let _ = s.write_all(cabecalho.as_bytes()).await;
                    let _ = s.write_all(&corpo).await;
                });
            }
        });
        format!("http://127.0.0.1:{porta}")
    }

    #[tokio::test]
    async fn preenche_so_o_que_falta_na_serie_de_anime() {
        let base = std::env::temp_dir().join(format!("rustyfin-anilist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let pool = crate::db::init_db(&format!("sqlite://{}?mode=rwc", base.join("t.db").to_string_lossy().replace('\\', "/")))
            .await
            .unwrap();
        let cache = base.join("cache");

        sqlx::query("INSERT INTO libraries (id, name, collection_type) VALUES ('an', 'Anime', 'anime'), ('sr', 'Séries', 'tvshows')")
            .execute(&pool)
            .await
            .unwrap();
        for (id, lib, nome, serie) in [("e1", "an", "Frieren", "s-frieren"), ("e2", "sr", "Frieren", "s-comum")] {
            sqlx::query(
                "INSERT INTO items (id, name, path, media_type, library_id, series_id, series_name, season_number, episode_number)
                 VALUES (?, ?, ?, 'Episode', ?, ?, ?, 1, 1)",
            )
            .bind(id)
            .bind(nome)
            .bind(format!("/m/{id}.mkv"))
            .bind(lib)
            .bind(serie)
            .bind(nome)
            .execute(&pool)
            .await
            .unwrap();
        }
        // A sinopse (por exemplo, a do TMDB em português) já existe e não pode ser trocada.
        sqlx::query("INSERT INTO series (id, overview) VALUES ('s-frieren', 'Sinopse em português')")
            .execute(&pool)
            .await
            .unwrap();

        let url = servidor(resposta().to_string().replace("https://s4.anilist.co", "{BASE}")).await;
        enriquecer_em(&pool, &cache, &url).await;

        let (sinopse, nota, poster, fundo, anilist): (String, f64, Option<String>, Option<String>, i64) =
            sqlx::query_as("SELECT overview, rating, poster, backdrop, anilist_id FROM series WHERE id = 's-frieren'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sinopse, "Sinopse em português");
        assert!((nota - 9.1).abs() < 1e-9);
        assert_eq!(anilist, 154_587);
        assert_eq!(std::fs::read(poster.unwrap()).unwrap(), b"imagem");
        assert!(fundo.is_some());
        // Biblioteca de séries comuns: nunca consultada.
        let comum: Option<(String,)> = sqlx::query_as("SELECT id FROM series WHERE id = 's-comum'").fetch_optional(&pool).await.unwrap();
        assert!(comum.is_none());

        pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }
}
