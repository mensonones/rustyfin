#!/usr/bin/env python3
"""Diagnostica o acesso ao Rustyfin por um proxy reverso, repetindo o que um app do Jellyfin faz ao conectar.

Rode de um aparelho de FORA da rede (ou de onde o app falha), com o endereço que você digitaria no app:

    python tools/testar_proxy.py https://midia.exemplo.com
    python tools/testar_proxy.py https://midia.exemplo.com --usuario ana --senha '...'   # testa também login, Range e WebSocket

Só usa a biblioteca padrão. Não altera nada no servidor; com --usuario cria uma sessão (um token) como qualquer app.
"""
import argparse
import base64
import http.client
import json
import os
import socket
import ssl
import sys
from urllib.parse import urlparse

AUTH = 'MediaBrowser Client="testar_proxy", Device="diagnostico", DeviceId="testar-proxy", Version="1"'
falhas = []


def ok(msg):
    print(f"  ok     {msg}")


def falha(msg, dica=""):
    print(f"  FALHOU {msg}")
    if dica:
        print(f"         -> {dica}")
    falhas.append(msg)


class Alvo:
    def __init__(self, url, inseguro, tempo):
        u = urlparse(url if "://" in url else "https://" + url)
        self.esquema = u.scheme
        self.host = u.hostname
        self.porta = u.port or (443 if u.scheme == "https" else 80)
        self.base = u.path.rstrip("/")
        self.tempo = tempo
        self.ctx = ssl.create_default_context()
        if inseguro:
            self.ctx.check_hostname = False
            self.ctx.verify_mode = ssl.CERT_NONE

    def conexao(self):
        if self.esquema == "https":
            return http.client.HTTPSConnection(self.host, self.porta, timeout=self.tempo, context=self.ctx)
        return http.client.HTTPConnection(self.host, self.porta, timeout=self.tempo)

    def pedir(self, metodo, caminho, token=None, corpo=None, extra=None):
        cabecalhos = {"Authorization": AUTH + (f', Token="{token}"' if token else ""), "Accept": "application/json"}
        if corpo is not None:
            cabecalhos["Content-Type"] = "application/json"
            corpo = json.dumps(corpo)
        cabecalhos.update(extra or {})
        c = self.conexao()
        try:
            c.request(metodo, self.base + caminho, body=corpo, headers=cabecalhos)
            r = c.getresponse()
            dados = r.read(2_000_000) if metodo != "HEAD" else b""
            return r.status, dict((k.lower(), v) for k, v in r.getheaders()), dados
        finally:
            c.close()


def como_json(dados):
    try:
        return json.loads(dados)
    except ValueError:
        return None


def passo_rede(a):
    print(f"1. Conexão com {a.host}:{a.porta} ({a.esquema})")
    try:
        enderecos = {i[4][0] for i in socket.getaddrinfo(a.host, a.porta, type=socket.SOCK_STREAM)}
        ok(f"DNS resolve para {', '.join(sorted(enderecos))}")
    except OSError as e:
        falha(f"o nome {a.host} não resolve ({e})", "confira o DNS do domínio e se o aparelho usa um DNS que o conhece")
        return False
    try:
        c = a.conexao()
        c.connect()
        c.close()
        ok("conexão TCP" + (" e TLS (certificado válido)" if a.esquema == "https" else ""))
    except ssl.SSLCertVerificationError as e:
        falha(f"certificado recusado: {e.verify_message}", "o app do iOS também recusa; use um certificado válido (Let's Encrypt) "
              "para o mesmo nome, com a cadeia completa (fullchain). Teste de novo com --inseguro só para isolar")
        return False
    except ssl.SSLError as e:
        falha(f"falha de TLS: {e}", "o proxy precisa servir HTTPS nessa porta; confira se não é um serviço HTTP na 443")
        return False
    except OSError as e:
        falha(f"não conectou na porta {a.porta} ({e})", "firewall, roteador sem redirecionar a porta, ou o proxy parado")
        return False
    return True


def passo_publico(a):
    print("2. O que o app pede antes do login")
    st, h, d = a.pedir("GET", "/System/Info/Public")
    j = como_json(d)
    if st in (301, 302, 307, 308):
        falha(f"/System/Info/Public respondeu {st} para {h.get('location')}", "o proxy redireciona; o app não segue para outro "
              "endereço. Use direto o endereço final (https, sem barra extra) ou desligue o redirecionamento no proxy")
    elif st in (502, 503, 504):
        falha(f"o proxy respondeu {st}", "ele não alcança o Rustyfin: confira o IP e a porta do destino, e se o servidor está no ar "
              "(e, no Docker, se o proxy e o Rustyfin estão na mesma rede)")
    elif st == 200 and isinstance(j, dict) and j.get("ProductName"):
        ok(f"/System/Info/Public: {j['ProductName']} {j.get('Version')} (\"{j.get('ServerName')}\")")
    elif st == 200:
        falha("200, mas não é a resposta de um servidor Jellyfin", "parece a página padrão do proxy ou de outro site: "
              "confira o nome do site/host virtual (server_name, Host) e o destino")
    else:
        falha(f"/System/Info/Public respondeu {st}", "o proxy ou um firewall (WAF, Cloudflare, autenticação) está barrando; "
              "o app não envia cookies nem faz login no proxy")
    if "text/html" in h.get("content-type", "") and st == 200 and not isinstance(j, dict):
        print("         (o conteúdo é HTML: uma página, não a API)")
    for caminho in ("/System/Ping", "/Users/Public", "/Branding/Configuration", "/QuickConnect/Enabled"):
        st, h, d = a.pedir("GET", caminho)
        (ok if st == 200 else falha)(f"GET {caminho} -> {st}")


