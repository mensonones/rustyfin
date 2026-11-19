# Solução de problemas

Para ver mais detalhes, aumente o nível de log: `RUST_LOG=rustyfin=debug` (no Docker, em `environment` do compose) e
leia com `docker compose logs -f`.

## O app não encontra o servidor

| Sintoma | Causa provável e solução |
|---|---|
| Não aparece na descoberta automática | Em **Docker**, defina `RUSTYFIN_PUBLIC_URL` com o IP do PC (ex.: `http://192.168.1.10:8096`) e publique a porta `7359/udp`. Confirme que o app e o servidor estão na mesma rede |
| O servidor não sobe e o log diz "a porta 8096 está ocupada" (ou `AddrInUse`) | Outro programa usa a porta: outro servidor de mídia ou o Rustyfin no Docker. Pare o outro serviço, ou mude `RUSTYFIN_PORT` no `rustyfin.env` e reinicie. Nos apps, use o endereço com a nova porta. No Windows, o ícone da bandeja mostra o aviso ao passar o mouse |
| Aparecia e sumiu | O Moonfin **esconde os servidores já salvos** da lista de descobertos. Remova o servidor antigo no app |
| Nem digitando o IP conecta | No Windows, [confira o perfil da rede e teste pelo navegador do celular](clientes.md#conexão-pelo-ip-no-windows). A conexão usa `8096/tcp` e a descoberta usa `7359/udp` |
| Conecta no PC, mas não no celular | Use o IP da rede (`192.168...`), não `localhost`; confira se o celular está no mesmo Wi-Fi e se o isolamento de clientes do roteador está desligado |

## Login

| Sintoma | Solução |
|---|---|
| "Muitas tentativas" | Aguarde 5 minutos. 5 senhas erradas bloqueiam o endereço e o usuário por esse tempo |
| `Admin` não entra de fora da rede | É proposital: contas sem senha só entram pela rede local. Defina uma senha |
| O app voltou para a tela de login | A sessão foi encerrada (painel → Sessões), a senha mudou ou o token expirou (`RUSTYFIN_TOKEN_TTL_DAYS`) |
| Esqueci a senha do administrador | Pelo painel, a partir da rede local: se não houver senha de administrador nem `RUSTYFIN_ADMIN_PASSWORD`, o painel abre sem senha e permite redefinir. Caso contrário, defina `RUSTYFIN_ADMIN_PASSWORD` e reinicie |

## Biblioteca

| Sintoma | Solução |
|---|---|
| A biblioteca está vazia | No Docker, a pasta precisa estar **montada como volume** e a biblioteca apontar para o caminho **dentro do container**. Confira em `/admin` → Bibliotecas e em Status |
| Um arquivo novo não aparece | O monitoramento leva alguns segundos; use "Escanear agora". Em unidades de rede, o evento de arquivo pode não chegar: o scan periódico é a reserva |
| Título errado ou sem capa | Renomeie para `Filme (2020).mkv` ou `Serie S01E02.mkv` (veja [nomes](configuracao.md#nomes-de-arquivo-e-pastas)); sem chave do TMDB só há metadados locais |
| Duas séries misturadas / uma série dividida | O ano separa séries de mesmo nome. Se só parte dos episódios tem o ano, use a mesma pasta e o mesmo padrão em todos |
| Indexar áudio ou ajustar uma legenda responde que a assinatura ainda está sendo calculada | Depois da primeira varredura o servidor calcula o hash de cada arquivo em segundo plano, um por vez (30 GB levam alguns minutos). Até lá essas ações respondem 409. O painel (`/admin/api/status`, `scan.signaturesPending`) mostra quantos faltam; tente de novo quando chegar a zero |
| Perdi o progresso depois de mexer nas pastas | O progresso é por item. Uma pasta indisponível **não** apaga o catálogo; se o arquivo realmente sumiu, o item é removido |

## Reprodução

| Sintoma | Solução |
|---|---|
| Demora a iniciar na TV Samsung | Veja [clientes](clientes.md#moonfin-para-samsung-tizen). Confirme nos logs (`RUST_LOG=rustyfin=debug`) se o plano foi Direct Play ou HLS |
| O avanço volta ao início | Acontecia com playlists HLS que só cresciam; transcodificações completas agora aceitam avanço livre. Com vídeo copiado (remux) o avanço fica limitado ao trecho já gerado |
| "Mídia não suportada" | O arquivo precisa ser transcodificado e o `ffmpeg` não está disponível: veja Status → ffmpeg |
| Erro 503 ao tocar | Atingiu o limite de transcodificações simultâneas (`RUSTYFIN_MAX_TRANSCODES`, também no painel) |
| Transcode lento | `libx264` roda na CPU. Com GPU Intel ou AMD no Linux, repasse `/dev/dri` ao container e o encoder `auto` a usa (veja [aceleração por hardware](reproducao.md#aceleração-por-hardware)). O log diz qual encoder está em uso e por que a GPU foi recusada |
| A GPU não é usada (o log diz `encoder libx264`) | Rode `docker compose exec rustyfin sh /app/gpu-check.sh`: ele mostra se o dispositivo existe, se há permissão e se a codificação funciona. Sem `/dev/dri`, rode `sh tools/gpu-setup.sh`. No Docker Desktop (Windows) a GPU Intel não chega ao container: rode o Rustyfin direto no Windows |
| O log diz "refazendo em software" | O ffmpeg com GPU falhou numa sessão e o servidor voltou para o processador. O erro do ffmpeg aparece logo acima no log; se for recorrente, defina `RUSTYFIN_VIDEO_ENCODER=libx264` e relate |
| Capas e fundos quebrados na TV Samsung (Moonfin) | A TV pede as imagens sem token. Confirme que `RUSTYFIN_IMAGENS_ANONIMAS` não está `false` e que o servidor vê a TV como rede local (atrás de proxy ele vê o IP do proxy). Com `RUST_LOG=rustyfin=debug`, o log mostra cada pedido recusado (`401: requisição sem token`) com o caminho e o aparelho |
| Sem legenda | SRT precisa ter o mesmo nome do vídeo. Legendas de imagem (PGS/DVD) só aparecem queimadas, o que força transcode |

## Metadados (TMDB)

| Sintoma | Solução |
|---|---|
| Nenhum metadado online | Configure a chave em `/admin` → Configurações (ou `TMDB_API_KEY`). O Status mostra "configurado" |
| "Chave recusada" nos logs | Use a chave **v3** (32 caracteres) ou o token **v4** do TMDB |
| Sem conexão | O servidor usa o `curl`; confirme que ele está no `PATH` e que há acesso à internet. A busca é refeita no próximo ciclo |

Se as capas geradas continuarem depois de configurar a chave, procure por `itens enriquecidos` e
`TMDB: sem conexão` no log. Para verificar o acesso à API sem expor a chave, execute
`curl.exe -4 -I https://api.themoviedb.org/3/configuration` no Windows. Uma resposta HTTP `401`
nesse teste é esperada porque a requisição foi feita sem credenciais. Após restabelecer a conexão,
inicie uma varredura no painel para buscar os metadados pendentes.

## Painel

| Sintoma | Solução |
|---|---|
| 403 ao abrir `/admin` | O acesso é só da rede local (e, se houver senha de administrador, exige login). Atrás de um proxy, o servidor vê o IP do proxy |
| O seletor de pastas só mostra `/media` | No Docker ele mostra as pastas **do container**. Monte a pasta do PC como volume |

## Desempenho

- A varredura usa vários `ffprobe` em paralelo; em disco lento ou rede, reduza com `RUSTYFIN_SCAN_CONCURRENCY`.
- No Windows com Docker, montar `C:\` é lento por causa do WSL2; copiar a mídia para dentro do WSL melhora a leitura.

Não resolveu? Abra uma *issue* com a versão (`/System/Info/Public`), o app e o trecho relevante do log.
