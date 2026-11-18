param(
    [int]$Porta = 8096,
    [switch]$Remover
)
# Regras do Firewall do Windows para o Rustyfin, na rede privada. Precisa de administrador (o setup pede a permissão).
$ErrorActionPreference = "Stop"
Get-NetFirewallRule -DisplayName "Rustyfin*" -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue
if ($Remover) { return }
New-NetFirewallRule -DisplayName "Rustyfin (HTTP)" -Direction Inbound -Protocol TCP -LocalPort $Porta -Action Allow -Profile Private | Out-Null
New-NetFirewallRule -DisplayName "Rustyfin (descoberta)" -Direction Inbound -Protocol UDP -LocalPort 7359 -Action Allow -Profile Private | Out-Null
