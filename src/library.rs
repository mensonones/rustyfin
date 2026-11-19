//! Varredura automática da biblioteca: scan inicial, por eventos do sistema de arquivos,
//! periódico (polling de segurança) e sob demanda.

use crate::{config::AppConfig, db::DbPool, libraries, metadata, scanner};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;
use tracing::{info, warn};

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Há um scan pedido que ainda não começou.
static PENDING: AtomicBool = AtomicBool::new(false);
/// O próximo scan não precisa esperar o debounce (pedido explícito, não rajada de eventos).
static IMMEDIATE: AtomicBool = AtomicBool::new(false);
static LAST_FINISHED: AtomicI64 = AtomicI64::new(0);
static LAST_FILES: AtomicUsize = AtomicUsize::new(0);

/// (em andamento, pendente, fim do último scan em segundos desde 1970 ou 0, arquivos vistos no último scan)
pub fn scan_status() -> (bool, bool, i64, usize) {
    (
        RUNNING.load(Ordering::Relaxed),
        PENDING.load(Ordering::Relaxed),
        LAST_FINISHED.load(Ordering::Relaxed),
        LAST_FILES.load(Ordering::Relaxed),
    )
}

/// Pede um scan imediato (painel, `/Library/Refresh`): sem esperar o debounce dos eventos de arquivo.
pub fn request_scan_now(trigger: &ScanTrigger) {
    PENDING.store(true, Ordering::Relaxed);
    IMMEDIATE.store(true, Ordering::Relaxed);
    trigger.notify_one();
}

/// Dispara um novo scan (usado por `/Library/Refresh` e pelo watcher). Pedidos feitos durante
/// um scan são coalescidos em um único scan seguinte.
pub type ScanTrigger = Arc<Notify>;

/// Espera após o primeiro evento para agrupar rajadas (ex.: cópia de um arquivo grande).
const DEBOUNCE: Duration = Duration::from_secs(3);

const WATCHED_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "webm", "m4v", "ts", "jpg", "jpeg", "png", "webp", "srt",
];

/// Evento relevante: criação/remoção/alteração de vídeo, imagem, legenda ou pasta.
/// Ignora acessos, temporários (`.part`, `.tmp`) e o diretório de cache.
fn is_relevant(event: &Event, cache_dir: &Path) -> bool {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_)
    ) {
        return false;
    }
    event.paths.iter().any(|p| {
        if p.starts_with(cache_dir) {
            return false;
        }
        match p.extension().and_then(|e| e.to_str()) {
            Some(ext) => WATCHED_EXTENSIONS.contains(&ext.to_lowercase().as_str()),
            None => true, // pasta (ou arquivo sem extensão): pode conter mídia
        }
    })
}

struct Watching {
    watcher: RecommendedWatcher,
    watched: HashSet<PathBuf>,
}

impl Watching {
    fn new(trigger: ScanTrigger, cache_dir: PathBuf) -> notify::Result<Self> {
        let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
            Ok(event) if is_relevant(&event, &cache_dir) => trigger.notify_one(),
            Ok(_) => {}
            Err(e) => warn!("Erro no monitor de pastas: {}", e),
        })?;
        Ok(Self { watcher, watched: HashSet::new() })
    }

    /// Passa a monitorar pastas que existem e ainda não eram monitoradas (podem surgir depois).
    fn sync(&mut self, dirs: &[PathBuf]) {
        for dir in dirs {
            if self.watched.contains(dir) || !dir.is_dir() {
                continue;
            }
            match self.watcher.watch(dir, RecursiveMode::Recursive) {
                Ok(()) => {
                    info!("Monitorando alterações em: {}", dir.display());
                    self.watched.insert(dir.clone());
                }
                Err(e) => warn!(
                    "Não foi possível monitorar {} ({}); o scan periódico continua ativo.",
                    dir.display(),
                    e
                ),
            }
        }
    }
}

