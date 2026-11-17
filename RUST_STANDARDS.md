# 🦀 Guia de Padrões e Arquitetura Rust para LLMs e Desenvolvedores (Rustyfin)

> **Destinatários:** Agentes de IA (LLMs) e desenvolvedores que contribuem com o codebase do Rustyfin.  
> **Objetivo:** Garantir código Rust idiomático, livre de travamentos do runtime Tokio, eficiente em memória (zero-copy), e 100% compatível com a especificação de API do Jellyfin (Moonfin, Swiftfin, Findroid).

---

## 1. Regras Inegociáveis (Core Rules)

1. **PROIBIDO `unwrap()` ou `expect()` em Handlers e Tarefas em Background:**
   * Qualquer falha de I/O, banco ou parsing deve ser propagada via `?` ou tratada retornando um `Result<T, AppError>`.
2. **NUNCA Bloqueie o Runtime do Tokio:**
   * Nunca use `std::fs`, `std::thread::sleep`, chamadas bloqueantes de rede ou loops pesados de CPU dentro de funções `async`.
   * Se precisar de I/O síncrono ou processamento de mídia pesado, use `tokio::task::spawn_blocking`.
3. **Zero-Copy Streaming:**
   * **Nunca** carregue um arquivo de vídeo inteiro na memória (`Vec<u8>`).
   * Sempre use `tokio::fs::File`, `AsyncSeekExt`, e `tokio_util::io::ReaderStream` para streaming sob demanda em chunks.
4. **Contrato de API do Jellyfin (PascalCase Obrigatório):**
   * Todos os DTOs serializados para os clientes Jellyfin devem possuir `#[serde(rename_all = "PascalCase")]`.
   * Duração de mídia é sempre em **Ticks** ($1\text{ segundo} = 10.000.000\text{ ticks}$).
   * IDs de itens são strings hexadecimais de 32 caracteres (GUIDs sem traços) ou UUIDs padrão.

---

## 2. Padrões no Axum 0.7

### 2.1 Sintaxe de Parâmetros de Rota
No Axum 0.7, parâmetros de rota utilizam `:` e **NÃO** `{}`:
```rust
// ❌ INCORRETO (Sintaxe do Actix-Web, causa 404 no Axum)
.route("/Users/{user_id}/Items", get(get_items))

// ✅ CORRETO (Sintaxe do Axum)
.route("/Users/:user_id/Items", get(get_items))
.route("/Videos/:id/stream.:ext", get(stream_video))
```

### 2.2 Ordem Estrita de Extratores (Extractors)
O Axum exige que extratores que consomem o corpo da requisição (`Json`, `String`, `Bytes`) sejam **sempre o último argumento** da função:
```rust
// ✅ CORRETO: State e Path vêm antes do Json
pub async fn update_item(
    State(pool): State<DbPool>,
    Path(item_id): Path<String>,
    Json(payload): Json<UpdateItemRequest>, // <-- Último argumento
) -> Result<impl IntoResponse, AppError> { ... }
```

### 2.3 Tratamento de Erros Tipado com `IntoResponse`
Evite `map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)`. Em vez disso, utilize o enum de erro da aplicação:

```rust
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::json;

pub enum AppError {
    NotFound(String),
    DatabaseError(sqlx::Error),
    IoError(std::io::Error),
    BadRequest(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::DatabaseError(err) => {
                tracing::error!("Erro de banco de dados: {:?}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Erro interno de banco".to_string())
            }
            AppError::IoError(err) => {
                tracing::error!("Erro de I/O: {:?}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "Erro interno de arquivo".to_string())
            }
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
        };

        (status, Json(json!({ "Error": message }))).into_response()
    }
}
```

---

## 3. Padrões de Banco de Dados (SQLite + SQLx)

1. **Modo WAL Permanente:** O SQLite deve operar em `PRAGMA journal_mode = WAL;` e `PRAGMA synchronous = NORMAL;`. Isso permite leituras simultâneas ilimitadas enquanto escritas de progresso de reprodução ocorrem.
2. **Separação de Modelos (DB vs DTO):**
   * **`DbItemRow`**: Estrutura plana que reflete as colunas da tabela SQLite.
   * **`BaseItemDto`**: Estrutura complexa e aninhada exigida pelo cliente Jellyfin.
   * *Regra:* Mapeie explicitamente de `DbItemRow` para `BaseItemDto` em funções de conversão dedicadas.
3. **Upsert Atômico para Sessões:**
   * Progresso de reprodução deve usar `ON CONFLICT(user_id, item_id) DO UPDATE SET position_ticks = excluded.position_ticks`.

---

## 4. Padrões de Concorrência e Background Jobs

1. **Spawning de Tarefas:**
   ```rust
   // ✅ Background scan desacoplado da inicialização da API
   let scan_pool = pool.clone();
   let scan_dirs = config.media_directories.clone();
   tokio::spawn(async move {
       scanner::run_scan(&scan_pool, &scan_dirs).await;
   });
   ```
2. **Uso de Tracing:**
   * Nunca use `println!` em código de biblioteca/serviço.
   * Use os macros do crate `tracing`: `info!`, `warn!`, `error!`, `debug!`.
   * Configure spans contextuais para requisições de streaming se necessário.

---

## 5. Padrões de Streaming HTTP Range (RFC 7233)

Ao lidar com endpoints de vídeo (`/Videos/:id/stream`):
1. **Sempre retorne `Accept-Ranges: bytes`**.
2. **Se o cliente enviar o header `Range: bytes=START-END`**:
   * Responda com status **`206 Partial Content`**.
   * Calcule `Content-Length = END - START + 1`.
   * Envie o cabeçalho `Content-Range: bytes START-END/TOTAL`.
   * Faça `file.seek(SeekFrom::Start(START))` antes de canalizar o stream.
3. **Mime Types:** Retorne sempre o tipo correto (`video/x-matroska`, `video/mp4`, `video/webm`, etc.), nunca `text/plain`.

---

## 6. Checklist para Adicionar Novos Endpoints (Guia para LLMs)

Ao ser instruído a implementar uma nova funcionalidade ou rota da API do Jellyfin:

1. [ ] **Verificar a especificação da rota**: Ela deve ser adicionada em `src/api/<modulo>.rs`.
2. [ ] **Registrar a rota no Router principal**: Adicionar em `src/api/mod.rs` com `:param` (não `{param}`).
3. [ ] **Verificar serialização dos DTOs**: Garantir `#[serde(rename_all = "PascalCase")]` em structs e enums expostos.
4. [ ] **Verificar tipos de ticks**: Duração e progresso devem ser `i64` representando ticks (10.000.000 / segundo).
5. [ ] **Evitar clones profundos desnecessários**: Compartilhe estado com `Arc<AppConfig>` e `DbPool`.
6. [ ] **Executar `cargo check`**: Garantir zero erros e zero warnings de compilação.
