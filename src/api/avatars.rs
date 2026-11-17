//! Foto de perfil: `GET/POST/DELETE /Users/{id}/Images/Primary` (como o Jellyfin) e as rotas do painel.
//!
//! O arquivo fica em `<cache>/avatars/<id>-<carimbo>.<ext>`; o banco guarda só o nome, então a URL
//! nunca escolhe um caminho em disco. O corpo do POST é a imagem (bytes) ou o texto em base64, que é
//! como o Jellyfin Web a envia.

use crate::{
    api::{auth::CurrentUser, shows::normalize_id},
    config::AppConfig,
    db::DbPool,
    users,
};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, StatusCode},
    response::Response,
};
use base64::Engine;
use std::{path::PathBuf, sync::Arc, time::SystemTime};

pub const MAX_BYTES: usize = 8 * 1024 * 1024;

type St = State<(Arc<AppConfig>, DbPool)>;

/// (extensão, MIME) pelo início do arquivo; `None` se não for JPEG, PNG, WebP ou GIF.
pub fn sniff(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(("png", "image/png"))
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(("gif", "image/gif"))
    } else {
        None
    }
}

/// Aceita a imagem crua ou em base64 (com ou sem quebras de linha).
pub fn decode_body(body: &[u8]) -> Option<Vec<u8>> {
    if body.is_empty() || body.len() > MAX_BYTES {
        return None;
    }
    if sniff(body).is_some() {
        return Some(body.to_vec());
    }
    let text: Vec<u8> = body.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(text).ok()?;
    sniff(&bytes).map(|_| bytes)
}

fn avatars_dir(config: &AppConfig) -> PathBuf {
    config.cache_dir.join("avatars")
}

/// O ID entra no nome do arquivo: só hexadecimal (nunca `..` nem separadores de pasta).
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Salva a foto, atualiza o usuário e apaga a anterior.
pub async fn save(config: &AppConfig, pool: &DbPool, user_id: &str, body: &[u8]) -> Result<(), StatusCode> {
    if !valid_id(user_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let bytes = decode_body(body).ok_or(StatusCode::BAD_REQUEST)?;
    let (ext, _) = sniff(&bytes).ok_or(StatusCode::BAD_REQUEST)?;
    let dir = avatars_dir(config);
    tokio::fs::create_dir_all(&dir).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = format!("{user_id}-{stamp}.{ext}");
    let tmp = dir.join(format!("{name}.part"));
    tokio::fs::write(&tmp, &bytes).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    tokio::fs::rename(&tmp, dir.join(&name)).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match users::set_avatar(pool, user_id, Some(&name)).await {
        Ok(old) => {
            if let Some(old) = old.filter(|o| *o != name) {
                let _ = tokio::fs::remove_file(dir.join(old)).await;
            }
            Ok(())
        }
        Err(users::UserError::NotFound) => {
            let _ = tokio::fs::remove_file(dir.join(&name)).await;
            Err(StatusCode::NOT_FOUND)
        }
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn clear(config: &AppConfig, pool: &DbPool, user_id: &str) -> Result<(), StatusCode> {
    if !valid_id(user_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    match users::set_avatar(pool, user_id, None).await {
        Ok(old) => {
            if let Some(old) = old {
                let _ = tokio::fs::remove_file(avatars_dir(config).join(old)).await;
            }
            Ok(())
        }
        Err(users::UserError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Tag de cache enviada em `PrimaryImageTag`: muda a cada troca de foto.
pub fn image_tag(avatar: &str) -> String {
    avatar.rsplit_once('-').map_or(avatar, |(_, rest)| rest.split('.').next().unwrap_or(rest)).to_string()
}

/// `GET /Users/{id}/Images/Primary`: pública (a tela de login mostra as fotos antes do login).
pub async fn get_avatar(State((config, pool)): St, Path((id, _kind)): Path<(String, String)>) -> Result<Response, StatusCode> {
    let user = users::get(&pool, &normalize_id(&id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let name = user.avatar.ok_or(StatusCode::NOT_FOUND)?;
    let bytes = tokio::fs::read(avatars_dir(&config).join(&name)).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let mime = sniff(&bytes).map_or("application/octet-stream", |(_, m)| m);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .header(header::ETAG, format!("\"{name}\""))
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .body(Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Só o próprio usuário ou um administrador troca a foto.
fn may_edit(me: &users::AuthUser, id: &str) -> bool {
    me.is_admin || me.id == id
}

pub async fn post_avatar(
    State((config, pool)): St,
    CurrentUser(me): CurrentUser,
    Path((id, _kind)): Path<(String, String)>,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    let id = normalize_id(&id);
    if !may_edit(&me, &id) {
        return Err(StatusCode::FORBIDDEN);
    }
    save(&config, &pool, &id, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_avatar(
    State((config, pool)): St,
    CurrentUser(me): CurrentUser,
    Path((id, _kind)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
    let id = normalize_id(&id);
    if !may_edit(&me, &id) {
        return Err(StatusCode::FORBIDDEN);
    }
    clear(&config, &pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

    #[test]
    fn detects_image_types() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]).map(|t| t.0), Some("jpg"));
        assert_eq!(sniff(PNG).map(|t| t.0), Some("png"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ").map(|t| t.0), Some("webp"));
        assert_eq!(sniff(b"GIF89a....").map(|t| t.0), Some("gif"));
        assert_eq!(sniff(b"<svg onload=alert(1)>"), None);
        assert_eq!(sniff(b"MZ\x90\0"), None);
    }

    #[test]
    fn accepts_raw_or_base64_only_for_images() {
        assert_eq!(decode_body(PNG).as_deref(), Some(PNG));
        let b64 = base64::engine::general_purpose::STANDARD.encode(PNG);
        let wrapped = format!("{}\n{}\r\n", &b64[..8], &b64[8..]);
        assert_eq!(decode_body(wrapped.as_bytes()).as_deref(), Some(PNG));
        // base64 de algo que não é imagem, texto qualquer e vazio
        let evil = base64::engine::general_purpose::STANDARD.encode(b"<script>alert(1)</script>");
        assert!(decode_body(evil.as_bytes()).is_none());
        assert!(decode_body(b"oi").is_none());
        assert!(decode_body(b"").is_none());
    }

    #[test]
    fn only_hex_ids_become_file_names() {
        assert!(valid_id("00000000000000000000000000000001"));
        assert!(!valid_id(r"..\..\evil"));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
    }

    #[test]
    fn tag_changes_with_each_upload() {
        assert_eq!(image_tag("abc123-1700000000000.png"), "1700000000000");
        assert_ne!(image_tag("abc123-1.png"), image_tag("abc123-2.png"));
    }
}
