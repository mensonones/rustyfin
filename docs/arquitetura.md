# Arquitetura

O Rustyfin é **um único binário** (`rustyfin`) sobre uma biblioteca de mesmo nome. Ele combina um servidor HTTP
(Axum/Tokio), um banco SQLite em modo WAL e o `ffmpeg` como processo externo.

```
 apps Jellyfin ──HTTP──▶  Axum (src/api)  ──▶  SQLite (WAL)
 (Moonfin, ...)              │    │
                             │    └──────────▶  ffmpeg / ffprobe  (processos filhos)
 painel /admin ──────────────┘
                              ▲
   scanner + watcher ─────────┘   TMDB (via curl)
```

## Estrutura do código

| Caminho | Responsabilidade |
|---|---|
| `src/main.rs` | Inicialização: logs, configuração, banco, tarefas em segundo plano e servidor HTTP |
| `src/lib.rs` | Declara os módulos da biblioteca |
| `src/config.rs` | Variáveis de ambiente |
| `src/settings.rs` | Configurações editáveis em tempo de execução (painel), guardadas no banco |
| `src/db.rs` | Conexão SQLite, criação e migração de tabelas |
| `src/libraries.rs` | Bibliotecas e pastas; regra "pasta mais específica" |
| `src/scanner.rs` | Varredura, `ffprobe`, imagens locais, identidade de séries e temporadas |
| `src/library.rs` | Laço do scan automático e monitoramento de pastas (`notify`) |
| `src/parser.rs` | Interpreta nomes de arquivo e de pastas (título, ano, temporada, episódio) |
| `src/metadata.rs` | TMDB: busca, elenco, estúdios, classificação, trailers e imagens |
| `src/users.rs` | Usuários, senhas (Argon2), tokens, sessões e permissões por biblioteca |
| `src/discovery.rs` | Descoberta automática (UDP 7359) |
| `src/models.rs` | DTOs do contrato Jellyfin (PascalCase) |
| `src/transcode/` | Decisão Direct Play/Remux/Transcode, argumentos do ffmpeg (software, VAAPI e Quick Sync), escolha do encoder na subida, processos e sessões HLS |
| `src/api/` | Rotas: itens, séries, imagens, streaming, transcodificação, legendas, usuários, painel |
| `src/bin/rustyfin-tray.rs` | Ícone da bandeja do Windows: inicia o servidor sem janela, o mantém no ar e oferece o menu (só no Windows) |
| `src/shutdown.rs` | Sinal interno de desligamento gracioso, usado pela rota `/admin/api/desligar` |
| `build.rs` | Embute o ícone (a logo) e as informações de versão nos executáveis do Windows |
| `packaging/` | Instaladores de serviço (Windows, Linux, macOS) e o modelo `rustyfin.env`; vão dentro dos pacotes de cada versão |
| `src/admin.html` | Painel de administração (página única, embutida no binário) |
| `src/assets/fontes/` | Fontes Geist e Geist Mono (OFL), embutidas e servidas em `/assets/fontes/` sem autenticação |
| `tests/` | Testes de integração do roteador |
| `tools/client_probe.py` | Simula o fluxo de um app Jellyfin contra um servidor em execução |

## Fluxo de uma requisição

