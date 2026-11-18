<#
.SYNOPSIS
  Instala o Rustyfin no Windows pela linha de comando: sobe quando você entra na conta, reinicia se cair e guarda tudo em
  uma pasta. (Para o assistente com telas, use o Rustyfin-Setup.exe.)

.DESCRIPTION
  Copia o rustyfin.exe para %LOCALAPPDATA%\Rustyfin, cria a configuração (rustyfin.env), copia o script de inicialização
  e registra a tarefa agendada "Rustyfin" (ao entrar na conta). Não precisa de administrador, exceto para -Firewall.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File packaging\windows\instalar.ps1 -Binario .\target\release\rustyfin.exe

.EXAMPLE
  .\instalar.ps1 -Binario .\rustyfin.exe -Firewall          # também libera as portas na rede privada (como administrador)
  .\instalar.ps1 -SemBandeja                                # sem o ícone na bandeja (o servidor sobe por um script oculto)
  .\instalar.ps1 -Remover                                   # remove a tarefa e o binário, mantém os dados
  .\instalar.ps1 -Remover -ApagarDados
  .\instalar.ps1 -SomenteMostrar                            # mostra o que faria, sem alterar nada
#>
[CmdletBinding()]
param(
    [string]$Binario = ".\rustyfin.exe",
    [string]$Pasta = (Join-Path $env:LOCALAPPDATA "Rustyfin"),
    [switch]$Firewall,
    [switch]$SemBandeja,
    [switch]$Remover,
    [switch]$ApagarDados,
    [switch]$SomenteMostrar
)
$ErrorActionPreference = "Stop"
$AquiPacote = Split-Path -Parent $PSScriptRoot
$Tarefa = Join-Path $PSScriptRoot "tarefa.ps1"

function Passo($texto, [scriptblock]$acao) {
    if ($SomenteMostrar) { Write-Host "[simulação] $texto" } else { Write-Host $texto; & $acao }
}

if ($Remover) {
    Passo "Parando e removendo a tarefa agendada 'Rustyfin'" { & $Tarefa -Pasta $Pasta -Acao Remover }
    Passo "Removendo as regras de firewall do Rustyfin (se existirem)" {
        Get-NetFirewallRule -DisplayName "Rustyfin*" -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue
    }
    if ($ApagarDados) {
        Passo "Apagando $Pasta (binário, configuração e dados)" { Remove-Item -Recurse -Force $Pasta -ErrorAction SilentlyContinue }
    } else {
        Passo "Removendo o binário; dados e configuração ficam em $Pasta" {
            Remove-Item -Force (Join-Path $Pasta "rustyfin.exe"), (Join-Path $Pasta "rustyfin-tray.exe"), (Join-Path $Pasta "iniciar.ps1") -ErrorAction SilentlyContinue
        }
    }
    Write-Host "Pronto."
    return
}

$Binario = if (Test-Path $Binario) { (Resolve-Path $Binario).Path } else { $Binario }
if (-not $SomenteMostrar -and -not (Test-Path $Binario)) {
    throw "Binário não encontrado: $Binario. Baixe o rustyfin.exe da página de versões ou compile com 'cargo build --release' (target\release\rustyfin.exe)."
}

Passo "Criando $Pasta" { New-Item -ItemType Directory -Force -Path (Join-Path $Pasta "dados") | Out-Null }
Passo "Parando a versão em execução, se houver" { & $Tarefa -Pasta $Pasta -Acao Parar }
Passo "Copiando o rustyfin.exe e o script de inicialização" {
    Copy-Item -Force $Binario (Join-Path $Pasta "rustyfin.exe")
    Copy-Item -Force (Join-Path $PSScriptRoot "iniciar.ps1") (Join-Path $Pasta "iniciar.ps1")
    $bandeja = Join-Path (Split-Path -Parent $Binario) "rustyfin-tray.exe"
    if (Test-Path $bandeja) { Copy-Item -Force $bandeja (Join-Path $Pasta "rustyfin-tray.exe") }
}

$ArquivoEnv = Join-Path $Pasta "rustyfin.env"
if (-not (Test-Path $ArquivoEnv)) {
    Passo "Criando a configuração $ArquivoEnv" { Copy-Item (Join-Path $AquiPacote "rustyfin.env.example") $ArquivoEnv }
}

Passo "Registrando a tarefa agendada 'Rustyfin' (ao entrar na conta)" {
    if ($SemBandeja) { & $Tarefa -Pasta $Pasta -Acao Registrar -SemBandeja } else { & $Tarefa -Pasta $Pasta -Acao Registrar }
    & $Tarefa -Pasta $Pasta -Acao Iniciar
}

if ($Firewall) {
    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $admin -and -not $SomenteMostrar) { throw "-Firewall precisa de um PowerShell aberto como administrador." }
    Passo "Liberando 8096/tcp e 7359/udp na rede privada" {
        Get-NetFirewallRule -DisplayName "Rustyfin*" -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue
        New-NetFirewallRule -DisplayName "Rustyfin (HTTP)" -Direction Inbound -Protocol TCP -LocalPort 8096 -Action Allow -Profile Private | Out-Null
        New-NetFirewallRule -DisplayName "Rustyfin (descoberta)" -Direction Inbound -Protocol UDP -LocalPort 7359 -Action Allow -Profile Private | Out-Null
    }
} else {
    Write-Host "Dica: se o celular ou a TV não encontrarem o servidor, rode de novo com -Firewall (como administrador)."
}

if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
    Write-Warning "ffmpeg não encontrado no PATH: sem ele só há Direct Play. Instale com: winget install Gyan.FFmpeg (e entre na conta de novo), ou defina RUSTYFIN_FFMPEG em $ArquivoEnv."
}
$porta = 8096
if (Test-Path $ArquivoEnv) {
    $linhaPorta = Get-Content $ArquivoEnv | Where-Object { $_ -match '^\s*RUSTYFIN_PORT\s*=\s*(\d+)' } | Select-Object -Last 1
    if ($linhaPorta -match '(\d+)') { $porta = $Matches[1] }
}
Write-Host "Rustyfin instalado."
Write-Host "  Painel:  http://localhost:$porta/admin"
Write-Host "  Pasta:   $Pasta"
Write-Host "  Logs:    Get-Content '$Pasta\rustyfin.log' -Wait"
