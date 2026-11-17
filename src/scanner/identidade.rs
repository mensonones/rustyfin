use crate::{
    db::DbPool,
    parser::{self, Kind},
    revisao_acervo::{self, Sugestao},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, Metadata},
    io::{self, Read},
    path::Path,
    time::UNIX_EPOCH,
};
use tracing::{info, warn};

#[derive(sqlx::FromRow)]
struct Catalogado {
    id: String,
    path: String,
    size_bytes: i64,
    content_sha256: Option<Vec<u8>>,
    content_mtime: Option<String>,
}

struct Assinatura {
    sha256: Vec<u8>,
    mtime: String,
    tamanho: i64,
}

pub(super) struct Verificacao {
    recalculadas: HashMap<String, Assinatura>,
    movimentos: Vec<(String, String, String)>,
    revisoes: Vec<Sugestao>,
    pub(super) completa: bool,
}

fn estado(metadados: &Metadata) -> io::Result<(String, i64)> {
    let instante = metadados
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let tamanho = i64::try_from(metadados.len()).map_err(io::Error::other)?;
    Ok((instante.as_nanos().to_string(), tamanho))
}

fn assinar(
    caminho: &Path,
    mtime: String,
    tamanho: i64,
    buffer: &mut [u8],
) -> io::Result<Assinatura> {
    let mut arquivo = File::open(caminho)?;
    let mut hash = Sha256::new();
    loop {
        let lidos = arquivo.read(buffer)?;
        if lidos == 0 {
            break;
        }
        hash.update(&buffer[..lidos]);
    }
    let atual = estado(&std::fs::metadata(caminho)?)?;
    if atual != (mtime.clone(), tamanho) || estado(&arquivo.metadata()?)? != atual {
        return Err(io::Error::other("O arquivo mudou durante a leitura"));
    }
    Ok(Assinatura {
        sha256: hash.finalize().to_vec(),
        mtime,
        tamanho,
    })
}

fn verificar(caminhos: &[String], catalogo: Vec<Catalogado>) -> Verificacao {
    let catalogo: HashMap<_, _> = catalogo
        .into_iter()
        .map(|item| (item.path.clone(), item))
        .collect();
    let mut por_assinatura: HashMap<Vec<u8>, Vec<&str>> = HashMap::new();
    let mut recalculadas = HashMap::new();
    let mut completa = true;
    let mut buffer = vec![0; 256 * 1024];
    for caminho in caminhos {
        let path = Path::new(caminho);
        let leitura = (|| {
            let (mtime, tamanho) = estado(&std::fs::metadata(path)?)?;
            if let Some(item) = catalogo.get(caminho) {
                if item.size_bytes == tamanho && item.content_mtime.as_deref() == Some(&mtime) {
                    if let Some(hash) = &item.content_sha256 {
                        return Ok(hash.clone());
                    }
                }
            }
            let assinatura = assinar(path, mtime, tamanho, &mut buffer)?;
            let hash = assinatura.sha256.clone();
            recalculadas.insert(caminho.clone(), assinatura);
            Ok::<_, io::Error>(hash)
        })();
        match leitura {
            Ok(hash) => por_assinatura.entry(hash).or_default().push(caminho),
            Err(e) => {
                completa = false;
                warn!(
                    "Não foi possível identificar {} pelo conteúdo: {e}",
                    path.display()
                );
            }
        }
    }

    let vistos: HashSet<_> = caminhos.iter().map(String::as_str).collect();
    let mut ausentes: HashMap<&[u8], Vec<&Catalogado>> = HashMap::new();
    for item in catalogo.values() {
        if vistos.contains(item.path.as_str()) {
            continue;
        }
        if let Some(hash) = item.content_sha256.as_deref() {
            match Path::new(&item.path).try_exists() {
                Ok(false) => ausentes.entry(hash).or_default().push(item),
                Ok(true) => {}
                Err(e) => {
                    completa = false;
                    warn!(
                        "Não foi possível verificar o caminho anterior {}: {e}",
                        item.path
                    );
                }
            }
        }
    }
    let movimentos: Vec<_> = ausentes
        .iter()
        .filter_map(|(hash, origens)| {
            let [origem] = origens.as_slice() else {
                return None;
            };
            let [destino] = por_assinatura.get(*hash)?.as_slice() else {
                return None;
            };
            if catalogo.contains_key(*destino) {
                // Uma cópia já conhecida pode ser escolhida manualmente na revisão.
                return None;
            }
            Some((
                origem.id.clone(),
                origem.path.clone(),
                (*destino).to_owned(),
            ))
        })
        .collect();
    for caminhos in por_assinatura.values_mut().filter(|c| c.len() > 1) {
        caminhos.sort_unstable();
    }
    let revisoes = sugerir_revisoes(
        &catalogo,
        &recalculadas,
        &ausentes,
        &por_assinatura,
        &movimentos,
    );
    Verificacao {
        recalculadas,
        movimentos,
        revisoes,
        completa,
    }
}

