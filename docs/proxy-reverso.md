# Proxy reverso e acesso de fora de casa

O Rustyfin funciona atrás de um proxy reverso (nginx, Caddy, Traefik...), que dá HTTPS e um endereço como
`https://midia.exemplo.com`. Os apps do Jellyfin só precisam desse endereço.

## O que muda: o IP do cliente

Várias regras dependem de o cliente estar na **rede local**: o painel `/admin`, o login de usuário **sem senha** e as
capas sem token. Atrás de um proxy, a conexão sempre vem do proxy, e sem cuidado todo visitante da internet pareceria da
rede local. Por isso o servidor segue estas regras:

1. **Só os proxies de `RUSTYFIN_TRUSTED_PROXIES` podem informar o IP do cliente**, pelo `X-Forwarded-For` (ou `X-Real-IP`).
   Com vários saltos vale o primeiro endereço, da direita para a esquerda, que não seja um proxy seu; o que o cliente
   escreve à esquerda não conta.
2. **Cabeçalhos de proxy vindos de quem não é confiável** (`X-Forwarded-For`, `X-Real-IP`, `Forwarded`...) fazem a requisição
   valer como **da internet**. Se você esquecer de configurar, o resultado é um painel fechado, nunca um painel aberto.
   O log avisa uma vez: "adicione o endereço dele a `RUSTYFIN_TRUSTED_PROXIES`".
3. **Um proxy confiável que não informa o cliente** também deixa a requisição como externa. O proxy precisa enviar
   `X-Forwarded-For` ou `X-Real-IP` (os exemplos abaixo enviam).

```text
RUSTYFIN_TRUSTED_PROXIES=192.168.1.10          # um proxy em outro aparelho, ou o IP da rede do próprio PC
RUSTYFIN_TRUSTED_PROXIES=172.16.0.0/12         # proxy em outro contêiner Docker (rede padrão do Docker)
```

> **Proxy no mesmo PC:** aponte o `proxy_pass` para o **IP da rede do PC** (`http://192.168.1.10:8096`) e confie nesse IP,
> em vez de confiar em `127.0.0.1`. Assim, quem abre `http://localhost:8096/admin` direto no PC continua sendo reconhecido
> como local (e o ícone da bandeja continua podendo desligar o servidor graciosamente).

## Proxy dentro da rede local (o caso mais comum em casa)

Se o proxy também está na sua rede e os aparelhos acessam por um nome interno (DNS local apontando `midia.exemplo.com`
para o IP do proxy), **os clientes continuam sendo da rede local**: o proxy repassa o IP deles no `X-Forwarded-For`, e com o
proxy em `RUSTYFIN_TRUSTED_PROXIES` tudo funciona como no acesso direto (painel, login sem senha, capas). Sem a variável,
o servidor não confia no cabeçalho e **trata tudo o que passa pelo proxy como vindo da internet**: o app conecta, mas
usuários sem senha não entram, as capas somem e o painel não abre por esse endereço. O log avisa na primeira requisição.
O painel também abre pelo endereço direto (`http://IP-do-servidor:8096/admin`).

Nesse cenário, a falha **"Could not connect to server" acontece antes do login**, ou seja, antes de qualquer regra do
Rustyfin. Quase sempre é o certificado (um nome interno com certificado de uma autoridade própria, que o iOS e as TVs não
conhecem, ou um certificado sem a cadeia completa) ou o proxy apontando para o destino errado. Rode o
`tools/testar_proxy.py` de um aparelho da própria rede.

## O que fica só na rede local, de propósito

- **O painel `/admin` e `/Library/VirtualFolders`** não abrem pela internet. Administre pela rede local ou por uma VPN
  (WireGuard, Tailscale) que coloque você na rede de casa.
- **Login sem senha:** usuários sem senha só entram da rede local. **Defina senha para todo usuário que vai acessar de fora.**
- **Capas sem token** só para a rede local; de fora, o app precisa estar logado.

## Antes de abrir para a internet

1. Senha em **todos** os usuários (painel → Usuários). O servidor limita tentativas por usuário e IP real.
2. HTTPS no proxy (Caddy e Traefik obtêm o certificado sozinhos; no nginx, use o seu).
3. Só a porta 443 (e a 80, se o proxy renovar certificados por ela) no roteador, apontando para o **proxy**, nunca para a
   porta 8096 do Rustyfin.
4. Lembre que a lista de usuários públicos (`/Users/Public`) mostra os nomes de usuário, como no Jellyfin.

## nginx

```nginx
# em http { ... }: mantém a conexão do WebSocket (/socket) quando o cliente pede upgrade
map $http_upgrade $connection_upgrade { default upgrade; '' close; }

server {
    listen 443 ssl http2;
    server_name midia.exemplo.com;
    # ssl_certificate / ssl_certificate_key ...

    client_max_body_size 20m;          # envio de foto de perfil

    location / {
        proxy_pass http://192.168.1.10:8096;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;   # sobrescreve: só o IP que o nginx viu
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;

        # Vídeo: sem buffer, e tempo para o ffmpeg gerar o primeiro trecho do HLS
        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 600s;
        proxy_send_timeout 600s;
    }
}
```

## Caddy