1. O roteador (`api::build_router`) aplica o CORS, o *log* de requisições e a **autenticação**: toda rota exige token,
   exceto as [públicas](usuarios-e-seguranca.md#o-que-não-exige-token).
2. O *handler* extrai o usuário (`CurrentUser`), confere o acesso à biblioteca do item e consulta o SQLite.
3. Respostas de mídia **não carregam o arquivo na memória**: usam `ReaderStream` com leitura em blocos.
4. Trabalho pesado ou bloqueante (varredura de pastas, hash de senha) roda em `spawn_blocking`; processos externos usam
   `tokio::process` com `kill_on_drop`.

## Banco de dados

SQLite com `journal_mode = WAL`, criado e migrado na inicialização (`db::init_db`), sem ferramenta externa de migração.

| Tabela | Conteúdo |
|---|---|
| `items` | Um registro por arquivo de vídeo: caminho, contêiner, duração, codecs, trilhas (JSON), identificação da série, biblioteca, dados do TMDB |
| `series`, `season_art` | Dados e imagens de séries e temporadas (os itens `Series`/`Season` são **virtuais**, derivados dos episódios) |
| `playback_state` | Progresso, assistido, favorito e contagem, **por usuário e item** |
| `libraries`, `library_paths` | Bibliotecas e suas pastas |
| `users`, `user_libraries`, `access_tokens` | Contas, acesso por biblioteca e um token por dispositivo |
| `people` | Foto (caminho no TMDB) de cada pessoa do elenco |
| `settings` | Configurações do painel |

Os IDs dos itens são UUIDs v5 do caminho do arquivo, então reindexar não os muda. Os IDs de séries e temporadas são
UUIDs v5 do nome (e do ano, quando há) normalizado.

## Cache em disco (`RUSTYFIN_CACHE_DIR`)

| Pasta | Conteúdo |
|---|---|
| `thumbs/` | Quadros de vídeo usados como capa e fundo |
| `resized/` | Imagens redimensionadas sob demanda |
| `tmdb/` | Imagens baixadas do TMDB (inclui fotos de pessoas) |
| `subs/` | Legendas extraídas |
| `avatars/` | Fotos de perfil dos usuários |
| `transcodes/` | Sessões HLS (apagadas ao iniciar e quando ociosas) |

## Varredura

1. `library::spawn_autoscan` roda a cada intervalo, ao iniciar, sob demanda (`/Library/Refresh`) e quando o *watcher*
   detecta mudanças de arquivo.
2. `scanner::run_scan` lista os vídeos de cada pasta (em `spawn_blocking`) e processa vários arquivos em paralelo
   (`ffprobe`). Arquivos inalterados (tamanho e data) só atualizam nome e imagens.
3. Antes de descartar itens ausentes, o scanner tenta reconhecer arquivos movidos pelo conteúdo. Só reconcilia
   caminhos ou remove itens após uma varredura completa, para preservar o catálogo se uma pasta ficar indisponível.
4. Depois, `metadata::enrich_pending` busca o TMDB (em lotes paralelos) e as miniaturas ausentes são geradas em
   segundo plano.

### Identidade de arquivos movidos

Na primeira varredura após a atualização, cada vídeo recebe uma impressão SHA-256 persistida no
SQLite. A leitura é sequencial, em blocos, e pode aumentar o tempo desse primeiro scan.
Varreduras seguintes reaproveitam a impressão enquanto o tamanho e a data de alteração
continuarem iguais.

Depois de mover um vídeo, o próximo scan mantém seu ID se encontrar uma única origem
catalogada ausente e um único destino novo com a mesma impressão. O progresso e os favoritos
continuam associados ao ID. O destino assume as permissões da biblioteca em que foi encontrado.
Faça pelo menos uma varredura antes de mover arquivos que já estavam catalogados.

Cópias que já foram indexadas separadamente mantêm IDs próprios. A associação só ocorre em
uma varredura completa. Arquivos reencodados, editados ou com metadados internos alterados
têm outra impressão digital.

### Revisar alterações do acervo

Em **Bibliotecas → Revisar alterações**, o painel mostra arquivos substituídos no mesmo caminho,
origens ausentes com cópias conhecidas e possíveis edições identificadas pelo nome de filme ou
pelos dados de série, temporada e episódio. Quando ambos os anos são conhecidos, eles precisam
coincidir para o scanner sugerir uma edição em outro caminho. Cada revisão lista até 12 destinos.

A fila guarda uma cópia dos favoritos e das marcações de assistido dos perfis. Ao confirmar um
destino, o Rustyfin combina esses marcadores com os que já existem nele e preserva a posição
do destino por padrão. A contagem de reproduções e os ajustes de legenda ficam fora da
transferência. **Manter separados** encerra a revisão e descarta a cópia dos
marcadores da origem. As duas ações deixam os arquivos de mídia intactos.

Em **Mapear retomada**, informe os tempos de duas cenas correspondentes nas duas edições.
O painel interpola posições entre as referências e permite confirmar ou editar a posição por
perfil. Para uma cena cortada ou fora do intervalo, informe o tempo manualmente. A duração
do destino precisa ser conhecida. Posições que já existem no destino ficam desmarcadas por
padrão e mudanças feitas após a prévia exigem nova confirmação. Essa opção só está disponível
para revisões criadas após a atualização que passaram a registrar a posição da origem.

## Decisões de projeto

- **Compatibilidade antes de tudo:** os DTOs seguem o contrato do Jellyfin e, onde um cliente exige um comportamento
  específico (como o contêiner `mov` para a TV Samsung), o servidor o reproduz.
- **Leve por construção:** nada de coleta de lixo, uma só conexão por assunto, caches em disco e imagens sob demanda.
- **Sem dependências de runtime além do `ffmpeg` e do `curl`:** o cliente HTTP do TMDB é o `curl`, para não puxar uma
  pilha TLS dentro do binário.
- **Erros nas bordas:** `?` e `Result` por dentro; `StatusCode` nos *handlers*. Sem `unwrap()` em rotas.
