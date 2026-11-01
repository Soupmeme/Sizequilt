# Shared build settings. Compilers record source paths, which would put this
# machine's username in a file we hand to other people, so each is rewritten.
# Dot-source it: . "$PSScriptRoot\tools\env.ps1"
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$project = Split-Path $PSScriptRoot -Parent
$flags = @(
    "--remap-path-prefix=$env:USERPROFILE\.cargo\registry\src=deps",
    "--remap-path-prefix=$env:USERPROFILE\.cargo=cargo",
    "--remap-path-prefix=$env:USERPROFILE\.rustup=rust",
    "--remap-path-prefix=$env:USERPROFILE=home",
    "--remap-path-prefix=$project=project"
)
# The encoded form, because a path may hold spaces.
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
