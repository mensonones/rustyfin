# Migrando do Jellyfin

O Rustyfin fala a mesma API que os apps do Jellyfin usam, então **os apps continuam os mesmos**. O que muda é o servidor:
um único binário que sobe em menos de um segundo e usa cerca de 10 MB de memória, em vez de ~400 MB.

Esta página diz, sem enfeite, **o que você ganha, o que perde e como trocar**, para você decidir com os olhos abertos. Os
números vêm do [comparativo de desempenho](desempenho.md).

## Vale a pena trocar?

### Troque se você...

- usa o Jellyfin só para **assistir filmes e séries** da sua rede, com Moonfin, Swiftfin ou Findroid;
- roda em um **mini PC, NAS, VM pequena ou container com pouca memória** e quer o servidor "invisível";
- quer **reiniciar e atualizar sem esperar** (0,8 s contra 2,6 s a quente) e **varrer a biblioteca rápido** (30 GB em 3 s
  contra 58 s; 1.000 arquivos numa pasta em 15 s contra 413 s);
- tem uma **GPU Intel** e quer transcodificar por hardware sem configurar nada além de um script.

### Fique no Jellyfin se você...

- depende de **plugins** (Trakt, Intro Skipper, OpenSubtitles etc.): o Rustyfin não tem sistema de plugins;
- usa **TV ao vivo, gravação (DVR), DLNA, ou SyncPlay**: nada disso existe aqui;
- precisa de **GPU NVIDIA**: o foco é Intel e AMD; a NVIDIA não está testada;
- precisa de **muitos usuários e acesso pela internet** com todo o ecossistema de contas, quotas e Quick Connect: o
  Rustyfin é um servidor de rede doméstica;
- depende de **trickplay** (miniaturas ao passar o mouse na barra de progresso) ou de capítulos com imagens.

Se a sua lista de "preciso" cabe na primeira, vale testar. E testar não custa nada, porque os dois podem rodar **lado a
lado** (veja abaixo).

## O que o Rustyfin tem a mais

| Recurso | O que faz |
|---|---|
| **[Ajuste de legendas SRT](reproducao.md#corrigir-uma-faixa-srt)** | Corrija atraso fixo ou desvio progressivo de uma legenda direto no painel, sem editar o arquivo |
| **[Revisão do acervo](arquitetura.md#revisar-alterações-do-acervo)** | Troquei um filme por outra edição: transfira favoritos, "assistido" e a posição de retomada para o arquivo novo, inclusive sugerindo pela trilha de áudio |
| **[Versões preparadas](reproducao.md#áudio-e-versões-preparadas)** | O servidor recomenda converter para MP4 o que sempre cai em transcode, e faz isso quando está ocioso |
| **[Diagnóstico antes do Play](reproducao.md)** | Compare o plano de reprodução (Direct Play, remux ou transcode) de dois aparelhos para um mesmo título |
| **Painel `/admin`** | Tema claro e escuro, paleta de comandos (`Ctrl K`) e avisos só do que precisa de atenção |

## Como trocar sem risco

Dá para testar o Rustyfin **sem desligar nem alterar o Jellyfin**.

1. **Instale ao lado.** Siga os [primeiros passos](primeiros-passos.md). Mude a porta se o Jellyfin já usa a 8096, por
   exemplo `8097:8096` em `ports`, e ajuste `RUSTYFIN_PUBLIC_URL`. Se os dois estiverem ligados, a descoberta automática
   (UDP 7359) pode mostrar os dois ou só um: adicione o Rustyfin digitando o endereço.
2. **Aponte para as mesmas pastas, em somente leitura.** Monte a mesma pasta de mídia com `:ro` (`D:/Videos:/mnt/videos:ro`).
   O Rustyfin só lê os arquivos. Os dois servidores convivem sem se atrapalhar.
3. **Recrie os usuários.** Em `/admin` → **Usuários**. Senhas, progresso e favoritos **não são importados** do Jellyfin:
   cada servidor guarda os seus.
4. **Adicione o servidor no mesmo app.** Os apps aceitam vários servidores. Use uma semana; se gostar, desligue o Jellyfin.
5. **Metadados.** Ponha a chave do TMDB (a mesma que você já usa serve) em `/admin` → **Configurações**. Capas locais
   (`poster.jpg`, `fanart.jpg`, `folder.jpg`) são respeitadas.

## Equivalências rápidas

| No Jellyfin | No Rustyfin |
|---|---|
| Painel → Bibliotecas | `/admin` → **Bibliotecas** |
| Painel → Usuários | `/admin` → **Usuários** (senha com Argon2, acesso por biblioteca, perfil oculto, foto) |
| Painel → Dispositivos | `/admin` → **Sessões** |
| Painel → Reprodução → Transcodificação | `RUSTYFIN_VIDEO_ENCODER` e `RUSTYFIN_MAX_TRANSCODES` ([aceleração por hardware](reproducao.md#aceleração-por-hardware)) |
| Painel → Painel de controle / Logs | `/admin` → **Diagnóstico** e `docker compose logs -f` |
| Volume `/config` | `./data` (banco e cache) |
| Volume `/media` | Qualquer pasta montada, de preferência `:ro` |
| Atualizar a imagem | `git pull && docker compose up -d --build` |

## Quanto a gente sabe que funciona

Os apps foram testados em aparelho: Moonfin (Android e Samsung Tizen) está validado; Swiftfin e Findroid devem funcionar,
mas ainda não foram validados. A lista está em [clientes](clientes.md). Se você testar um app que não está lá, conte em uma
*issue*: ajuda todo mundo.