```caddy
midia.exemplo.com {
    reverse_proxy 192.168.1.10:8096 {
        flush_interval -1
    }
}
```

O Caddy já envia o `X-Forwarded-For` (com o IP que ele viu), obtém o certificado sozinho e suporta WebSocket.

## Traefik (Docker)

```yaml
services:
  rustyfin:
    # ... (o serviço do compose do projeto)
    environment:
      RUSTYFIN_TRUSTED_PROXIES: 172.16.0.0/12      # a rede Docker em que o Traefik está
    labels:
      - traefik.enable=true
      - traefik.http.routers.midia.rule=Host(`midia.exemplo.com`)
      - traefik.http.routers.midia.entrypoints=websecure
      - traefik.http.routers.midia.tls.certresolver=letsencrypt
      - traefik.http.services.midia.loadbalancer.server.port=8096
```

O Traefik envia o `X-Forwarded-For` por padrão. Se houver outro proxy (como o Cloudflare) na frente dele, configure
`forwardedHeaders.trustedIPs` no *entrypoint* do Traefik com os IPs desse proxy; senão ele descarta o cabeçalho que chegou.

## Nginx Proxy Manager

Muito usado em casa e em Docker. No *Proxy Host*:

- **Forward Hostname / IP** e **Port**: o IP da rede do PC e `8096` (ou o nome do contêiner, se estiverem na mesma rede
  Docker). Em **Scheme**, `http`.
- Ligue **Websockets Support** e **Block Common Exploits** (opcional). Não ligue **Cache Assets**.
- Aba **SSL**: um certificado válido para o nome, com **Force SSL** e **HTTP/2**.
- Aba **Advanced** (cole em *Custom Nginx Configuration*):

```nginx
proxy_buffering off;
proxy_request_buffering off;
proxy_read_timeout 600s;
proxy_send_timeout 600s;
client_max_body_size 20m;
```

O NPM já envia `X-Forwarded-For` e `X-Real-IP`. Em `RUSTYFIN_TRUSTED_PROXIES`, ponha o IP (ou a rede Docker, como
`172.16.0.0/12`) **de onde o NPM se conecta ao Rustyfin**.

## Rede em que tudo precisa passar pela porta 443

Em redes que só deixam sair a porta 443 (empresas, escolas, ou regras como "TVs e máquinas só falam 443 com os
servidores"), tudo precisa passar pelo proxy, e o Rustyfin já se comporta assim: o app usa **uma única origem HTTPS**
(API, imagens, vídeo e WebSocket, tudo pelo mesmo endereço e pela mesma porta). Não há segunda porta, e nada abre conexão
direta ao servidor. Fica de fora só a descoberta automática (UDP 7359), que não passa por proxy: digite o endereço no app.

## Nos apps

Use o endereço completo do proxy (`https://midia.exemplo.com`, sem porta e sem barra no final). A descoberta automática
(UDP) só funciona na rede local e não passa por proxy. `RUSTYFIN_PUBLIC_URL` serve só à descoberta e não precisa mudar.

O app do iOS (Jellyfin e Swiftfin) mostra apenas **"Could not connect to server"** para qualquer falha: DNS, certificado
inválido, proxy que devolve 502 ou uma página qualquer, redirecionamento. Para saber qual é, rode de um aparelho de fora
da rede, com o mesmo endereço:

```bash
python tools/testar_proxy.py https://midia.exemplo.com
python tools/testar_proxy.py https://midia.exemplo.com --usuario ana --senha 'minha-senha'   # também login, Range e WebSocket
```

Ele repete o que o app faz (DNS, TLS, `/System/Info/Public`, login, um pedido com `Range` e o WebSocket) e diz, em cada
falha, o que costuma causar. Quatro causas respondem pela maioria dos casos:

1. **Certificado** que o iOS não aceita (autoassinado, nome diferente, sem a cadeia completa). O app recusa em silêncio.
2. **O proxy responde por outro site** (o host virtual errado devolve a página padrão do proxy, ou um 404).
3. **502/504**: o proxy não alcança o Rustyfin (IP ou porta do destino errados, ou outra rede Docker).
4. **Serviço na frente** (Cloudflare, WAF, autenticação básica no proxy): o app não envia cookies nem passa por desafios.

## Limites

- **Subcaminho** (`https://exemplo.com/midia/`) não é suportado: o painel e as listas HLS usam caminhos absolutos. Use um
  subdomínio.
- O proxy não deve cachear respostas de `/Videos`, `/Audio` nem `/Items/.../Images` (são por usuário).

## Se algo der errado

| Sintoma | Causa provável |
|---|---|
| O app do iOS diz "Could not connect to server" | Rode `python tools/testar_proxy.py https://seu-endereco`: ele aponta se é DNS, certificado, proxy (502) ou site errado |
| O painel responde "só está disponível na rede local" até para você | O proxy não está em `RUSTYFIN_TRUSTED_PROXIES` (veja o aviso no log), ou ele não envia `X-Forwarded-For` |
| Nenhum usuário sem senha consegue entrar de fora | É de propósito: defina uma senha |
| O vídeo trava no início ou corta | `proxy_buffering off` e `proxy_read_timeout` altos no nginx |
| O app não conecta pelo WebSocket | Faltam `Upgrade` e `Connection` no proxy |