#[derive(PartialEq, Eq, Hash)]
enum Chave {
    Filme(String),
    Episodio(String, i32, i32),
}

fn chave(caminho: &str) -> (Chave, Option<i32>) {
    let parsed = parser::parse_path(Path::new(caminho));
    let chave = match parsed.kind {
        Kind::Movie => Chave::Filme(parsed.title.to_lowercase()),
        Kind::Episode {
            series,
            season,
            episode,
        } => Chave::Episodio(series.to_lowercase(), season, episode),
    };
    (chave, parsed.year)
}

fn sugerir_revisoes(
    catalogo: &HashMap<String, Catalogado>,
    recalculadas: &HashMap<String, Assinatura>,
    ausentes: &HashMap<&[u8], Vec<&Catalogado>>,
    por_assinatura: &HashMap<Vec<u8>, Vec<&str>>,
    movimentos: &[(String, String, String)],
) -> Vec<Sugestao> {
    let mut novos = HashMap::<Chave, Vec<(&str, Option<i32>, &[u8])>>::new();
    let mut revisoes = Vec::new();
    for (caminho, assinatura) in recalculadas {
        if let Some(item) = catalogo.get(caminho) {
            if item
                .content_sha256
                .as_ref()
                .is_some_and(|hash| *hash != assinatura.sha256)
            {
                revisoes.push(Sugestao {
                    id: item.id.clone(),
                    caminho: caminho.clone(),
                    motivo: "substituido",
                    destinos: vec![(caminho.clone(), assinatura.sha256.clone())],
                });
            }
        } else {
            let (chave, ano) = chave(caminho);
            novos
                .entry(chave)
                .or_default()
                .push((caminho, ano, &assinatura.sha256));
        }
    }

    let movidos: HashSet<_> = movimentos.iter().map(|(id, _, _)| id.as_str()).collect();
    for (hash, origens) in ausentes {
        for item in origens {
            if movidos.contains(item.id.as_str()) {
                continue;
            }
            let (motivo, mut destinos) = if let Some(caminhos) = por_assinatura.get(*hash) {
                let destinos = caminhos
                    .iter()
                    .take(12)
                    .map(|c| (*c, *hash))
                    .collect::<Vec<_>>();
                ("mesmo_conteudo", destinos)
            } else {
                let (chave, ano) = chave(&item.path);
                let destinos = novos
                    .get(&chave)
                    .into_iter()
                    .flatten()
                    .filter(|(_, outro, _)| ano.is_none() || outro.is_none() || ano == *outro)
                    .map(|(caminho, _, hash)| (*caminho, *hash))
                    .collect();
                ("possivel_edicao", destinos)
            };
            destinos.sort_unstable_by_key(|(caminho, _)| *caminho);
            destinos.truncate(12);
            if !destinos.is_empty() {
                revisoes.push(Sugestao {
                    id: item.id.clone(),
                    caminho: item.path.clone(),
                    motivo,
                    destinos: destinos
                        .into_iter()
                        .map(|(c, h)| (c.to_owned(), h.to_vec()))
                        .collect(),
                });
            }
        }
    }
    revisoes
}

pub(super) async fn preparar(
    pool: &DbPool,
    caminhos: Vec<String>,
) -> Result<Verificacao, Box<dyn std::error::Error + Send + Sync>> {
    let catalogo =
        sqlx::query_as("SELECT id, path, size_bytes, content_sha256, content_mtime FROM items")
            .fetch_all(pool)
            .await?;
    Ok(tokio::task::spawn_blocking(move || verificar(&caminhos, catalogo)).await?)
}