def passo_login(a, usuario, senha):
    print("3. Login e uso")
    st, h, d = a.pedir("POST", "/Users/AuthenticateByName", corpo={"Username": usuario, "Pw": senha})
    j = como_json(d)
    if st == 200 and isinstance(j, dict) and j.get("AccessToken"):
        ok(f"login de {usuario}")
        token = j["AccessToken"]
    else:
        falha(f"login recusado ({st})", "usuário sem senha só entra da rede local: defina uma senha; e confira "
              "RUSTYFIN_TRUSTED_PROXIES (veja o log do servidor)" if not senha else "usuário ou senha incorretos")
        return None
    st, _, d = a.pedir("GET", "/UserViews", token)
    (ok if st == 200 else falha)(f"GET /UserViews -> {st}")
    st, _, d = a.pedir("GET", "/Items?Recursive=true&IncludeItemTypes=Movie,Episode&Limit=1&Fields=Path", token)
    j = como_json(d)
    itens = (j or {}).get("Items") or []
    if st != 200:
        falha(f"GET /Items -> {st}")
        return token
    ok(f"GET /Items -> {len(itens)} item(ns) de amostra")
    if itens:
        passo_video(a, token, itens[0]["Id"])
    passo_websocket(a, token)
    return token


def passo_video(a, token, item):
    st, h, _ = a.pedir("GET", f"/Videos/{item}/stream?static=true&api_key={token}", token, extra={"Range": "bytes=0-1023"})
    if st == 206 and "content-range" in h:
        ok("Direct Play com Range (206 Partial Content): o app consegue buscar posições no vídeo")
    elif st == 200:
        falha("o vídeo veio inteiro (200) em vez de 206", "o proxy removeu o cabeçalho Range; o vídeo vai travar ao avançar. "
              "No nginx: proxy_buffering off e não sobrescreva Range")
    else:
        falha(f"GET /Videos/.../stream -> {st}")


def passo_websocket(a, token):
    caminho = f"{a.base}/socket?api_key={token}&deviceId=testar-proxy"
    chave = base64.b64encode(os.urandom(16)).decode()
    pedido = (f"GET {caminho} HTTP/1.1\r\nHost: {a.host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
              f"Sec-WebSocket-Key: {chave}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode()
    try:
        bruto = socket.create_connection((a.host, a.porta), timeout=a.tempo)
        s = a.ctx.wrap_socket(bruto, server_hostname=a.host) if a.esquema == "https" else bruto
        s.sendall(pedido)
        resposta = s.recv(512).decode("latin-1", "replace")
        s.close()
    except OSError as e:
        falha(f"WebSocket: {e}")
        return
    primeira = resposta.split("\r\n", 1)[0]
    if " 101 " in primeira + " ":
        ok("WebSocket (/socket) abre pelo proxy")
    else:
        falha(f"WebSocket respondeu \"{primeira}\"", "faltam Upgrade e Connection no proxy (no Nginx Proxy Manager: ligue "
              "\"Websockets Support\"); sem isso o app pode funcionar, mas sem comandos remotos nem atualização de estado")


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(errors="replace")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("url", help="endereço que o app usa, por exemplo https://midia.exemplo.com")
    p.add_argument("--usuario")
    p.add_argument("--senha", default="")
    p.add_argument("--inseguro", action="store_true", help="não valida o certificado (só para isolar um problema de certificado)")
    p.add_argument("--tempo", type=float, default=15.0, help="limite de espera por pedido, em segundos")
    args = p.parse_args()
    a = Alvo(args.url, args.inseguro, args.tempo)
    if passo_rede(a):
        try:
            passo_publico(a)
            if args.usuario:
                passo_login(a, args.usuario, args.senha)
            else:
                print("3. (login, Range e WebSocket não testados: passe --usuario e --senha)")
        except (OSError, http.client.HTTPException) as e:
            falha(f"erro de rede durante o teste: {e}")
    print()
    if falhas:
        print(f"{len(falhas)} problema(s). Guia: docs/proxy-reverso.md")
        return 1
    print("Tudo certo: o app deve conseguir conectar por este endereço.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
