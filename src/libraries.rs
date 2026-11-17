//! Bibliotecas de mídia (Filmes/Séries) e suas pastas, persistidas no banco.
//!
//! No primeiro uso (banco sem bibliotecas) são criadas "Filmes" e "Séries" apontando para as pastas
//! de `RUSTYFIN_MEDIA_DIRS`; depois disso o banco é a fonte da verdade e tudo é editável pelo
//! painel `/admin` ou pelas rotas `/Library/VirtualFolders` (compatíveis com o Jellyfin).

use crate::{config::AppConfig, db::DbPool};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use uuid::Uuid;

/// IDs fixos das bibliotecas criadas por padrão (mantêm os links dos clientes já configurados).
pub const MOVIES_LIBRARY_ID: &str = "f137a2dd21bbc1b99aa5c0f6bf02a805";
pub const SERIES_LIBRARY_ID: &str = "7b2a4f1e9c3d4a58b6e1d0f3a9c52e47";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Library {
    pub id: String,
    pub name: String,
    /// "movies" ou "tvshows"
    pub collection_type: String,
    pub paths: Vec<String>,
}

#[derive(Debug)]
pub enum LibError {
    EmptyName,
    InvalidType,
    NameExists,
    NotFound,
    PathExists,
    InvalidPath(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for LibError {
    fn from(e: sqlx::Error) -> Self {
        LibError::Db(e)
    }
}

impl std::fmt::Display for LibError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LibError::EmptyName => write!(f, "O nome da biblioteca não pode ser vazio."),
            LibError::InvalidType => write!(f, "Tipo inválido: use movies ou tvshows."),
            LibError::NameExists => write!(f, "Já existe uma biblioteca com esse nome."),
            LibError::NotFound => write!(f, "Biblioteca ou pasta não encontrada."),
            LibError::PathExists => write!(f, "Essa pasta já está na biblioteca."),
            LibError::InvalidPath(m) => write!(f, "{m}"),
            LibError::Db(e) => write!(f, "Erro de banco de dados: {e}"),
        }
    }
}

/// Aceita os nomes de tipo do Jellyfin; só estes dois são suportados.
pub fn normalize_type(raw: &str) -> Option<&'static str> {
    match raw.trim().to_lowercase().as_str() {
        "movies" | "movie" => Some("movies"),
        "tvshows" | "tvshow" | "series" => Some("tvshows"),
        _ => None,
    }
}

/// Caminho absoluto, existente e que seja pasta. Devolve a forma normalizada (sem `/` final).
pub fn validate_path(raw: &str) -> Result<String, LibError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LibError::InvalidPath("Informe o caminho da pasta.".into()));
    }
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err(LibError::InvalidPath(format!("O caminho precisa ser absoluto: {raw}")));
    }
    if !p.exists() {
        return Err(LibError::InvalidPath(format!("A pasta não existe: {raw}")));
    }
    if !p.is_dir() {
        return Err(LibError::InvalidPath(format!("Não é uma pasta: {raw}")));
    }
    let normalized: PathBuf = p.components().collect();
    Ok(normalized.to_string_lossy().to_string())
}

pub async fn list(pool: &DbPool) -> Result<Vec<Library>, sqlx::Error> {
    let libs: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, name, collection_type FROM libraries ORDER BY name COLLATE NOCASE")
            .fetch_all(pool)
            .await?;
    let paths: Vec<(String, String)> =
        sqlx::query_as("SELECT library_id, path FROM library_paths ORDER BY path")
            .fetch_all(pool)
            .await?;
    Ok(libs
        .into_iter()
        .map(|(id, name, collection_type)| Library {
            paths: paths.iter().filter(|(lid, _)| *lid == id).map(|(_, p)| p.clone()).collect(),
            id,
            name,
            collection_type,
        })
        .collect())
}

pub async fn get_by_name(pool: &DbPool, name: &str) -> Result<Option<Library>, sqlx::Error> {
    Ok(list(pool)
        .await?
        .into_iter()
        .find(|l| l.name.eq_ignore_ascii_case(name.trim())))
}

pub async fn get_by_id(pool: &DbPool, id: &str) -> Result<Option<Library>, sqlx::Error> {
    Ok(list(pool).await?.into_iter().find(|l| l.id == id))
}

pub async fn create(
    pool: &DbPool,
    name: &str,
    collection_type: &str,
    paths: &[String],
) -> Result<Library, LibError> {
    create_with_id(pool, &Uuid::new_v4().simple().to_string(), name, collection_type, paths).await
}