pub fn spawn_autoscan(pool: DbPool, config: Arc<AppConfig>, trigger: ScanTrigger) {
    tokio::spawn(async move {
        let mut watching = if config.watch_enabled {
            match Watching::new(trigger.clone(), config.cache_dir.clone()) {
                Ok(w) => Some(w),
                Err(e) => {
                    warn!("Monitor de pastas indisponível ({}); usando apenas scan periódico.", e);
                    None
                }
            }
        } else {
            None
        };

        loop {
            let libs = match libraries::list(&pool).await {
                Ok(l) => l,
                Err(e) => {
                    // Uma lista vazia por erro faria o scan apagar o catálogo: pula e tenta de novo.
                    warn!("Falha ao ler as bibliotecas; scan adiado: {}", e);
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    continue;
                }
            };
            PENDING.store(false, Ordering::Relaxed);
            RUNNING.store(true, Ordering::Relaxed);
            let files = scanner::run_scan(&pool, &libs).await;
            metadata::enrich_pending(&pool, &config).await;
            crate::anilist::enriquecer(&pool, &config).await;
            crate::api::facets::invalidar();
            // Assinaturas de conteúdo que a varredura adiou: não atrasam a biblioteca.
            static HASHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !HASHING.swap(true, Ordering::AcqRel) {
                let hash_pool = pool.clone();
                tokio::spawn(async move {
                    scanner::completar_hashes(&hash_pool).await;
                    HASHING.store(false, Ordering::Release);
                });
            }
            // Miniaturas ao avançar (trickplay): uma por vez, só com o servidor sem transcodificações.
            static MINIATURAS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if config.trickplay && !MINIATURAS.swap(true, Ordering::AcqRel) {
                let (pool_t, cache) = (pool.clone(), config.cache_dir.clone());
                tokio::spawn(async move {
                    crate::trickplay::gerar_pendentes(&pool_t, &cache).await;
                    MINIATURAS.store(false, Ordering::Release);
                });
            }
            // Miniaturas/backdrops dos itens sem arte: prontos antes do 1º acesso (em segundo plano).
            tokio::spawn(crate::api::images::pregenerate(pool.clone(), config.clone()));
            LAST_FILES.store(files, Ordering::Relaxed);
            LAST_FINISHED.store(
                SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
                Ordering::Relaxed,
            );
            RUNNING.store(false, Ordering::Relaxed);

            // Pastas novas (adicionadas no painel) passam a ser monitoradas a cada ciclo.
            if let Some(w) = watching.as_mut() {
                let dirs: Vec<PathBuf> = libs
                    .iter()
                    .flat_map(|l| l.paths.iter().map(PathBuf::from))
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                w.sync(&dirs);
            }

            let interval = crate::settings::scan_interval_secs();
            if interval == 0 {
                trigger.notified().await;
            } else {
                tokio::select! {
                    () = trigger.notified() => {}
                    () = tokio::time::sleep(Duration::from_secs(interval)) => {}
                }
            }

            // Debounce: aguarda a rajada de eventos terminar e descarta o aviso acumulado,
            // senão cada cópia de arquivo geraria um scan extra.
            if !IMMEDIATE.swap(false, Ordering::Relaxed) {
                tokio::time::sleep(DEBOUNCE).await;
                let _ = tokio::time::timeout(Duration::ZERO, trigger.notified()).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind};

    fn ev(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(PathBuf::from(path))
    }

    #[test]
    fn relevance_filter() {
        let cache = Path::new("/data/cache");
        let create = EventKind::Create(CreateKind::File);
        assert!(is_relevant(&ev(create, "/media/a.MKV"), cache));
        assert!(is_relevant(&ev(create, "/media/poster.jpg"), cache));
        assert!(is_relevant(&ev(create, "/media/a.SRT"), cache));
        assert!(is_relevant(&ev(create, "/media/NovaPasta"), cache));
        assert!(is_relevant(&ev(EventKind::Modify(ModifyKind::Any), "/media/a.mp4"), cache));
        assert!(!is_relevant(&ev(create, "/media/a.mkv.part"), cache));
        assert!(!is_relevant(&ev(create, "/media/notas.txt"), cache));
        assert!(!is_relevant(&ev(create, "/data/cache/thumbs/x.jpg"), cache));
        assert!(!is_relevant(&ev(EventKind::Access(AccessKind::Any), "/media/a.mkv"), cache));
    }
}
