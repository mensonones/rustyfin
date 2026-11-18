param(
    [Parameter(Mandatory = $true)][string]$Pasta,
    [ValidateSet("Registrar", "Remover", "Iniciar", "Parar")][string]$Acao = "Registrar",
    [switch]$SemGatilho
)
# Atenção: o PowerShell não diferencia maiúsculas de minúsculas, então nenhuma variável local pode se chamar $acao (é o parâmetro).
# Gerencia a tarefa agendada "Rustyfin" (usada pelo instalar.ps1 e pelo setup). Não precisa de administrador.
$ErrorActionPreference = "Stop"
$Tarefa = "Rustyfin"

function Parar-Servidor {
    Stop-ScheduledTask -TaskName $Tarefa -ErrorAction SilentlyContinue
    Get-Process -Name rustyfin -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like "$Pasta*" } | Stop-Process -Force
}

switch ($Acao) {
    "Registrar" {
        $acaoTarefa = New-ScheduledTaskAction -Execute "powershell.exe" `
            -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$(Join-Path $Pasta 'iniciar.ps1')`""
        $config = New-ScheduledTaskSettingsSet -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) `
            -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
        if ($SemGatilho) {
            Register-ScheduledTask -TaskName $Tarefa -Action $acaoTarefa -Settings $config -Force | Out-Null
        } else {
            $gatilho = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
            Register-ScheduledTask -TaskName $Tarefa -Action $acaoTarefa -Trigger $gatilho -Settings $config -Force | Out-Null
        }
    }
    "Iniciar" { Start-ScheduledTask -TaskName $Tarefa }
    "Parar" { Parar-Servidor }
    "Remover" {
        Parar-Servidor
        Unregister-ScheduledTask -TaskName $Tarefa -Confirm:$false -ErrorAction SilentlyContinue
    }
}
