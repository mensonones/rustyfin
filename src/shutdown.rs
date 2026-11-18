//! Pedido de desligamento gracioso vindo de dentro do processo (hoje, do ícone da bandeja do Windows).
//!
//! O servidor já encerra ao receber SIGTERM ou Ctrl+C; este sinal faz o mesmo caminho (para as transcodificações e fecha
//! o banco), para quem não consegue enviar um sinal ao processo, como um programa sem console.

use tokio::sync::Notify;

/// Quem chama `DESLIGAR.notify_one()` pede o desligamento; o `main` espera por `DESLIGAR.notified()`.
pub static DESLIGAR: Notify = Notify::const_new();
