<#
.SYNOPSIS
    One-line installer for kino-agent (Windows service).

.DESCRIPTION
    Downloads the prebuilt kino-agent.exe from the latest GitHub release (no Rust
    toolchain needed), then registers the Windows service via install.ps1.

    Run from an elevated PowerShell:

      $s = irm https://raw.githubusercontent.com/Samarthegde/kino-agent/main/packaging/bootstrap.ps1
      & ([scriptblock]::Create($s)) -RelayUrl wss://kino-relay.example.com
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $RelayUrl,

    [string] $AgentId,
    [string] $Version = 'latest',
    [string] $Repo = 'Samarthegde/kino-agent'
)

$ErrorActionPreference = 'Stop'
$asset = 'kino-agent-x86_64-pc-windows-msvc.exe'

# ── elevation check ──────────────────────────────────────────────────────────
$principal = [Security.Principal.WindowsPrincipal]::new(
    [Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Must run from an elevated PowerShell (Run as Administrator).'
}

# ── resolve the release tag ──────────────────────────────────────────────────
if ($Version -eq 'latest') {
    $rel = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    $tag = $rel.tag_name
    if (-not $tag) { throw "Could not determine the latest release of $Repo (no releases yet?)." }
} else {
    $tag = $Version
}
Write-Host "==> Installing kino-agent $tag (x86_64-pc-windows-msvc)"

# ── download binary + installer into a temp dir, then delegate ───────────────
$tmp = Join-Path $env:TEMP ("kino-agent-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
    $base = "https://github.com/$Repo/releases/download/$tag"
    $raw  = "https://raw.githubusercontent.com/$Repo/$tag/packaging"

    Write-Host "==> Downloading binary"
    Invoke-WebRequest "$base/$asset" -OutFile (Join-Path $tmp 'kino-agent.exe')

    Write-Host "==> Downloading installer"
    Invoke-WebRequest "$raw/install.ps1" -OutFile (Join-Path $tmp 'install.ps1')

    # install.ps1 picks up the .exe sitting next to it.
    $params = @{ RelayUrl = $RelayUrl }
    if ($AgentId) { $params.AgentId = $AgentId }
    & (Join-Path $tmp 'install.ps1') @params
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
