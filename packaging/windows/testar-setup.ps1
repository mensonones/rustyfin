<#
.SYNOPSIS
  Testa o Rustyfin-Setup.exe sem telas: instala, confere o servidor, atualiza por cima e desinstala.

.DESCRIPTION
  Usado pelo fluxo "Setup do Windows" do GitHub Actions e útil na sua máquina. Instala numa pasta de teste, na porta
  informada, valida a inicialização pela tarefa agendada sem bandeja e falha (código de saída 1)
  se qualquer conferência não passar. O executável da bandeja é verificado como parte do pacote.

.EXAMPLE
  .\testar-setup.ps1 -Setup .\dist\Rustyfin-Setup-0.25.0-x64.exe
  .\testar-setup.ps1 -Setup .\dist\Rustyfin-Setup-0.25.0-x64.exe -BaixarFfmpeg     # também testa o download do ffmpeg (~200 MB)
#>
param(
    [Parameter(Mandatory = $true)][string]$Setup,
    [string]$Pasta = "",
    [int]$Porta = 8097,
    [switch]$BaixarFfmpeg
)
$ErrorActionPreference = "Stop"
# Pasta de teste: o padrão fica na pasta temporária do sistema (um valor padrão calculado no param() que falhasse seria
# ignorado em silêncio e deixaria $Pasta vazio).
if ([string]::IsNullOrWhiteSpace($Pasta)) { $Pasta = Join-Path ([System.IO.Path]::GetTempPath()) "RustyfinSetupTeste" }
$Setup = (Resolve-Path $Setup).Path
$Midia = "$Pasta-midia"
$Log = "$Pasta-instalacao.log"
$Url = "http://127.0.0.1:$Porta/System/Info/Public"
$falhas = @()

function Conferir($descricao, [bool]$ok) {
    if ($ok) { Write-Host "  ok     $descricao" } else { Write-Host "  FALHOU $descricao"; $script:falhas += $descricao }
}
function Esperar-Servidor([bool]$no_ar, [int]$segundos = 40) {
    for ($i = 0; $i -lt $segundos; $i++) {
        try { $r = (Invoke-WebRequest -Uri $Url -UseBasicParsing -TimeoutSec 2).StatusCode -eq 200 } catch { $r = $false }
        if ($r -eq $no_ar) { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}
function Instalar([string]$ffmpeg) {
    # Seleciona só o início automático para o teste funcionar sem área de notificação.
    $argumentos = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/DIR=$Pasta", "/PORTA=$Porta", "/MIDIAS=$Midia",
        "/FFMPEG=$ffmpeg", "/TASKS=inicio", "/LOG=$Log")
    $p = Start-Process -FilePath $Setup -ArgumentList $argumentos -Wait -PassThru
    return $p.ExitCode
}

New-Item -ItemType Directory -Force -Path $Midia | Out-Null
$modo = if ($BaixarFfmpeg) { "baixar" } else { "nenhum" }

Write-Host "0. Verificação de porta ocupada (porta.ps1, usada pelo assistente)"
$script_porta = Join-Path $PSScriptRoot "porta.ps1"
# A conta fica numa variável: "New-Object Tipo(a, $x + 100)" seria lido como três argumentos.
$porta_ocupada = $Porta + 100
$ocupada = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Any, $porta_ocupada)
$ocupada.Start()
& $script_porta -Porta $porta_ocupada
Conferir "uma porta em uso é reconhecida como ocupada" ($LASTEXITCODE -eq 1)
$sugerida = & $script_porta -Porta $porta_ocupada -Procurar
Conferir "a busca sugere outra porta livre ($sugerida)" ($LASTEXITCODE -eq 0 -and [int]$sugerida -gt $porta_ocupada)
$ocupada.Stop()
& $script_porta -Porta $porta_ocupada
Conferir "a mesma porta, depois de liberada, é reconhecida como livre" ($LASTEXITCODE -eq 0)

