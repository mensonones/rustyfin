$ErrorActionPreference = "Stop"
# Inicia o Rustyfin a partir da pasta onde este script está: lê o rustyfin.env (CHAVE=VALOR), define a pasta de dados e
# mantém o servidor no ar, com o registro em rustyfin.log. É o que a tarefa agendada "Rustyfin" executa.
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
