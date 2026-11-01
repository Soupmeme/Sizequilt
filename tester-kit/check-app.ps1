# Checks a copy of Sizequilt.exe for networking, without running it.
# It only reads the file. It needs no administrator rights and changes nothing.
#
#   powershell -ExecutionPolicy Bypass -File .\check-app.ps1
#   powershell -ExecutionPolicy Bypass -File .\check-app.ps1 -ExpectedHash <the hash you were sent>
param(
    [string]$Path = (Join-Path $PSScriptRoot 'Sizequilt.exe'),
    [string]$ExpectedHash = ''
)

# Windows libraries that exist to talk to a network.
$NetworkLibraries = 'ws2_32', 'wsock32', 'mswsock', 'winhttp', 'wininet', 'urlmon', 'dnsapi',
    'iphlpapi', 'webio', 'netapi32', 'wtsapi32', 'rasapi32', 'bitsprox', 'qmgr', 'winsock'
# Function names that open connections, should something look them up while running.
$NetworkFunctions = 'WSAStartup', 'WSASocket', 'getaddrinfo', 'InternetOpen', 'WinHttpOpen',
    'URLDownloadToFile', 'HttpSendRequest', 'DnsQuery'

if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    Write-Output "FAIL: there is no file at $Path"
    exit 1
}
$bytes = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Path).Path)

function ReadU16($at) { [BitConverter]::ToUInt16($bytes, $at) }
function ReadU32($at) { [BitConverter]::ToUInt32($bytes, $at) }
function ReadText($at) {
    $end = [Array]::IndexOf($bytes, [byte]0, [int]$at)
    [Text.Encoding]::ASCII.GetString($bytes, $at, $end - $at)
}

if ((ReadU16 0) -ne 0x5A4D) {
    Write-Output 'FAIL: this is not a Windows program'
    exit 1
}
$pe = ReadU32 0x3C
$sectionCount = ReadU16 ($pe + 6)
$optionalSize = ReadU16 ($pe + 20)
$optional = $pe + 24
$directories = $optional + $(if ((ReadU16 $optional) -eq 0x20B) { 112 } else { 96 })

$sections = for ($i = 0; $i -lt $sectionCount; $i++) {
    $at = $pe + 24 + $optionalSize + $i * 40
    [pscustomobject]@{
        Start = ReadU32 ($at + 12)
        Size  = [Math]::Max((ReadU32 ($at + 8)), (ReadU32 ($at + 16)))
        Raw   = ReadU32 ($at + 20)
    }
}
function Offset($rva) {
    foreach ($s in $sections) {
        if ($rva -ge $s.Start -and $rva -lt $s.Start + $s.Size) { return $rva - $s.Start + $s.Raw }
    }
    return $null
}

# Every library the program declares it needs, delayed ones included.
$imports = New-Object System.Collections.ArrayList
$normal = ReadU32 ($directories + 8)
if ($normal) {
    $at = Offset $normal
    while ($at -and (ReadU32 ($at + 12))) {
        [void]$imports.Add((ReadText (Offset (ReadU32 ($at + 12)))))
        $at += 20
    }
}
$delayed = ReadU32 ($directories + 13 * 8)
if ($delayed) {
    $at = Offset $delayed
    while ($at -and (ReadU32 ($at + 4))) {
        [void]$imports.Add((ReadText (Offset (ReadU32 ($at + 4)))))
        $at += 32
    }
}

$findings = New-Object System.Collections.ArrayList
foreach ($library in $imports) {
    foreach ($name in $NetworkLibraries) {
        if ($library -like "$name*") { [void]$findings.Add("imports the network library $library") }
    }
}

# The same names anywhere in the file, in the two forms Windows programs store text.
$narrow = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
foreach ($name in ($NetworkLibraries + $NetworkFunctions)) {
    $wide = -join ($name.ToCharArray() | ForEach-Object { "$_`0" })
    foreach ($form in $name, $wide) {
        if ($narrow.IndexOf($form, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            [void]$findings.Add("contains the name $name")
            break
        }
    }
}

$hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
Write-Output "File:      $Path"
Write-Output "SHA-256:   $hash"
Write-Output "Libraries: $($imports.Count) declared: $(($imports | Sort-Object) -join ', ')"
Write-Output ''

$failed = $false
if ($ExpectedHash) {
    if ($hash -ieq $ExpectedHash.Trim()) {
        Write-Output 'PASS: the file matches the hash you were given.'
    } else {
        Write-Output 'FAIL: this file does NOT match the hash you were given. Do not run it.'
        $failed = $true
    }
} else {
    Write-Output 'NOTE: no hash was given to compare. Ask for it, and get it separately from the file.'
}

if ($findings.Count) {
    $findings | Select-Object -Unique | ForEach-Object { Write-Output "FAIL: this program $_" }
    $failed = $true
} else {
    Write-Output 'PASS: no network libraries or network function names found.'
}

Write-Output ''
Write-Output 'This is good evidence, not proof: a program can hide what it does.'
Write-Output 'For a guarantee, scan with your network off. See PRIVACY.md.'
if ($failed) { exit 1 }
exit 0
