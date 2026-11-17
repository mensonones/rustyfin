# Clientes

O Rustyfin implementa a API de clientes Jellyfin 10.9. Qualquer app compatível deve se conectar; esta página registra o
que foi **efetivamente testado** e as particularidades de cada um.

## Situação

| Cliente | Plataforma | Situação |
|---|---|---|
| **Moonfin 3.x** | Android | Testado em aparelho: conexão, login, home, bibliotecas, séries (temporada → episódio), "Próximo episódio", Direct Play com avanço, HLS por limite de bitrate, legendas SRT, marcar como assistido e descoberta automática |
| **Moonfin for Tizen 3.1** | Samsung (Tizen 6) | Testado em TV: login, Direct Play e avanço. O avanço em HLS ainda não foi testado na TV |
| **Swiftfin** | iOS, tvOS | Ainda sem validação em aparelho |
| **Findroid** | Android | Ainda sem validação em aparelho |
| Simulador de cliente | `tools/client_probe.py` | 63 rotas do fluxo de um app (descoberta, login, tela inicial, biblioteca, detalhes, reprodução com `Range`, marcar assistido e favorito) |

Rotas e recursos que o servidor não implementa (plugins, SyncPlay, DLNA, jogos, trickplay...) respondem com listas vazias
ou `404`, que os apps tratam sem erro. `Moonfin/*` e `Plugins/*` retornam `404` de propósito, pois o plugin de servidor
não está instalado.

## Como conectar

1. Descubra o IP do PC (`ipconfig` no Windows, `ip addr` no Linux).
2. No Windows, permita o executável (ou o Docker) no Firewall para **rede privada**; as portas são `8096/tcp` e
   `7359/udp`.
3. No app, adicione o servidor `http://<ip-do-pc>:8096` (ou escolha-o na descoberta automática).
4. Entre com um usuário. O `Admin` inicial não tem senha e só entra a partir da rede local.

O celular ou a TV precisam estar **na mesma rede** que o servidor.

### Descoberta automática

O servidor responde ao broadcast UDP `who is JellyfinServer?` na porta 7359 com o nome e o endereço dele.

- Em **Docker**, defina `RUSTYFIN_PUBLIC_URL` (por exemplo `http://192.168.1.10:8096` no `.env`), pois o container não
  sabe o IP do PC.
- O Moonfin **esconde da lista de descobertos os servidores já salvos**. Se o servidor não aparece, remova o antigo.

## Particularidades

### Moonfin para Samsung (Tizen)

- O player da TV só toca bem arquivos MP4 quando a URL termina em `.mov` (o servidor já informa isso), e só lê por
  trechos quando o servidor **não** envia `DirectStreamUrl`. O Rustyfin segue o mesmo comportamento do Jellyfin.
- A TV envia um perfil próprio ("Moonfin Tizen 6"); com `RUST_LOG=rustyfin=debug` o log mostra o perfil recebido e a decisão tomada.
- Se o Direct Play falha, o app cai para HLS depois de um tempo: lento para iniciar e, se o arquivo for
  recodificado, com a playlist completa para permitir o avanço.

### Moonfin para Android

- Mostra os destaques da home usando o fundo (`Backdrop`) de cada item: sem um, o servidor gera um quadro do vídeo.
- Usa um WebSocket em `/socket`; o servidor o aceita e ignora as mensagens.

### Qualquer app

- Para reproduzir, o app precisa conseguir alcançar o servidor pelo endereço que **ele mesmo** usou ao conectar.
- Os tokens são por dispositivo: trocar a senha ou encerrar a sessão no painel leva o aparelho de volta à tela de login.

Encontrou um app que não funciona? Veja [solução de problemas](solucao-de-problemas.md) e abra uma *issue* informando o
app, a versão e o trecho do log.
