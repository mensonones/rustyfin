# Instruções para agentes (Rustyfin)

Este arquivo orienta agentes de IA (Codex, Claude Code e compatíveis) e pessoas que contribuem com o
Rustyfin. Ele incorpora as diretrizes do
[`AGENTS.md` do projeto *rust-skills*](https://github.com/actionbook/rust-skills/blob/main/AGENTS.md),
cujo texto original está no [apêndice](#apêndice-texto-original-do-rust-skills), e as combina com as
regras específicas deste servidor de mídia. As habilidades (*skills*) citadas por ele estão em
[`skills/`](skills/NOTICE.md). Em caso de conflito, valem as regras do Rustyfin.

## Leia antes de alterar o código

1. [`RUST_STANDARDS.md`](RUST_STANDARDS.md): regras de arquitetura (Axum 0.7, Tokio, *streaming*, contrato do Jellyfin).
2. [`.cursorrules`](.cursorrules): resumo das regras acima.
3. [`CONTRIBUTING.md`](CONTRIBUTING.md): padrão de commits e de versões.
4. [`ROADMAP.md`](ROADMAP.md): o que já existe e o que falta.

## Configuração do projeto

O `Cargo.toml` já adota estes padrões; mantenha-os em qualquer arquivo ou *crate* novo:

- `edition = "2024"` e `rust-version = "1.85"`.
- `[lints.rust] unsafe_code = "warn"`.
- `[lints.clippy] all = "warn"` e `pedantic = "warn"`.

O `pedantic` tem poucas exceções, todas comentadas no `Cargo.toml` (conversões numéricas de *ticks* e
tamanhos, DTOs do Jellyfin com várias flags, extratores do Axum recebidos por valor). Não acrescente
`#[allow(...)]` para silenciar um aviso: corrija o código ou, se a exceção for justa, explique o motivo
no `Cargo.toml`. O projeto deve terminar `cargo clippy --all-targets` **sem avisos**.

## Estilo de código

- `snake_case` para funções e variáveis, `PascalCase` para tipos e *traits*, `SCREAMING_SNAKE_CASE` para constantes.
- Linhas de até 100 colunas no código novo.
- Nomes, comentários e mensagens ao usuário em português, como no resto do projeto.

## Tratamento de erros

- Prefira `?` a `unwrap()`. Em bibliotecas e módulos internos, propague `Result`.
- **Nunca** use `unwrap()` ou `expect()` em *handlers* HTTP nem em tarefas de segundo plano
  (regra do `RUST_STANDARDS.md`). Nos testes é aceitável.
- Converta erros nas bordas: `map_err` para o tipo da camada, e `StatusCode`/`Response` nos *handlers*.

```rust
// Bom
let row = sqlx::query_as::<_, Item>("SELECT ...").fetch_optional(&pool).await.map_err(db_err)?;

// Evite
let row = sqlx::query_as::<_, Item>("SELECT ...").fetch_optional(&pool).await.unwrap();
```

## Código `unsafe`

O projeto não usa `unsafe`. Se for indispensável, cada bloco `unsafe` precisa de um comentário
`// SAFETY:` que explique por que o contrato foi respeitado, e a mudança deve ser justificada no commit.

## Concorrência e Tokio

- Não bloqueie o *runtime*: use `tokio::fs` e `tokio::process`, e `spawn_blocking` para trabalho síncrono pesado.
- Dados compartilhados entre tarefas: `Arc` com `Mutex`/`RwLock` (do Tokio quando o *lock* atravessa um `.await`).
- Nunca carregue um vídeo inteiro em memória: use `ReaderStream` e respostas HTTP 206.

## Erros comuns do compilador

| Código | Causa | Correção |
| --- | --- | --- |
| E0382 | uso de valor já movido | `clone()`, emprestar com `&` ou reorganizar a posse |
| E0597 | referência vive mais que o dono | estender o escopo do dono ou devolver o valor próprio |
| E0502 | empréstimo mutável e imutável ao mesmo tempo | encurtar o escopo de um deles |
| E0499 | dois empréstimos mutáveis | dividir a estrutura ou usar `RefCell`/`Mutex` quando fizer sentido |
| E0277 | *trait* não implementada | derivar ou implementar a *trait*; no Tokio, conferir `Send` entre `.await` |

## Fluxo de trabalho

- Rode e teste sempre pelo Docker, com o `ffmpeg` real da imagem (`docker compose up -d --build`).
- Depois de qualquer alteração: `cargo check`; antes de concluir: `cargo clippy --all-targets` e `cargo test --locked`.
- O `probe` de cliente (`python tools/client_probe.py http://127.0.0.1:8096`) simula o fluxo de um app Jellyfin.
- Commits em [Conventional Commits](https://www.conventionalcommits.org/pt-br/v1.0.0/) em português
  (veja o `CONTRIBUTING.md`). Mudanças de comportamento sobem a versão e entram no `CHANGELOG.md`.
- Não inclua linha de coautor nos commits, nem segredos (`.env`, chaves, tokens) no repositório.

## Apêndice: texto original do rust-skills

> Cópia do `AGENTS.md` do [rust-skills](https://github.com/actionbook/rust-skills) v2.0.9 (MIT), sem alterações além do nível dos títulos. As regras do Rustyfin acima têm prioridade.

### Rust Skills - Agent Instructions

> For OpenAI Codex and compatible agents

#### Default Project Settings

When creating Rust projects or Cargo.toml files, ALWAYS use:

```toml
[package]
edition = "2024"
rust-version = "1.85"

[lints.rust]
unsafe_code = "warn"

[lints.clippy]
all = "warn"
pedantic = "warn"
```

#### Core Capabilities

##### 1. Question Routing
Route Rust questions to appropriate skills:
- Ownership/borrowing → m01-ownership
- Smart pointers → m02-resource
- Error handling → m06-error-handling
- Concurrency → m07-concurrency
- Unsafe code → unsafe-checker

##### 2. Code Style
Follow Rust coding guidelines:
- Use snake_case for variables and functions
- Use PascalCase for types and traits
- Use SCREAMING_SNAKE_CASE for constants
- Max line length: 100 characters
- Use `?` operator instead of `unwrap()` in library code

##### 3. Error Handling
```rust
// Good: Use Result with context
fn read_config() -> Result<Config, ConfigError> {
    let content = std::fs::read_to_string("config.toml")
        .map_err(|e| ConfigError::Io(e))?;
    toml::from_str(&content)
        .map_err(|e| ConfigError::Parse(e))
}

// Avoid: unwrap() in library code
fn read_config() -> Config {
    let content = std::fs::read_to_string("config.toml").unwrap(); // Bad
    toml::from_str(&content).unwrap() // Bad
}
```

##### 4. Unsafe Code
Every `unsafe` block MUST have a `// SAFETY:` comment:
```rust
// SAFETY: We checked that index < len above, so this is in bounds
unsafe { slice.get_unchecked(index) }
```

##### 5. Common Error Fixes

| Error | Cause | Fix |
|-------|-------|-----|
| E0382 | Use of moved value | Clone, borrow, or use reference |
| E0597 | Lifetime too short | Extend lifetime or restructure |
| E0502 | Borrow conflict | Split borrows or use RefCell |
| E0499 | Multiple mut borrows | Restructure to single mut borrow |
| E0277 | Missing trait impl | Add trait bound or implement trait |

#### Quick Reference

##### Ownership
- Each value has one owner
- Borrowing: `&T` (shared) or `&mut T` (exclusive)
- Lifetimes: `'a` annotations for references

##### Smart Pointers
- `Box<T>`: Heap allocation
- `Rc<T>`: Reference counting (single-threaded)
- `Arc<T>`: Atomic reference counting (thread-safe)
- `RefCell<T>`: Interior mutability

##### Concurrency
- `Send`: Safe to transfer between threads
- `Sync`: Safe to share references between threads
- `Mutex<T>`: Mutual exclusion
- `RwLock<T>`: Reader-writer lock

##### Async
```rust
#[tokio::main]
async fn main() {
    let handle = tokio::spawn(async {
        // async work
    });
    handle.await.unwrap();
}
```

#### Skill Files

For detailed guidance, see:
- `skills/rust-router/SKILL.md` - Question routing
- `skills/coding-guidelines/SKILL.md` - Code style rules
- `skills/unsafe-checker/SKILL.md` - Unsafe code review
- `skills/m01-ownership/SKILL.md` - Ownership concepts
- `skills/m06-error-handling/SKILL.md` - Error patterns
- `skills/m07-concurrency/SKILL.md` - Concurrency patterns
