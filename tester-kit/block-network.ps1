# Adds a Windows Firewall rule that stops Sizequilt.exe connecting out.
# Windows enforces the rule, not the app. Read this file before you run it.
#
#   Status, no administrator needed:
#     powershell -ExecutionPolicy Bypass -File .\block-network.ps1 -Status
#   Add the rule, from a PowerShell window opened as administrator:
#     powershell -ExecutionPolicy Bypass -File .\block-network.ps1
#   Take it away again, also as administrator:
#     powershell -ExecutionPolicy Bypass -File .\block-network.ps1 -Remove
param(
    [string]$Path = (Join-Path $PSScriptRoot 'Sizequilt.exe'),
    [switch]$Status,
    [switch]$Remove
)

$RuleName = 'Sizequilt - block network'
$existing = Get-NetFirewallRule -DisplayName $RuleName -ErrorAction SilentlyContinue

if ($Status) {
    if ($existing) {
        $program = ($existing | Get-NetFirewallApplicationFilter).Program
        Write-Output "The rule is in place and $($existing.Enabled -replace 'True','enabled' -replace 'False','DISABLED'): $program"
    } else {
        Write-Output 'There is no rule yet.'
    }
    exit 0
}

$identity = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Output 'Changing the firewall needs administrator rights.'
    Write-Output 'Right-click PowerShell, choose Run as administrator, and run this again.'
    exit 1
}

if ($Remove) {
    if ($existing) {
        $existing | Remove-NetFirewallRule
        Write-Output 'The rule is gone.'
    } else {
        Write-Output 'There was no rule to remove.'
    }
    exit 0
}

if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    Write-Output "There is no file at $Path"
    exit 1
}
$full = (Resolve-Path -LiteralPath $Path).Path
if ($existing) { $existing | Remove-NetFirewallRule }
New-NetFirewallRule -DisplayName $RuleName -Direction Outbound -Action Block -Program $full | Out-Null
Write-Output "Blocked: $full"
Write-Output 'The rule follows this exact path. If you move the app, run this again.'
Write-Output 'Use the Folder walker scan method, so the app never has the rights to change this rule.'
exit 0
