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

fn verificar(caminhos: &[String], catalogo: Vec<Catalogado>, com_historico: &HashSet<String>) -> Verificacao {
    let catalogo: HashMap<_, _> = catalogo
        .into_iter()
        .map(|item| (item.path.clone(), item))
        .collect();
    let mut por_assinatura: HashMap<Vec<u8>, Vec<&str>> = HashMap::new();
    let mut recalculadas = HashMap::new();
    let mut completa = true;
    let mut buffer = vec![0; 256 * 1024];

    // Arquivos do catálogo que sumiram: só eles podem ter sido movidos ou editados.
    let vistos: HashSet<_> = caminhos.iter().map(String::as_str).collect();
    let mut ausentes: HashMap<&[u8], Vec<&Catalogado>> = HashMap::new();
    for item in catalogo.values() {
        if vistos.contains(item.path.as_str()) {
            continue;
        }
        // Sem hash (adiado pela varredura e ainda não calculado), o item não casa por conteúdo: entra com a
        // chave vazia e só pode ser oferecido na revisão do acervo, pelo nome, nunca movido sozinho.
        let hash = item.content_sha256.as_deref().unwrap_or_default();
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
    let tamanhos_ausentes: HashSet<i64> = ausentes.values().flatten().map(|i| i.size_bytes).collect();
    let chaves_ausentes: HashSet<Chave> = ausentes.values().flatten().map(|i| chave(&i.path).0).collect();

    // O hash do arquivo inteiro custa uma leitura completa (30 GB levavam ~5 min). Só é calculado aqui quando
    // serve para decidir algo agora: um arquivo alterado que já tinha hash (substituição) ou um arquivo novo que
    // pode ser o destino de um movimento (mesmo tamanho ou mesmo nome que um ausente). Os demais ficam sem hash
    // e o `completar_pendentes` os preenche em segundo plano, sem atrasar a biblioteca.
    for caminho in caminhos {
        let path = Path::new(caminho);
        let leitura = (|| {
            let (mtime, tamanho) = estado(&std::fs::metadata(path)?)?;
            let precisa = if let Some(item) = catalogo.get(caminho) {
                if item.size_bytes == tamanho && item.content_mtime.as_deref() == Some(&mtime) {
                    if let Some(hash) = &item.content_sha256 {
                        return Ok(Some(hash.clone()));
                    }
                }
                // Sem hash anterior e com o tamanho diferente do catalogado, o arquivo foi trocado antes de a assinatura em
                // segundo plano chegar: se alguém já assistiu ou favoritou, o conteúdo novo é lido agora para a troca virar
                // uma revisão em vez de herdar o histórico em silêncio.
                item.content_sha256.is_some() || (com_historico.contains(&item.id) && item.size_bytes != tamanho)
            } else {
                tamanhos_ausentes.contains(&tamanho) || chaves_ausentes.contains(&chave(caminho).0)
            };
            if !precisa {
                return Ok(None);
            }
            let assinatura = assinar(path, mtime, tamanho, &mut buffer)?;
            let hash = assinatura.sha256.clone();
            recalculadas.insert(caminho.clone(), assinatura);
            Ok::<_, io::Error>(Some(hash))
        })();
        match leitura {
            Ok(Some(hash)) => por_assinatura.entry(hash).or_default().push(caminho),
            Ok(None) => {}
            Err(e) => {
                completa = false;
                warn!(
                    "Não foi possível identificar {} pelo conteúdo: {e}",
                    path.display()
                );
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
        com_historico,
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
    com_historico: &HashSet<String>,
) -> Vec<Sugestao> {
    let mut novos = HashMap::<Chave, Vec<(&str, Option<i32>, &[u8])>>::new();
    let mut revisoes = Vec::new();
    for (caminho, assinatura) in recalculadas {
        if let Some(item) = catalogo.get(caminho) {
            let trocado = match &item.content_sha256 {
                Some(hash) => *hash != assinatura.sha256,
                // Ainda sem hash: só o tamanho diferente do catalogado prova a troca, e só importa se há histórico.
                None => com_historico.contains(&item.id) && item.size_bytes != assinatura.tamanho,
            };
            if trocado {
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

/// Hashes que a varredura deixou para depois (arquivos novos sem relação com algum ausente), calculados um por vez,
/// em segundo plano (quem chama garante que só um preenchimento roda por vez).
pub(super) async fn completar_pendentes(pool: &DbPool) {
    let pendentes: Vec<(String, i64)> =
        sqlx::query_as("SELECT path, size_bytes FROM items WHERE content_sha256 IS NULL ORDER BY size_bytes")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    if !pendentes.is_empty() {
        info!("Calculando a assinatura de {} arquivos em segundo plano...", pendentes.len());
    }
    for (caminho, tamanho) in pendentes {
        let alvo = caminho.clone();
        let resultado = tokio::task::spawn_blocking(move || {
            let (mtime, tam) = estado(&std::fs::metadata(&alvo)?)?;
            if tam != tamanho {
                return Err(io::Error::other("O tamanho mudou desde a varredura"));
            }
            let mut buffer = vec![0; 256 * 1024];
            assinar(Path::new(&alvo), mtime, tam, &mut buffer)
        })
        .await;
        match resultado {
            Ok(Ok(a)) => {
                let _ = sqlx::query("UPDATE items SET content_sha256 = ?, content_mtime = ? WHERE path = ? AND size_bytes = ?")
                    .bind(a.sha256)
                    .bind(a.mtime)
                    .bind(&caminho)
                    .bind(a.tamanho)
                    .execute(pool)
                    .await;
            }
            Ok(Err(e)) => warn!("Assinatura adiada de {caminho} não calculada: {e}"),
            Err(_) => {}
        }
    }
}

pub(super) async fn preparar(
    pool: &DbPool,
    caminhos: Vec<String>,
) -> Result<Verificacao, Box<dyn std::error::Error + Send + Sync>> {
    let catalogo =
        sqlx::query_as("SELECT id, path, size_bytes, content_sha256, content_mtime FROM items")
            .fetch_all(pool)
            .await?;
    // Itens que alguém já assistiu, favoritou ou parou no meio: são os que têm histórico a proteger numa troca.
    let com_historico: HashSet<String> = sqlx::query_scalar(
        "SELECT DISTINCT item_id FROM playback_state WHERE is_played = 1 OR is_favorite = 1 OR position_ticks > 0",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    Ok(tokio::task::spawn_blocking(move || verificar(&caminhos, catalogo, &com_historico)).await?)
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
    fn arquivo_novo_sem_ausentes_nao_e_lido() {
        let dir = std::env::temp_dir().join(format!("rustyfin-identidade-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let novo = dir.join("novo.mp4");
        std::fs::write(&novo, b"conteudo novo").unwrap();
        let resultado = verificar(&[novo.to_str().unwrap().into()], Vec::new(), &HashSet::new());
        std::fs::remove_dir_all(dir).unwrap();
        assert!(resultado.completa);
        assert!(resultado.recalculadas.is_empty(), "sem ausentes não há o que casar: nada é lido");
    }

    #[test]
    fn novo_com_o_tamanho_ou_o_nome_de_um_ausente_e_lido() {
        let dir = std::env::temp_dir().join(format!("rustyfin-identidade-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let ausente = Catalogado {
            id: "a".into(),
            path: dir.join("Filme (2020).mp4").to_str().unwrap().into(),
            size_bytes: 13,
            content_sha256: Some(Sha256::digest(b"outro arquivo").to_vec()),
            content_mtime: None,
        };
        let mesmo_tamanho = dir.join("renomeado.mp4");
        let mesmo_nome = dir.join("sub").join("Filme (2020).mkv");
        let alheio = dir.join("alheio.mp4");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(&mesmo_tamanho, b"outro arquivo").unwrap();
        std::fs::write(&mesmo_nome, b"re-encodado, tamanho diferente").unwrap();
        std::fs::write(&alheio, b"coisa sem relacao").unwrap();
        let caminhos: Vec<String> = [&mesmo_tamanho, &mesmo_nome, &alheio].iter().map(|p| p.to_str().unwrap().into()).collect();
        let resultado = verificar(&caminhos, vec![ausente], &HashSet::new());
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(resultado.recalculadas.len(), 2, "só o de mesmo tamanho e o de mesmo nome são lidos");
        assert!(!resultado.recalculadas.contains_key(alheio.to_str().unwrap()));
        assert_eq!(resultado.movimentos.len(), 1);
    }

    #[test]
    fn video_trocado_antes_do_primeiro_hash_vira_revisao_so_se_ha_historico() {
        let dir = std::env::temp_dir().join(format!("rustyfin-identidade-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let caminho = dir.join("Filme (2020).mkv");
        std::fs::write(&caminho, b"edicao nova, bem maior que a antiga").unwrap();
        let item = || Catalogado {
            id: "item1".into(),
            path: caminho.to_str().unwrap().into(),
            size_bytes: 10, // o tamanho que a varredura anterior viu
            content_sha256: None,
            content_mtime: None,
        };
        let caminhos = [caminho.to_str().unwrap().to_owned()];

        // Alguém já assistiu: o conteúdo novo é lido e a troca vira revisão, em vez de herdar o histórico.
        let com = HashSet::from(["item1".to_owned()]);
        let resultado = verificar(&caminhos, vec![item()], &com);
        assert_eq!(resultado.revisoes.len(), 1);
        assert_eq!(resultado.revisoes[0].motivo, "substituido");

        // Sem histórico, nada a proteger: o arquivo nem é lido e segue o fluxo normal.
        let resultado = verificar(&caminhos, vec![item()], &HashSet::new());
        std::fs::remove_dir_all(dir).unwrap();
        assert!(resultado.revisoes.is_empty());
        assert!(resultado.recalculadas.is_empty());
    }

    #[tokio::test]
    async fn pendentes_sao_preenchidos_em_segundo_plano() {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!("rustyfin-identidade-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let arquivo = dir.join("filme.mp4");
        std::fs::write(&arquivo, b"conteudo do filme").unwrap();
        let caminho = arquivo.to_str().unwrap().to_string();
        sqlx::query("INSERT INTO items (id, name, path, media_type, size_bytes) VALUES ('i1', 'Filme', ?, 'Movie', 17)")
            .bind(&caminho)
            .execute(&pool)
            .await
            .unwrap();
        completar_pendentes(&pool).await;
        let (hash, mtime): (Option<Vec<u8>>, Option<String>) =
            sqlx::query_as("SELECT content_sha256, content_mtime FROM items WHERE id = 'i1'").fetch_one(&pool).await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(hash.unwrap(), Sha256::digest(b"conteudo do filme").to_vec());
        assert!(mtime.is_some());
    }

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
        let resultado = verificar(&caminhos, vec![origem], &HashSet::new());
        std::fs::remove_dir_all(dir).unwrap();
        assert!(resultado.completa);
        assert_eq!(resultado.recalculadas.len(), 2);
        assert!(resultado.movimentos.is_empty());
    }
}
