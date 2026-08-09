# Import old agent usage data from a previous Windows install's profile
# (e.g. D:\Users\deki) into the current profile, so TokenTray can read it.
#
# Requires elevation (takes ownership of the old profile's ACLs).
# Usage:  powershell -ExecutionPolicy Bypass -File scripts\import-old-data.ps1
#         powershell -ExecutionPolicy Bypass -File scripts\import-old-data.ps1 -ListOnly
#         powershell -ExecutionPolicy Bypass -File scripts\import-old-data.ps1 -OldProfile "D:\Users\deki"

param(
    [string]$OldProfile = "D:\Users\deki",
    [switch]$ListOnly,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

function Write-Step($msg) { Write-Host "`n=== $msg ===" -ForegroundColor Cyan }
function Write-Info($msg) { Write-Host "  $msg" -ForegroundColor Gray }
function Write-Ok($msg) { Write-Host "  OK: $msg" -ForegroundColor Green }

# --- elevation ------------------------------------------------------------

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not $isAdmin) {
    Write-Host "Relaunching elevated (UAC prompt)..."
    $args = @("-ExecutionPolicy", "Bypass", "-File", "`"$($PSCommandPath)`"", "-OldProfile", "`"$OldProfile`"")
    if ($ListOnly) { $args += "-ListOnly" }
    if ($Force) { $args += "-Force" }
    Start-Process powershell -Verb RunAs -ArgumentList $args
    exit
}

$current = $env:USERPROFILE
Write-Step "Source: $OldProfile"
Write-Step "Target: $current"

if (-not (Test-Path $OldProfile)) {
    Write-Host "Old profile not found." -ForegroundColor Red
    exit 1
}

# --- unlock the old profile (if needed) -----------------------------------

Write-Step "Unlocking old profile ACLs"
function Unlock-Path($path) {
    if (-not (Test-Path $path)) { return }
    $out = icacls $path 2>&1
    if ($LASTEXITCODE -ne 0 -or ($out -join " ") -match "Access is denied") {
        Write-Info "Taking ownership of $path ..."
        takeown /f $path /r /d y 2>&1 | Out-Null
        icacls $path /grant "$($env:USERNAME):F" /t /c 2>&1 | Out-Null
        Write-Ok "unlocked $path"
    } else {
        Write-Ok "already readable: $path"
    }
}

Unlock-Path $OldProfile
Unlock-Path "$OldProfile\.claude"
Unlock-Path "$OldProfile\.claude\projects"
Unlock-Path "$OldProfile\.codex"
Unlock-Path "$OldProfile\.codex\sessions"
Unlock-Path "$OldProfile\.local"
Unlock-Path "$OldProfile\.local\share\opencode"

# --- inventory -------------------------------------------------------------

function Get-DirInfo($path) {
    if (-not (Test-Path $path)) { return $null }
    $files = Get-ChildItem $path -Recurse -File -ErrorAction SilentlyContinue
    $size = ($files | Measure-Object -Property Length -Sum).Sum
    $oldest = $files | Sort-Object LastWriteTime | Select-Object -First 1
    $newest = $files | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    [pscustomobject]@{
        Count   = $files.Count
        Size    = $size
        Oldest  = if ($oldest) { $oldest.LastWriteTime.ToString("yyyy-MM-dd") } else { "n/a" }
        Newest  = if ($newest) { $newest.LastWriteTime.ToString("yyyy-MM-dd") } else { "n/a" }
    }
}

Write-Step "Found data"
$claude   = Get-DirInfo "$OldProfile\.claude\projects"
$codex    = Get-DirInfo "$OldProfile\.codex"
$opencode = Get-DirInfo "$OldProfile\.local\share\opencode"

if (-not $claude -and -not $codex -and -not $opencode) {
    Write-Host "No agent data found in this profile." -ForegroundColor Yellow
    exit 0
}
if ($claude)   { Write-Info ("Claude Code : {0} files, {1} MB, {2} .. {3}" -f $claude.Count, [math]::Round($claude.Size/1MB), $claude.Oldest, $claude.Newest) }
if ($codex)    { Write-Info ("Codex CLI    : {0} files, {1} MB, {2} .. {3}" -f $codex.Count, [math]::Round($codex.Size/1MB), $codex.Oldest, $codex.Newest) }
if ($opencode) { Write-Info ("OpenCode     : {0} files, {1} MB, {2} .. {3}" -f $opencode.Count, [math]::Round($opencode.Size/1MB), $opencode.Oldest, $opencode.Newest) }

if ($ListOnly) {
    Write-Host "`n(ListOnly mode - nothing was copied)" -ForegroundColor Yellow
    exit 0
}

# --- merge into current profile --------------------------------------------

function Copy-Merge($src, $dst, $what) {
    if (-not (Test-Path $src)) { return }
    New-Item -ItemType Directory -Force -Path $dst | Out-Null
    $n = 0
    robocopy $src $dst /E /XO /NJH /NJS /NP /NFL /NDL | Out-Null
    if ($LASTEXITCODE -lt 8) {
        Write-Ok "$what merged (older files kept on both sides)"
    } else {
        Write-Host "robocopy reported an error for $what" -ForegroundColor Yellow
    }
}

Write-Step "Importing"

if ($claude) {
    Copy-Merge "$OldProfile\.claude\projects" "$current\.claude\projects" "Claude Code sessions"
}
if ($codex) {
    Copy-Merge "$OldProfile\.codex" "$current\.codex" "Codex CLI data"
}
if ($opencode) {
    $db = "$OldProfile\.local\share\opencode\opencode.db"
    if (Test-Path $db) {
        New-Item -ItemType Directory -Force -Path "$current\.local\share\opencode" | Out-Null
        foreach ($ext in @("", "-wal", "-shm")) {
            $src = $db + $ext
            $dst = "$current\.local\share\opencode\opencode.db" + $ext
            if (Test-Path $src) {
                Copy-Item $src $dst -Force
                Write-Ok "copied opencode.db$ext ($([math]::Round((Get-Item $src).Length/1MB)) MB)"
            }
        }
    } else {
        Copy-Merge "$OldProfile\.local\share\opencode" "$current\.local\share\opencode" "OpenCode data"
    }
}

# The old Claude Code OAuth login (same account, same person). Copying it lets
# TokenTray's live quota check work even though the current install has no
# credentials file. Harmless if expired.
$oldCreds = "$OldProfile\.claude\.credentials.json"
if (Test-Path $oldCreds) {
    $dstCreds = "$current\.claude\.credentials.json"
    if (-not (Test-Path $dstCreds)) {
        Copy-Item $oldCreds $dstCreds -Force
        Write-Ok "copied Claude Code login (.credentials.json) so live quotas can work"
    } else {
        Write-Info "current .credentials.json already exists - keeping it"
    }
}

Write-Host "`nDone. Launch TokenTray and hit Refresh to see the imported history." -ForegroundColor Green
Write-Host "NOTE: the old Claude .claude.json config was NOT imported (it can break the current install)." -ForegroundColor DarkGray
