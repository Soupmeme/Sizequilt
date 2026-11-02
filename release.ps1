# Builds a release for testers. Any failed check stops it before a kit is made.
# Result: dist\Sizequilt-tester-kit.zip, and the hash to send separately.
$root = $PSScriptRoot
. "$root\tools\env.ps1"

function Stop-Release($why) {
    Write-Output "RELEASE STOPPED: $why"
    exit 1
}

Set-Location $root
Write-Output '--- the pinned compiler'
$pinned = (Select-String -Path "$root\rust-toolchain.toml" -Pattern '^channel = "(.+)"').Matches[0].Groups[1].Value
$rustc = rustc -vV
if ($rustc -notcontains 'host: x86_64-pc-windows-gnu') { Stop-Release 'the compiler is not the GNU one, so the build would differ' }
if ($rustc -notcontains "release: $pinned") { Stop-Release "the compiler is not Rust $pinned, as rust-toolchain.toml asks" }
rustc -V

Write-Output '--- tests'
cargo test --locked -p sizequilt --lib
if ($LASTEXITCODE -ne 0) { Stop-Release 'the tests failed' }

Write-Output '--- no socket types in our code or the vendored engine'
cargo clippy --locked --workspace -- -D clippy::disallowed_types
if ($LASTEXITCODE -ne 0) { Stop-Release 'code uses a network type' }

Write-Output '--- no networking crates'
& "$root\tools\check-deps.ps1" -Names 'serde', 'reqwest' | Out-Null
if ($LASTEXITCODE -eq 0) { Stop-Release 'the crate check passed a list holding reqwest, so it is broken' }
& "$root\tools\check-deps.ps1"
if ($LASTEXITCODE -ne 0) { Stop-Release 'a networking crate is in the build' }

Write-Output '--- build'
cargo build --locked --release -p sizequilt
if ($LASTEXITCODE -ne 0) { Stop-Release 'the build failed' }

Write-Output '--- the file check can fail'
& "$root\tester-kit\check-app.ps1" -Path "$env:SystemRoot\System32\curl.exe" | Out-Null
if ($LASTEXITCODE -eq 0) { Stop-Release 'the file check passed curl.exe, so it is broken' }

Write-Output '--- the built file'
$exe = Join-Path $root 'target\release\sizequilt.exe'
& "$root\tester-kit\check-app.ps1" -Path $exe
if ($LASTEXITCODE -ne 0) { Stop-Release 'the built file contains networking' }

Write-Output '--- no personal paths'
$bait = Join-Path $env:TEMP 'sq-clean-check.bin'
[IO.File]::WriteAllText($bait, "x$env:USERPROFILE\y")
& "$root\tools\check-clean.ps1" -Path $bait | Out-Null
$baitStatus = $LASTEXITCODE
Remove-Item -LiteralPath $bait -Force
if ($baitStatus -eq 0) { Stop-Release 'the path check passed a file holding a personal path, so it is broken' }
& "$root\tools\check-clean.ps1" -Path $exe
if ($LASTEXITCODE -ne 0) { Stop-Release 'the built file holds a personal path' }

$dist = Join-Path $root 'dist'
$kit = Join-Path $dist 'Sizequilt-tester-kit'
if (Test-Path -LiteralPath $kit) { Remove-Item -LiteralPath $kit -Recurse -Force }
New-Item -ItemType Directory -Force -Path $kit | Out-Null
Copy-Item $exe (Join-Path $kit 'Sizequilt.exe')
Copy-Item $exe (Join-Path $dist 'Sizequilt.exe') -Force
foreach ($file in 'check-app.ps1', 'block-network.ps1', 'PRIVACY.md') {
    Copy-Item (Join-Path $root "tester-kit\$file") $kit
}
Copy-Item (Join-Path $root 'vendor\LICENSE-TreeMap.txt') $kit
$zip = "$kit.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path "$kit\*" -DestinationPath $zip
# The program alone: what a false-positive report to an antivirus vendor needs.
$single = Join-Path $dist 'Sizequilt-app-only.zip'
if (Test-Path -LiteralPath $single) { Remove-Item -LiteralPath $single -Force }
Compress-Archive -Path (Join-Path $kit 'Sizequilt.exe') -DestinationPath $single

$hash = (Get-FileHash (Join-Path $kit 'Sizequilt.exe') -Algorithm SHA256).Hash
Set-Content -Path (Join-Path $dist 'SHA256-send-separately.txt') -Value $hash -Encoding ascii
Write-Output ''
Write-Output "Kit:  $zip"
Write-Output "Hash: $hash"
Write-Output 'Send the hash through a different channel from the zip.'
exit 0
