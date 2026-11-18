<#
.SYNOPSIS
  Instala o Rustyfin no Windows: sobe quando você entra na conta, reinicia se cair e guarda tudo em uma pasta.

.DESCRIPTION
  Copia o rustyfin.exe para %LOCALAPPDATA%\Rustyfin, cria a configuração (rustyfin.env), um script de inicialização e uma
  tarefa agendada "Rustyfin" (ao entrar na conta). Não precisa de administrador, exceto para -Firewall.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File packaging\windows\instalar.ps1 -Binario .\target\release\rustyfin.exe

.EXAMPLE
  .\instalar.ps1 -Binario .\rustyfin.exe -Firewall          # também libera as portas na rede privada (como administrador)
  .\instalar.ps1 -Remover                                   # remove a tarefa e o binário, mantém os dados
  .\instalar.ps1 -Remover -ApagarDados
  .\instalar.ps1 -SomenteMostrar                            # mostra o que faria, sem alterar nada
#>
[CmdletBinding()]
param(
    [string]$Binario = ".\rustyfin.exe",
    [string]$Pasta = (Join-Path $env:LOCALAPPDATA "Rustyfin"),
    [switch]$Firewall,
    [switch]$Remover,
    [switch]$ApagarDados,
    [switch]$SomenteMostrar
)
$ErrorActionPreference = "Stop"
$Tarefa = "Rustyfin"
$AquiPacote = Split-Path -Parent $PSScriptRoot

function Passo($texto, [scriptblock]$acao) {
    if ($SomenteMostrar) { Write-Host "[simulação] $texto" } else { Write-Host $texto; & $acao }
}

if ($Remover) {
    Passo "Parando e removendo a tarefa agendada '$Tarefa'" {
        Stop-ScheduledTask -TaskName $Tarefa -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $Tarefa -Confirm:$false -ErrorAction SilentlyContinue
        Get-Process -Name rustyfin -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Pasta*" } | Stop-Process -Force
    }
    Passo "Removendo as regras de firewall do Rustyfin (se existirem)" {
        Get-NetFirewallRule -DisplayName "Rustyfin*" -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue
    }
    if ($ApagarDados) {
        Passo "Apagando $Pasta (binário, configuração e dados)" { Remove-Item -Recurse -Force $Pasta -ErrorAction SilentlyContinue }
    } else {
        Passo "Removendo o binário; dados e configuração ficam em $Pasta" {
            Remove-Item -Force (Join-Path $Pasta "rustyfin.exe"), (Join-Path $Pasta "iniciar.ps1") -ErrorAction SilentlyContinue
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
Passo "Parando a versão em execução, se houver" {
    Stop-ScheduledTask -TaskName $Tarefa -ErrorAction SilentlyContinue
    Get-Process -Name rustyfin -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Pasta*" } | Stop-Process -Force
}
Passo "Copiando o rustyfin.exe" { Copy-Item -Force $Binario (Join-Path $Pasta "rustyfin.exe") }

$ModeloEnv = Join-Path $AquiPacote "rustyfin.env.example"
$ArquivoEnv = Join-Path $Pasta "rustyfin.env"
if (-not (Test-Path $ArquivoEnv)) {
    Passo "Criando a configuração $ArquivoEnv" { Copy-Item $ModeloEnv $ArquivoEnv }
}

# O script lê o rustyfin.env (KEY=VALUE), define a pasta de dados e inicia o servidor, com o registro em rustyfin.log.
$Inicio = @'
$ErrorActionPreference = "Stop"
$Pasta = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:RUSTYFIN_DATA_DIR = Join-Path $Pasta "dados"
$arquivo = Join-Path $Pasta "rustyfin.env"
if (Test-Path $arquivo) {
    foreach ($linha in Get-Content $arquivo) {
        $linha = $linha.Trim()
        if ($linha -eq "" -or $linha.StartsWith("#") -or -not $linha.Contains("=")) { continue }
        $nome, $valor = $linha.Split("=", 2)
        [Environment]::SetEnvironmentVariable($nome.Trim(), $valor.Trim().Trim('"'), "Process")
    }
}
$env:NO_COLOR = "1"
$exe = Join-Path $Pasta "rustyfin.exe"
$log = Join-Path $Pasta "rustyfin.log"
# O cmd grava a saída em UTF-8 e sem as cores (o redirecionamento do PowerShell 5.1 gravaria UTF-16).
# Se o servidor cair (código de saída diferente de zero), sobe de novo depois de 5 segundos.
while ($true) {
    cmd.exe /c "`"$exe`" >> `"$log`" 2>&1"
    if ($LASTEXITCODE -eq 0) { break }
    Add-Content -Path $log -Value "Rustyfin encerrou com o código $LASTEXITCODE; reiniciando em 5 segundos." -Encoding UTF8
    Start-Sleep -Seconds 5
}
'@
Passo "Criando o script de inicialização" { Set-Content -Path (Join-Path $Pasta "iniciar.ps1") -Value $Inicio -Encoding UTF8 }

Passo "Registrando a tarefa agendada '$Tarefa' (ao entrar na conta)" {
    $acao = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$(Join-Path $Pasta 'iniciar.ps1')`""
    $gatilho = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
    $config = New-ScheduledTaskSettingsSet -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) `
        -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
    Register-ScheduledTask -TaskName $Tarefa -Action $acao -Trigger $gatilho -Settings $config -Force | Out-Null
    Start-ScheduledTask -TaskName $Tarefa
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
