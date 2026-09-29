<#
.SYNOPSIS
  Build / run / inspect the Jodd Tauri app on Windows from PowerShell.

.DESCRIPTION
  One wrapper so you don't have to remember the npm/cargo/npx commands - and
  so Windows PowerShell 5.1 users don't trip over bash-style '&&'. Every
  external command is checked via $LASTEXITCODE and stops the script on
  failure. ASCII-only on purpose: Windows PowerShell 5.1 reads .ps1 files as
  the system ANSI codepage, so any non-ASCII char would corrupt the file.

.PARAMETER Check
  Compile-only gate: cargo build (backend) + vite build (frontend). No app
  launch. This is the closest thing the project currently has to "run tests".

.PARAMETER Build
  Produce a release build (npm run tauri build).

.PARAMETER Tags
  Copy the live SQLite DB and dump the note_tags table - handy for verifying
  the tags feature. Uses sqlite3.exe if on PATH, otherwise points you at the
  copied file to open in DB Browser for SQLite.

.EXAMPLE
  .\dev.ps1            # ensure deps, then launch the dev app
  .\dev.ps1 -Check     # just compile backend + frontend
  .\dev.ps1 -Build     # release build
  .\dev.ps1 -Tags      # inspect the tags stored in SQLite

.NOTES
  If PowerShell blocks the script, run it once as:
    powershell -ExecutionPolicy Bypass -File .\dev.ps1
  Prereqs: Node.js, Rust (MSVC toolchain), WebView2, VS Build Tools (C++).
#>
[CmdletBinding()]
param(
    [switch]$Check,
    [switch]$Build,
    [switch]$Tags
)

$ErrorActionPreference = 'Stop'
# Anchor to the repo root (this script's folder) regardless of where it's run.
Set-Location -LiteralPath $PSScriptRoot

function Assert-Tool {
    param([string]$Name, [string]$Hint)
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Write-Host "MISSING: '$Name' is not on PATH. $Hint" -ForegroundColor Red
        exit 1
    }
}

function Invoke-Step {
    param([string]$Desc, [scriptblock]$Cmd)
    Write-Host ">> $Desc" -ForegroundColor Cyan
    & $Cmd
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED ($LASTEXITCODE): $Desc" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

# --- Tags inspection mode (no build/run) ---------------------------------
if ($Tags) {
    $src = Join-Path $env:APPDATA 'jodd\jodd.sqlite3'
    if (-not (Test-Path $src)) {
        Write-Host "No DB at $src - launch the app at least once first." -ForegroundColor Yellow
        exit 1
    }
    $dst = Join-Path $env:TEMP 'jodd_copy.sqlite3'
    Copy-Item $src $dst -Force
    foreach ($ext in '-wal', '-shm') {
        if (Test-Path "$src$ext") { Copy-Item "$src$ext" "$dst$ext" -Force }
    }
    if (Get-Command sqlite3 -ErrorAction SilentlyContinue) {
        Write-Host "=== tag -> note count ===" -ForegroundColor Cyan
        sqlite3 $dst "SELECT tag, COUNT(*) FROM note_tags GROUP BY tag ORDER BY tag;"
        Write-Host "=== (uuid, tag) rows ===" -ForegroundColor Cyan
        sqlite3 $dst "SELECT uuid, tag FROM note_tags ORDER BY uuid, tag;"
    }
    else {
        Write-Host "sqlite3.exe not on PATH. Open this copy in DB Browser for SQLite:" -ForegroundColor Yellow
        Write-Host "  $dst"
    }
    exit 0
}

# --- Prerequisites -------------------------------------------------------
Assert-Tool -Name node  -Hint 'Install Node.js: https://nodejs.org'
Assert-Tool -Name npm   -Hint 'Ships with Node.js.'
Assert-Tool -Name cargo -Hint 'Install Rust (MSVC toolchain): https://rustup.rs'

# --- Dependencies (only if missing) --------------------------------------
if (-not (Test-Path 'node_modules')) {
    Invoke-Step -Desc 'npm install' -Cmd { npm install }
}

# --- Modes ---------------------------------------------------------------
if ($Check) {
    Invoke-Step -Desc 'cargo build (backend)' -Cmd {
        Push-Location src-tauri
        cargo build
        Pop-Location
    }
    Invoke-Step -Desc 'vite build (frontend)' -Cmd { npx vite build }
    Write-Host "OK - backend and frontend both compiled." -ForegroundColor Green
    exit 0
}

if ($Build) {
    Invoke-Step -Desc 'tauri build (release)' -Cmd { npm run tauri build }
    exit 0
}

# Default: launch the dev app (this rebuilds the Rust backend itself).
Invoke-Step -Desc 'tauri dev' -Cmd { npm run tauri dev }
