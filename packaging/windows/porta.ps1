param(
    [int]$Porta = 8096,
    [switch]$Procurar,
    [string]$Saida
)
# Confere se uma porta TCP está livre neste computador (o setup usa antes de gravar a porta no rustyfin.env).
#   porta.ps1 -Porta 8096              código de saída 0 se está livre, 1 se está em uso
#   porta.ps1 -Porta 8096 -Procurar    escreve a primeira porta livre a partir da informada (até 50 acima)
#   ... -Saida arquivo.txt             grava a porta encontrada nesse arquivo, em vez de escrevê-la na tela
function Livre([int]$numero) {
    try {
        $escuta = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Any, $numero)
        $escuta.Start()
        $escuta.Stop()
        return $true
    } catch {
        return $false
    }
}

if ($Procurar) {
    for ($p = $Porta; $p -lt $Porta + 50; $p++) {
        if (Livre $p) {
            if ($Saida) { Set-Content -Path $Saida -Value $p -Encoding ascii } else { Write-Output $p }
            exit 0
        }
    }
    exit 1
}
if (Livre $Porta) { exit 0 } else { exit 1 }
