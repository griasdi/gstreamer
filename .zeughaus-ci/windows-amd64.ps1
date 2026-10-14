# /// ci
# on = ["tag v*"]
# platform = "windows-amd64"
# cache = ["target"]
# when_busy = "freeze"
# timeout_minutes = 300
# ///
# The GStreamer prefix and source for Windows, built with MSVC on the Windows
# VM: the tag check, then the uv that the GStreamer build runs its meson and
# ninja from, then dist.
$ErrorActionPreference = 'Stop'

# Runs a native command and ends the script with its exit code when it fails.
# Its stderr is shown, not turned into an error (Windows PowerShell 5.1 does
# that to the stderr of native commands under 'Stop').
function Run([string]$Command, [string[]]$Arguments = @()) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Command @Arguments 2>&1 | ForEach-Object { Write-Host "$_" }
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
    if ($code -ne 0) {
        Write-Host "$Command $($Arguments -join ' ') failed with exit code $code"
        exit $code
    }
}

# The stable toolchain, whatever `rust-toolchain.toml` a dependency ships
# (see lib.sh): rustup would otherwise install what it asks for from several
# rustc processes at once.
$env:RUSTUP_TOOLCHAIN = 'stable'

# A tag must name the release id of this checkout (check_tag in lib.sh). The
# tool prints what it expects; Run reports the failure.
if ($env:CI_EVENT -eq 'tag') {
    Run 'cargo' @('run', '--release', '--quiet', '--', 'check-tag', $env:CI_REF)
}

# Installers are downloaded, checked against a recorded SHA-256 and run
# silently; what they install stays on W: between runs.
function Get-Verified([string]$Url, [string]$Sha256, [string]$Path) {
    Run 'curl.exe' @('-fsSL', '--retry', '3', '-o', $Path, $Url)
    $hash = (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower()
    if ($hash -ne $Sha256) {
        Remove-Item $Path
        throw "hash mismatch for ${Url}: $hash"
    }
}

New-Item -ItemType Directory -Force -Path W:\tools | Out-Null

# 1. uv, the Python, meson and ninja of the GStreamer build: a fixed release,
# checked against its SHA-256 (https://github.com/astral-sh/uv/releases).
$uvVersion = '0.12.23'
$uvSha256 = '75d05de6762778c31ee183398de7dd15093fad0ed90b1f236d8205ea5ec00c90'
$uv = "W:\tools\uv-$uvVersion"
if (Test-Path (Join-Path $uv 'uv.exe')) {
    Write-Host "uv $uvVersion is already in $uv"
} else {
    $scratch = Join-Path $env:TEMP "gstreamer-ci-$($env:CI_PIPELINE)"
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    $zip = Join-Path $scratch 'uv.zip'
    Get-Verified "https://github.com/astral-sh/uv/releases/download/$uvVersion/uv-x86_64-pc-windows-msvc.zip" $uvSha256 $zip
    New-Item -ItemType Directory -Force -Path $uv | Out-Null
    Run 'tar.exe' @('-xf', $zip, '-C', $uv)
    Remove-Item -Recurse -Force $scratch
}
$env:PATH = $uv + ';' + $env:PATH

# 2. The prefix and the source, into $env:CI_OUTPUT (created by dist).
Run 'cargo' @('run', '--release', '--', 'dist', '--target', 'x86_64-pc-windows-msvc', '--out', $env:CI_OUTPUT)
