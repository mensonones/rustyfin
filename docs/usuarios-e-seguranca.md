# Usuários e segurança

## Contas

- Na primeira execução é criado o usuário **`Admin`**, **sem senha**. Defina uma senha em `/admin` → **Usuários**.
- Cada usuário tem **progresso, favoritos e "continuar assistindo" próprios**.
- Cada usuário pode ter acesso a **todas as bibliotecas** ou só a algumas (por exemplo, um perfil infantil só com Filmes).
- Sempre sobra pelo menos **um administrador ativo**: o servidor recusa excluir, desativar ou rebaixar o último.
- Um usuário pode ser **desativado** (não entra) ou **oculto** (some da lista de perfis da tela de login, mas entra
  digitando o nome).
- Cada usuário pode ter uma **foto de perfil** (JPEG, PNG, WebP ou GIF, até 8 MB), enviada pelo app ou pelo painel.

## Senhas e login

- Senhas são guardadas com **Argon2**.
- Cada login gera um **token por dispositivo**. O token é revogado ao trocar a senha, desativar a conta, sair ou quando
  um administrador encerra a sessão.
- **5 senhas erradas** bloqueiam novas tentativas daquele endereço e usuário por 5 minutos.
- **Contas sem senha só entram a partir da rede local.** De um IP público o login é recusado, então defina uma senha antes
  de expor o servidor à internet.
- Dispositivos parados podem expirar com `RUSTYFIN_TOKEN_TTL_DAYS` (por padrão, nunca expiram).

## Sessões e dispositivos

Em `/admin` → **Sessões** aparecem os aparelhos conectados (nome, app, usuário e último uso). Dá para encerrar uma
sessão ou todas; o aparelho encerrado volta à tela de login. O token nunca é exibido nem retornado pela API.

## Painel de administração

O painel (`/admin`) e as rotas de biblioteca do Jellyfin (`/Library/VirtualFolders*`) seguem estas regras:

1. Só aceitam clientes de **rede privada ou loopback** (10.x, 172.16-31.x, 192.168.x, 127.x, link-local, IPv6 local).
2. Se existir um **administrador com senha** ou `RUSTYFIN_ADMIN_PASSWORD`, exigem login: HTTP Basic com usuário e senha
   de administrador, ou o token de um administrador logado no app.
3. Sem nenhuma senha de administrador, vale só a regra de rede local.

Atrás de um *proxy reverso*, o servidor enxerga o endereço do proxy; configure o acesso externo com cuidado.

## O que não exige token

Para os apps funcionarem, algumas rotas são públicas:

| Rota | Motivo |
|---|---|
| `GET /System/Info/Public`, `/System/Ping`, `/Branding/*`, `/QuickConnect/Enabled` | Descoberta e tela de conexão |
| `GET /Users/Public`, `POST /Users/AuthenticateByName` | Tela de login |
| Avatares de usuários (`/Users/{id}/Images/*`) | Os apps carregam os avatares na tela de login |
| Capas e fundos (`/Items/{id}/Images/*`), só da rede local | Os apps que usam `<img>` não enviam token (veja abaixo) |

Todo o resto (listagens, detalhes, streaming, reprodução, administração de usuários) exige um token válido, aceito em
`Authorization: MediaBrowser Token="..."`, `X-Emby-Token`, `X-MediaBrowser-Token` ou `api_key`/`ApiKey` na URL.

**Capas e fundos** (`GET /Items/{id}/Images/...`) abrem sem token **para clientes da rede local**. Os apps que carregam
imagens com `<img>`, como o Moonfin na TV Samsung, não conseguem enviar cabeçalhos, e o Jellyfin também serve as imagens sem
login. De fora da rede local, ou com `RUSTYFIN_IMAGENS_ANONIMAS=false`, as imagens exigem token. Só as imagens são abertas:
listagens, detalhes, streaming e o resto continuam exigindo token. Como a imagem anônima não tem usuário, o acesso por
biblioteca não filtra capas: quem está na rede local e conhece o ID de um título vê a capa dele. No HLS, o servidor
inclui `api_key` nas URLs das playlists e dos segmentos para players que não reenviam cabeçalhos. As URLs
de reprodução contêm uma credencial e devem ser tratadas como privadas.

Isto é um servidor de **rede doméstica**. Se for expô-lo à internet, use HTTPS por um *proxy reverso*,
defina senhas fortes e configure o proxy para omitir a query dos logs.

## Logs

O log de requisições registra só o método e o caminho. Tokens e `api_key` das URLs não vão para os logs.

## Reportar uma falha de segurança

Abra uma *issue* no repositório sem detalhes que permitam explorar a falha e peça um contato privado.
