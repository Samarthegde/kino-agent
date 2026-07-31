<#
.SYNOPSIS
    Install kino-agent as a Windows service.

.DESCRIPTION
    Copies the binary into Program Files, registers a service that starts at boot
    and restarts on failure, and starts it. Re-running is safe: it updates the
    binary and config in place and restarts the service.

    Must be run from an elevated (Administrator) PowerShell.

.EXAMPLE
    .\install.ps1 -RelayUrl wss://kino-relay.example.com

.EXAMPLE
    .\install.ps1 -RelayUrl wss://kino-relay.example.com -AgentId 3f9a1c2b-...
#>
[CmdletBinding()]
param(
    [string] $RelayUrl,
    # Kino Cloud: discover relays and refresh tokens via kino-control.
    [string] $ControlUrl,

    # Omit to keep the existing id, or mint a new one on a first install.
    [string] $AgentId,

    # Bearer token, when the relay requires auth (its static RELAY_TOKEN, or a
    # kino-control agent token). Omit to keep the existing one on a re-run.
    [string] $Token,

    # Kino Cloud machine credential (kca_...); requires -ControlUrl.
    [string] $AgentKey,

    [string] $InstallDir = "$env:ProgramFiles\kino-agent"
)

$ErrorActionPreference = 'Stop'

$ServiceName = 'kino-agent'
$ExePath     = Join-Path $InstallDir 'kino-agent.exe'
$LogPath     = Join-Path $env:ProgramData 'kino-agent\kino-agent.log'
$ConfigPath  = Join-Path $env:ProgramData 'kino-agent\config.json'

# ── Preconditions ────────────────────────────────────────────────────────────
$identity  = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Must run from an elevated PowerShell (Run as Administrator).'
}

# The agent forwards to the local SSH daemon, so there needs to be one.
$sshd = Get-Service -Name sshd -ErrorAction SilentlyContinue
if (-not $sshd) {
    Write-Warning @'
OpenSSH Server does not appear to be installed. The agent will connect out to the
relay, but every session will fail until an SSH server is listening on 127.0.0.1:22.
Install it with:

    Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
    Set-Service -Name sshd -StartupType Automatic
    Start-Service sshd
'@
} elseif ($sshd.Status -ne 'Running') {
    Write-Warning "OpenSSH Server is installed but not running (status: $($sshd.Status))."
}

# ── Reuse existing config when re-running ────────────────────────────────────
if (Test-Path $ConfigPath) {
    $existing = Get-Content $ConfigPath -Raw | ConvertFrom-Json
    if (-not $AgentId) {
        $AgentId = $existing.AgentId
        Write-Host "==> Reusing existing agent id: $AgentId"
    }
    foreach ($prop in 'Token', 'RelayUrl', 'ControlUrl', 'AgentKey') {
        if (-not (Get-Variable $prop -ValueOnly) -and $existing.PSObject.Properties[$prop]) {
            Set-Variable $prop $existing.$prop
        }
    }
}
if (-not $RelayUrl -and -not $ControlUrl) {
    throw 'Pass -RelayUrl or -ControlUrl (or both).'
}
if (-not $AgentId) {
    $AgentId = [guid]::NewGuid().ToString()
    Write-Host "==> Generated agent id: $AgentId"
}

# ── Locate the binary ────────────────────────────────────────────────────────
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$candidates = @(
    (Join-Path $scriptDir 'kino-agent.exe'),
    (Join-Path $scriptDir '..\target\release\kino-agent.exe'),
    (Join-Path $scriptDir '..\target\x86_64-pc-windows-msvc\release\kino-agent.exe')
)
$source = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $source) {
    throw "kino-agent.exe not found. Build it first (cargo build --release), or drop the .exe next to this script."
}

# ── Stop any existing service before overwriting the binary ──────────────────
$svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($svc -and $svc.Status -ne 'Stopped') {
    Write-Host "==> Stopping existing service"
    Stop-Service -Name $ServiceName -Force
    $svc.WaitForStatus('Stopped', '00:00:30')
}

Write-Host "==> Installing binary to $ExePath"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -Path $source -Destination $ExePath -Force

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $LogPath) | Out-Null
[pscustomobject]@{
    RelayUrl = $RelayUrl; ControlUrl = $ControlUrl; AgentId = $AgentId
    Token = $Token; AgentKey = $AgentKey
} | ConvertTo-Json | Set-Content -Path $ConfigPath -Encoding UTF8

# The SCM launches the exe with this exact command line, so clap picks the flags up
# the same way it does on a console. Quote every path - Program Files has a space.
# Note: the command line (and config.json) are readable by local users - the
# relay token is a relay-access secret, not an SSH credential; scope it to this
# agent id (kino-control) so a leak exposes only this host's relay slot.
$binPath = '"{0}" --agent-id "{1}" --log-file "{2}"' -f $ExePath, $AgentId, $LogPath
if ($RelayUrl)   { $binPath += ' --relay-url "{0}"' -f $RelayUrl }
if ($ControlUrl) { $binPath += ' --control-url "{0}"' -f $ControlUrl }
if ($Token)      { $binPath += ' --token "{0}"' -f $Token }
if ($AgentKey)   { $binPath += ' --agent-key "{0}"' -f $AgentKey }

if ($svc) {
    Write-Host "==> Updating existing service"
    # sc.exe, not Set-Service: only it can rewrite binPath. The space after '=' is required.
    & sc.exe config $ServiceName binPath= $binPath start= auto | Out-Null
} else {
    Write-Host "==> Registering service '$ServiceName'"
    & sc.exe create $ServiceName binPath= $binPath start= auto DisplayName= "Kino Agent" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "sc.exe create failed with exit code $LASTEXITCODE" }
}

& sc.exe description $ServiceName "Reaches this host's SSH daemon out through a Kino relay." | Out-Null

# Restart on failure - the equivalent of systemd's Restart=always. Reset the crash
# counter after 24h so a long-lived agent isn't permanently marked failed.
& sc.exe failure $ServiceName reset= 86400 actions= restart/5000/restart/5000/restart/30000 | Out-Null

Write-Host "==> Starting service"
Start-Service -Name $ServiceName
Start-Sleep -Seconds 2

$svc = Get-Service -Name $ServiceName
if ($svc.Status -eq 'Running') {
    Write-Host "`n[OK] kino-agent is running."
} else {
    Write-Warning "kino-agent is not running (status: $($svc.Status)). Check $LogPath"
}

@"

  Relay:    $RelayUrl
  Agent ID: $AgentId
  Token:    $(if ($Token) { 'set' } else { 'none' })
  Logs:     $LogPath

Add this host in Kino SSH Manager with connection mode "Kino Agent",
using the relay URL and agent ID above.

  Status:   Get-Service $ServiceName
  Logs:     Get-Content '$LogPath' -Tail 50 -Wait
  Restart:  Restart-Service $ServiceName
  Remove:   Stop-Service $ServiceName; sc.exe delete $ServiceName
"@ | Write-Host
