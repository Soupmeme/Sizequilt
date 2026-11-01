# Fails if anything that could talk to a network is compiled into the Windows
# build. -Names checks a list instead of the real build, to test this script.
param([string[]]$Names = @())

$Banned = '^(reqwest|ureq|hyper|h2|h3|quinn|rustls|native-tls|openssl|openssl-sys|curl|curl-sys|' +
    'isahc|attohttpc|surf|tokio|async-std|smol|mio|socket2|tungstenite|tokio-tungstenite|tiny_http|' +
    'axum|actix-web|warp|rocket|sentry|opentelemetry|self_update|trust-dns-resolver|' +
    'hickory-resolver|mdns|lettre|libp2p|ws|websocket|nng|zmq)$'

if ($Names.Count -eq 0) {
    Push-Location (Join-Path $PSScriptRoot '..')
    $tree = & cargo tree -p sizequilt -e normal --target x86_64-pc-windows-gnu --prefix none
    $status = $LASTEXITCODE
    Pop-Location
    if ($status -ne 0) { Write-Output 'FAIL: cargo tree did not run'; exit 1 }
    $Names = $tree | ForEach-Object { ($_ -split ' ')[0] } | Where-Object { $_ } | Sort-Object -Unique
    # An empty list would pass everything, so a short one is a failure too.
    if ($Names.Count -lt 50) { Write-Output "FAIL: only $($Names.Count) crates listed"; exit 1 }
}

# Called through -File, a list arrives as one comma-joined string, which would
# match nothing and pass. Split it, so the check can't fail open.
$Names = @($Names | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$found = @($Names | Where-Object { $_ -match $Banned })
if ($found.Count) {
    Write-Output "FAIL: networking crates in the build: $($found -join ', ')"
    exit 1
}
Write-Output "PASS: none of the $(@($Names).Count) crates in the build is a networking crate."
exit 0
