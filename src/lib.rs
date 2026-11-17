//! Rustyfin: servidor de mídia leve compatível com a API de clientes Jellyfin.
//!
//! A biblioteca concentra toda a lógica (rotas, banco, scanner, transcodificação); o binário
//! (`src/main.rs`) só inicializa o registro de logs, carrega a configuração e sobe o servidor.
//! Isso permite testes de integração em `tests/` e `cargo doc`.

mod linha_do_tempo;
mod revisao_acervo;
pub mod api;
pub mod config;
pub mod db;
pub mod discovery;
pub mod libraries;
pub mod library;
pub mod metadata;
pub mod models;
pub mod parser;
pub mod scanner;
pub mod settings;
pub mod transcode;
pub mod users;