impl Verificacao {
    pub(super) async fn mover(
        &self,
        pool: &DbPool,
        bibliotecas: &HashMap<String, Option<String>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.movimentos.is_empty() {
            return Ok(());
        }
        for (_, origem, destino) in &self.movimentos {
            let assinatura = self
                .recalculadas
                .get(destino)
                .ok_or_else(|| io::Error::other("Assinatura do destino ausente"))?;
            let metadados = tokio::fs::metadata(destino).await?;
            let (mtime, tamanho) = estado(&metadados)?;
            if mtime != assinatura.mtime
                || tamanho != assinatura.tamanho
                || tokio::fs::try_exists(origem).await?
            {
                return Err(io::Error::other("Origem ou destino mudou durante a varredura").into());
            }
        }
        let mut tx = pool.begin().await?;
        for (id, origem, destino) in &self.movimentos {
            sqlx::query(
                "UPDATE items SET path = ?, library_id = ?, poster_path = NULL, backdrop_path = NULL,                  subtitle_path = NULL, series_poster_path = NULL, series_backdrop_path = NULL,                  season_poster_path = NULL, probed = 0, content_mtime = NULL                  WHERE id = ? AND path = ?",
            )
            .bind(destino)
            .bind(bibliotecas.get(destino).and_then(Option::as_deref))
            .bind(id)
            .bind(origem)
            .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        for (_, origem, destino) in &self.movimentos {
            info!("Arquivo movido: {origem} → {destino}");
        }
        Ok(())
    }

    pub(super) fn substituicoes(&self) -> HashSet<String> {
        self.revisoes
            .iter()
            .filter(|r| r.motivo == "substituido")
            .map(|r| r.caminho.clone())
            .collect()
    }

    pub(super) fn suspender_substituicoes(&mut self, suspensas: &HashSet<String>) {
        self.recalculadas
            .retain(|caminho, _| !suspensas.contains(caminho));
    }

    pub(super) async fn publicar_revisoes(
        &self,
        pool: &DbPool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.revisoes.is_empty() {
            return Ok(());
        }
        for sugestao in &self.revisoes {
            if sugestao.motivo == "substituido" {
                let assinatura = self
                    .recalculadas
                    .get(&sugestao.caminho)
                    .ok_or_else(|| io::Error::other("Assinatura da substituição ausente"))?;
                let (mtime, tamanho) = estado(&tokio::fs::metadata(&sugestao.caminho).await?)?;
                if mtime != assinatura.mtime || tamanho != assinatura.tamanho {
                    return Err(io::Error::other("A substituição mudou durante a varredura").into());
                }
            } else if tokio::fs::try_exists(&sugestao.caminho).await? {
                return Err(io::Error::other("A origem reapareceu durante a varredura").into());
            }
        }
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        for sugestao in &self.revisoes {
            revisao_acervo::arquivar(&mut tx, sugestao).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn salvar(self, pool: &DbPool) -> Result<(), sqlx::Error> {
        if self.recalculadas.is_empty() {
            return Ok(());
        }
        let mut tx = pool.begin().await?;
        for (caminho, assinatura) in self.recalculadas {
            sqlx::query(
                "UPDATE items SET content_sha256 = ?, content_mtime = ?                  WHERE path = ? AND size_bytes = ?",
            )
            .bind(assinatura.sha256).bind(assinatura.mtime).bind(caminho).bind(assinatura.tamanho)
            .execute(&mut *tx).await?;
        }
        tx.commit().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dois_destinos_com_o_mesmo_conteudo_sao_ambiguos() {
        let dir =
            std::env::temp_dir().join(format!("rustyfin-identidade-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let antigo = dir.join("antigo.mp4");
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        for caminho in [&a, &b] {
            std::fs::write(caminho, b"mesmo arquivo").unwrap();
        }
        let origem = Catalogado {
            id: "antigo".into(),
            path: antigo.to_str().unwrap().into(),
            size_bytes: 13,
            content_sha256: Some(Sha256::digest(b"mesmo arquivo").to_vec()),
            content_mtime: None,
        };
        let caminhos = [a.to_str().unwrap().into(), b.to_str().unwrap().into()];
        let resultado = verificar(&caminhos, vec![origem]);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(resultado.completa);
        assert_eq!(resultado.recalculadas.len(), 2);
        assert!(resultado.movimentos.is_empty());
    }
}