async fn create_with_id(
    pool: &DbPool,
    id: &str,
    name: &str,
    collection_type: &str,
    paths: &[String],
) -> Result<Library, LibError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibError::EmptyName);
    }
    let ctype = normalize_type(collection_type).ok_or(LibError::InvalidType)?;
    if get_by_name(pool, name).await?.is_some() {
        return Err(LibError::NameExists);
    }
    let mut valid = Vec::new();
    for p in paths {
        let v = validate_path(p)?;
        if !valid.contains(&v) {
            valid.push(v);
        }
    }

    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO libraries (id, name, collection_type) VALUES (?, ?, ?)")
        .bind(id)
        .bind(name)
        .bind(ctype)
        .execute(&mut *tx)
        .await?;
    for p in &valid {
        sqlx::query("INSERT INTO library_paths (library_id, path) VALUES (?, ?)")
            .bind(id)
            .bind(p)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    Ok(Library {
        id: id.to_string(),
        name: name.to_string(),
        collection_type: ctype.to_string(),
        paths: valid,
    })
}

/// Remove a biblioteca e suas pastas. Os itens somem no próximo scan (já não estão em nenhuma pasta).
pub async fn delete(pool: &DbPool, name: &str) -> Result<(), LibError> {
    let lib = get_by_name(pool, name).await?.ok_or(LibError::NotFound)?;
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM library_paths WHERE library_id = ?").bind(&lib.id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM libraries WHERE id = ?").bind(&lib.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn add_path(pool: &DbPool, name: &str, path: &str) -> Result<(), LibError> {
    let lib = get_by_name(pool, name).await?.ok_or(LibError::NotFound)?;
    let p = validate_path(path)?;
    if lib.paths.contains(&p) {
        return Err(LibError::PathExists);
    }
    sqlx::query("INSERT INTO library_paths (library_id, path) VALUES (?, ?)")
        .bind(&lib.id)
        .bind(&p)
        .execute(pool)
        .await?;
    Ok(())
}

/// Não valida existência: a pasta pode já ter sido apagada/desmontada.
pub async fn remove_path(pool: &DbPool, name: &str, path: &str) -> Result<(), LibError> {
    let lib = get_by_name(pool, name).await?.ok_or(LibError::NotFound)?;
    let wanted: PathBuf = Path::new(path.trim()).components().collect();
    let wanted = wanted.to_string_lossy().to_string();
    let found = lib.paths.iter().find(|p| **p == wanted || **p == path.trim()).ok_or(LibError::NotFound)?;
    sqlx::query("DELETE FROM library_paths WHERE library_id = ? AND path = ?")
        .bind(&lib.id)
        .bind(found)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn rename(pool: &DbPool, name: &str, new_name: &str) -> Result<(), LibError> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err(LibError::EmptyName);
    }
    let lib = get_by_name(pool, name).await?.ok_or(LibError::NotFound)?;
    if let Some(other) = get_by_name(pool, new_name).await? {
        if other.id != lib.id {
            return Err(LibError::NameExists);
        }
    }
    sqlx::query("UPDATE libraries SET name = ? WHERE id = ?")
        .bind(new_name)
        .bind(&lib.id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Banco sem bibliotecas: cria "Filmes" e "Séries" sobre as pastas configuradas por ambiente.
pub async fn seed_if_empty(pool: &DbPool, config: &AppConfig) -> Result<(), sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM libraries").fetch_one(pool).await?;
    if count > 0 {
        return Ok(());
    }

    let mut paths = Vec::new();
    for dir in &config.media_directories {
        // Pasta padrão (ex.: ./media) é criada na primeira execução; as demais precisam existir.
        if !dir.exists() {
            if let Err(e) = tokio::fs::create_dir_all(dir).await {
                warn!("Não foi possível criar {}: {}", dir.display(), e);
                continue;
            }
        }
        let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.clone());
        if let Ok(p) = validate_path(&abs.to_string_lossy()) {
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }

    for (id, name, ctype) in [
        (MOVIES_LIBRARY_ID, "Filmes", "movies"),
        (SERIES_LIBRARY_ID, "Séries", "tvshows"),
    ] {
        match create_with_id(pool, id, name, ctype, &paths).await {
            Ok(_) => {}
            Err(LibError::Db(e)) => return Err(e),
            Err(e) => warn!("Biblioteca padrão '{}' não criada: {}", name, e),
        }
    }
    info!("Bibliotecas padrão criadas (Filmes e Séries) com {} pasta(s).", paths.len());
    Ok(())
}

// ---------------------------------------------------------------- varredura

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibRef {
    pub id: String,
    pub collection_type: String,
}

/// Pastas distintas a varrer, cada uma com as bibliotecas que a contêm. As mais profundas vêm
/// primeiro: um arquivo em pasta aninhada pertence à biblioteca mais específica.
pub fn scan_roots(libs: &[Library]) -> Vec<(PathBuf, Vec<LibRef>)> {
    let mut roots: Vec<(PathBuf, Vec<LibRef>)> = Vec::new();
    for lib in libs {
        for p in &lib.paths {
            let path = PathBuf::from(p);
            let r = LibRef { id: lib.id.clone(), collection_type: lib.collection_type.clone() };
            match roots.iter_mut().find(|(rp, _)| *rp == path) {
                Some((_, refs)) => refs.push(r),
                None => roots.push((path, vec![r])),
            }
        }
    }
    roots.sort_by(|a, b| b.0.components().count().cmp(&a.0.components().count()).then(a.0.cmp(&b.0)));
    roots
}

/// Entre as bibliotecas da pasta, a de tipo compatível com o conteúdo (episódio → séries).
pub fn pick_library(libs: &[LibRef], is_episode: bool) -> Option<String> {
    let want = if is_episode { "tvshows" } else { "movies" };
    libs.iter()
        .find(|l| l.collection_type == want)
        .or(libs.first())
        .map(|l| l.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib(id: &str, ctype: &str, paths: &[&str]) -> Library {
        Library {
            id: id.into(),
            name: id.into(),
            collection_type: ctype.into(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn type_names() {
        assert_eq!(normalize_type("Movies"), Some("movies"));
        assert_eq!(normalize_type("tvshows"), Some("tvshows"));
        assert_eq!(normalize_type("series"), Some("tvshows"));
        assert_eq!(normalize_type("music"), None);
    }

    #[test]
    fn validates_paths() {
        let tmp = std::env::temp_dir();
        let ok = validate_path(&format!("{}/", tmp.display())).unwrap();
        assert!(!ok.ends_with('/') || ok.len() == 1);
        assert!(matches!(validate_path(""), Err(LibError::InvalidPath(_))));
        assert!(matches!(validate_path("relativo/pasta"), Err(LibError::InvalidPath(_))));
        let missing = tmp.join("rf_pasta_que_nao_existe_xyz");
        assert!(matches!(validate_path(&missing.to_string_lossy()), Err(LibError::InvalidPath(_))));
        let file = tmp.join(format!("rf_arquivo_{}.txt", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(validate_path(&file.to_string_lossy()), Err(LibError::InvalidPath(_))));
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn scan_roots_group_and_order() {
        let libs = vec![
            lib("filmes", "movies", &["/media", "/media/anime"]),
            lib("series", "tvshows", &["/media"]),
        ];
        let roots = scan_roots(&libs);
        // /media/anime (mais profunda) primeiro; /media compartilhada pelas duas bibliotecas
        assert_eq!(roots[0].0, PathBuf::from("/media/anime"));
        assert_eq!(roots[0].1.len(), 1);
        assert_eq!(roots[1].0, PathBuf::from("/media"));
        assert_eq!(roots[1].1.len(), 2);
    }

    #[test]
    fn picks_matching_library() {
        let refs = vec![
            LibRef { id: "f".into(), collection_type: "movies".into() },
            LibRef { id: "s".into(), collection_type: "tvshows".into() },
        ];
        assert_eq!(pick_library(&refs, false).as_deref(), Some("f"));
        assert_eq!(pick_library(&refs, true).as_deref(), Some("s"));
        // Só há biblioteca de filmes: episódios caem nela mesmo assim
        assert_eq!(pick_library(&refs[..1], true).as_deref(), Some("f"));
        assert_eq!(pick_library(&[], true), None);
    }

    #[tokio::test]
    async fn crud_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rf_lib_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.join("t.db").to_string_lossy().replace('\\', "/"));
        let pool = crate::db::init_db(&url).await.unwrap();
        let a = dir.join("a").to_string_lossy().to_string();
        let b = dir.join("b").to_string_lossy().to_string();

        let l = create(&pool, "Meus Filmes", "movies", std::slice::from_ref(&a)).await.unwrap();
        assert_eq!(l.paths.len(), 1);
        assert!(matches!(create(&pool, "meus filmes", "movies", &[]).await, Err(LibError::NameExists)));
        assert!(matches!(create(&pool, "X", "music", &[]).await, Err(LibError::InvalidType)));
        assert!(matches!(create(&pool, "  ", "movies", &[]).await, Err(LibError::EmptyName)));

        add_path(&pool, "MEUS FILMES", &b).await.unwrap();
        assert!(matches!(add_path(&pool, "Meus Filmes", &b).await, Err(LibError::PathExists)));
        assert_eq!(get_by_name(&pool, "Meus Filmes").await.unwrap().unwrap().paths.len(), 2);

        remove_path(&pool, "Meus Filmes", &a).await.unwrap();
        rename(&pool, "Meus Filmes", "Cinema").await.unwrap();
        let all = list(&pool).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Cinema");
        assert_eq!(all[0].paths, vec![b]);

        delete(&pool, "Cinema").await.unwrap();
        assert!(list(&pool).await.unwrap().is_empty());
        assert!(matches!(delete(&pool, "Cinema").await, Err(LibError::NotFound)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
