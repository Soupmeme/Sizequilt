# Fails if a built file holds this machine's username or project folder.
param([Parameter(Mandatory = $true)][string]$Path)

$bytes = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Path).Path)
$text = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
$project = Split-Path $PSScriptRoot -Parent
$personal = @($env:USERPROFILE, ($project -replace '\\', '/'), $project, "Users\$env:USERNAME", "Users/$env:USERNAME")

$found = New-Object System.Collections.ArrayList
foreach ($item in $personal) {
    $wide = -join ($item.ToCharArray() | ForEach-Object { "$_`0" })
    foreach ($form in $item, $wide) {
        if ($text.IndexOf($form, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            [void]$found.Add($item)
            break
        }
    }
}
if ($found.Count) {
    Write-Output "FAIL: this file holds personal paths: $(($found | Select-Object -Unique) -join ', ')"
    exit 1
}
Write-Output 'PASS: no username or project path in this file.'
exit 0
