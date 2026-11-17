# Desenvolvimento

Este guia é para quem vai alterar o código. Para o padrão de commits e de versões, veja o
[CONTRIBUTING.md](../CONTRIBUTING.md); para agentes de IA, o [AGENTS.md](../AGENTS.md).

## Requisitos

- **Rust 1.85 ou mais novo** (edição 2024), com `rustup`.
- `ffmpeg` e `ffprobe` no `PATH`, para exercitar transcode, legendas e miniaturas.
- `curl`, para o TMDB.
- Docker, para rodar o servidor como ele é distribuído (a imagem inclui o `ffmpeg`).
- Python 3.8 ou mais novo, para o *hook* de commits e para `tools/client_probe.py`.

## Compilar e rodar

```bash
cargo run                       # desenvolvimento
cargo build --release           # binário otimizado em target/release/rustyfin

docker compose up -d --build    # como o servidor é distribuído
docker compose logs -f
```

> Teste sempre pelo Docker antes de concluir uma mudança: é nele que o `ffmpeg` real está disponível e que o
> comportamento chega aos apps.

## Verificações

```bash
cargo check
cargo clippy --locked --all-targets -- -D warnings   # o projeto deve ficar sem avisos
cargo test --locked                                  # unitários e de integração
```

| Tipo | Onde | O que cobre |
|---|---|---|
| Unitários | no próprio arquivo (`#[cfg(test)]`) | Parser de nomes, decisão de reprodução, argumentos do ffmpeg, Range, usuários, filtros |
| Integração | `tests/` | O roteador completo com um SQLite temporário: rotas públicas, token obrigatório, perfis ocultos, sessões |
| Fluxo de um app | `tools/client_probe.py` | Sequência de um cliente Jellyfin contra um servidor em execução |

```bash
python tools/client_probe.py http://127.0.0.1:8096
```

O Clippy roda com `all` e `pedantic` em aviso (edição 2024); as poucas exceções estão comentadas no `Cargo.toml`.
A CI (GitHub Actions) executa o Clippy com `-D warnings` e os testes.

## Estrutura

Veja a [arquitetura](arquitetura.md#estrutura-do-código). Em resumo: `src/lib.rs` expõe os módulos, `src/main.rs` só
inicializa, `src/api/` tem as rotas e `src/transcode/` a reprodução. Arquivos que passam de ~700 linhas viram diretório de
módulo.

## Regras do código

As regras de arquitetura estão em [RUST_STANDARDS.md](../RUST_STANDARDS.md). As principais:

1. Sem `unwrap()`/`expect()` em *handlers* nem em tarefas de segundo plano.
2. Nunca bloquear o *runtime* do Tokio (`spawn_blocking` para trabalho síncrono).
3. Nunca carregar um vídeo inteiro na memória: `ReaderStream` e `Range`.
4. DTOs do Jellyfin com `#[serde(rename_all = "PascalCase")]`; durações em ticks.
5. Rotas do Axum 0.7 usam `:param` (não `{param}`).
6. SQLite sempre em modo WAL.

## Adicionando uma rota

1. Escreva o *handler* em `src/api/<area>.rs`, com `CurrentUser` quando precisar do usuário e
   `users::item_allowed` para respeitar o acesso por biblioteca.
2. Registre a rota em `api::build_router` (`src/api/mod.rs`). Se for pública, ajuste `is_public` em `src/api/auth.rs`.
3. Cubra com um teste unitário e, se mexer em autenticação ou listas de usuários, com um teste em `tests/`.
4. Documente em [api.md](api.md).

## Banco de dados

As tabelas são criadas e migradas em `db::init_db`. Para uma coluna nova, use `add_column_if_missing`; não há
ferramenta de migração externa. Valores derivados (como o TMDB de itens antigos) são completados em segundo plano.

## Versões e lançamento

- Versionamento semântico: PATCH para correções, MINOR para funcionalidades (na série `0.x`, mudanças incompatíveis
  também sobem o MINOR).
- Agrupe as mudanças no [CHANGELOG](../CHANGELOG.md), atualize a versão em `Cargo.toml` e `Cargo.lock` e crie uma tag
  anotada `vX.Y.Z` no commit da versão.
- O commit de versão usa `chore(release): prepara a versão X.Y.Z`.

## Imagens do projeto

O logotipo e os ícones estão em `docs/assets/`. `src/assets/` guarda as versões embutidas no binário (ícone do painel).
