# Builds the app, or runs any cargo command. Usage: .\build.ps1 [cargo arguments]
# With no arguments it also runs the no-network checks, and fails if one does.
. "$PSScriptRoot\tools\env.ps1"
if ($args.Count -ne 0) {
    cargo @args
    exit $LASTEXITCODE
}
cargo build --release -p sizequilt
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& "$PSScriptRoot\tools\check-deps.ps1"
if ($LASTEXITCODE -ne 0) { exit 1 }
& "$PSScriptRoot\tester-kit\check-app.ps1" -Path "$PSScriptRoot\target\release\sizequilt.exe"
exit $LASTEXITCODE