Write-Host "1. Instalação sem telas (ffmpeg: $modo)"
$codigo = Instalar $modo
Conferir "o setup terminou com código 0 (recebido $codigo)" ($codigo -eq 0)
Conferir "rustyfin.exe foi instalado" (Test-Path (Join-Path $Pasta "rustyfin.exe"))
Conferir "o ícone da bandeja (rustyfin-tray.exe) foi instalado" (Test-Path (Join-Path $Pasta "rustyfin-tray.exe"))
Conferir "os scripts de inicialização foram instalados" ((Test-Path (Join-Path $Pasta "iniciar.ps1")) -and (Test-Path (Join-Path $Pasta "tarefa.ps1")))
$env_ = Join-Path $Pasta "rustyfin.env"
Conferir "rustyfin.env foi criado" (Test-Path $env_)
if (Test-Path $env_) {
    $conteudo = Get-Content $env_ -Raw
    Conferir "a porta escolhida está no rustyfin.env" ($conteudo -match "RUSTYFIN_PORT=$Porta")
    Conferir "a pasta de mídia está no rustyfin.env" ($conteudo.Contains($Midia))
}
$tarefa = Get-ScheduledTask -TaskName Rustyfin -ErrorAction SilentlyContinue
Conferir "a tarefa agendada existe" ($null -ne $tarefa)
if ($tarefa) {
    $script_inicio = [regex]::Escape((Join-Path $Pasta "iniciar.ps1"))
    $acoes = @($tarefa.Actions)
    $usa_script = $acoes.Count -eq 1 -and $acoes[0].Execute -eq "powershell.exe" -and
        $acoes[0].Arguments -match ('(?:^|\s)-File "' + $script_inicio + '"$')
    Conferir "a tarefa agendada inicia o servidor sem bandeja" $usa_script
}
$servidor_no_ar = Esperar-Servidor $true
Conferir "o servidor responde na porta $Porta" $servidor_no_ar
if (-not $servidor_no_ar) {
    Write-Host "  --- rustyfin.log (últimas linhas):"
    $registro = Join-Path $Pasta "rustyfin.log"
    if (Test-Path $registro) { Get-Content $registro -Tail 25 } else { Write-Host "      (sem rustyfin.log)" }
}
if ($BaixarFfmpeg) {
    Conferir "o ffmpeg foi baixado e extraído" (Test-Path (Join-Path $Pasta "ffmpeg\bin\ffmpeg.exe"))
    Conferir "o ffprobe veio junto" (Test-Path (Join-Path $Pasta "ffmpeg\bin\ffprobe.exe"))
    Conferir "o rustyfin.env aponta para o ffmpeg" ((Get-Content $env_ -Raw).Contains("RUSTYFIN_FFMPEG="))
    Start-Sleep -Seconds 2
    $registro = Join-Path $Pasta "rustyfin.log"
    Conferir "o servidor achou o ffmpeg (log)" ((Test-Path $registro) -and ((Get-Content $registro -Raw) -match "ffmpeg encontrado"))
}

Write-Host "2. Atualização por cima (a configuração precisa ser preservada)"
Add-Content -Path $env_ -Value "# marcador do teste"
$codigo = Instalar $modo
Conferir "a atualização terminou com código 0 (recebido $codigo)" ($codigo -eq 0)
Conferir "a configuração foi preservada" ((Get-Content $env_ -Raw).Contains("# marcador do teste"))
Conferir "o servidor voltou a responder" (Esperar-Servidor $true)

Write-Host "2b. Atualização com o ffmpeg configurado quebrado (o assistente corrige só essa linha)"
$linhas = @(Get-Content $env_ | Where-Object { $_ -notmatch '^\s*RUSTYFIN_FFMPEG=' })
Set-Content -Path $env_ -Value ($linhas + 'RUSTYFIN_FFMPEG=C:\nao\existe\ffmpeg.exe') -Encoding utf8
$codigo = Instalar "nenhum"
Conferir "a atualização com ffmpeg quebrado terminou com código 0 (recebido $codigo)" ($codigo -eq 0)
$texto = Get-Content $env_ -Raw
Conferir "a linha do ffmpeg quebrado saiu da configuração" (-not ($texto -match '(?m)^\s*RUSTYFIN_FFMPEG=C:\\nao'))
Conferir "o resto da configuração foi preservado" ($texto.Contains("# marcador do teste") -and ($texto -match "RUSTYFIN_PORT=$Porta"))
Conferir "o servidor voltou a responder" (Esperar-Servidor $true)

Write-Host "3. Desinstalação (apagando os dados)"
$desinstalador = Get-ChildItem -Path $Pasta -Filter "unins*.exe" | Select-Object -First 1
Conferir "o desinstalador existe" ($null -ne $desinstalador)
if ($desinstalador) {
    $p = Start-Process -FilePath $desinstalador.FullName -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/APAGARDADOS=1" -Wait -PassThru
    Conferir "o desinstalador terminou com código 0 (recebido $($p.ExitCode))" ($p.ExitCode -eq 0)
    Start-Sleep -Seconds 3
}
Conferir "o servidor parou" (Esperar-Servidor $false 15)
Conferir "a tarefa agendada foi removida" ($null -eq (Get-ScheduledTask -TaskName Rustyfin -ErrorAction SilentlyContinue))
Conferir "a pasta de instalação foi apagada" (-not (Test-Path (Join-Path $Pasta "rustyfin.exe")))
Remove-Item -Recurse -Force $Midia -ErrorAction SilentlyContinue

if ($falhas.Count -gt 0) {
    Write-Host "`n$($falhas.Count) conferência(s) falharam. Log do setup: $Log"
    if (Test-Path $Log) { Get-Content $Log | Select-Object -Last 40 }
    exit 1
}
Write-Host "`nTodas as conferências passaram."
