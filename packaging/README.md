# Installing kino-agent

The agent runs on the machine you want to reach. It makes only **outbound**
connections to the relay, so the host needs no inbound firewall rule and no public
IP - that's the whole point of it.

It forwards to whatever SSH daemon is listening on `127.0.0.1:22`, so one must be
running locally.

---

## Kino Cloud install

If the machine was added through kino-control, use the personalized command it
generated - it registers the machine and passes `--control-url`/`--agent-key`
to these installers automatically:

```bash
curl -fsSL https://kino.dpdns.org/install/<one-time-enroll-key> | sudo sh
```

Both installers also accept the cloud flags directly:
`--control-url <url> --agent-key <kca_...>` (sh) /
`-ControlUrl <url> -AgentKey <kca_...>` (PowerShell); `--relay-url` becomes
optional then (kept as a fallback if kino-control is unreachable).

## Quick install (one-liner, self-hosted relay)

Downloads the prebuilt binary from the latest GitHub release and registers the
service. No Rust toolchain required.

**Linux:**

```bash
curl -fsSL https://raw.githubusercontent.com/Samarthegde/kino-agent/main/packaging/bootstrap.sh \
  | sudo sh -s -- --relay-url wss://kino-relay.example.com
```

**Windows** (elevated PowerShell):

```powershell
$s = irm https://raw.githubusercontent.com/Samarthegde/kino-agent/main/packaging/bootstrap.ps1
& ([scriptblock]::Create($s)) -RelayUrl wss://kino-relay.example.com
```

Add `--agent-id <id>` / `-AgentId <id>` to pin a specific id; otherwise one is
generated. If the relay requires auth, pass `--token <token>` / `-Token <token>`
(its static `RELAY_TOKEN`, or an agent token issued by kino-control). Pin a
release with `--version vX.Y.Z` / `-Version vX.Y.Z`.

> The one-liners require published GitHub releases (the `build` workflow attaches
> the binaries on every `v*` tag). Until the first release exists, use the
> manual steps below with a locally built binary.

---

## Manual - Linux (systemd)

```bash
sudo ./install.sh --relay-url wss://kino-relay.example.com [--token <token>]
```

Prints the agent ID it generated - paste that, and the relay URL, into Kino SSH
Manager when adding the host (connection mode **Kino Agent**).

Re-running is safe: it updates the binary and config in place, keeps the existing
agent ID (and token), and restarts the service. The token is stored as
`KINO_TOKEN` in the root-only env file, not on the unit's command line.

| | |
|---|---|
| Binary | `/usr/local/bin/kino-agent` |
| Config | `/etc/kino-agent/kino-agent.env` (`0600`, root-owned) |
| Unit | `/etc/systemd/system/kino-agent.service` |
| Logs | `journalctl -u kino-agent -f` |
| Remove | `systemctl disable --now kino-agent && rm -rf /etc/systemd/system/kino-agent.service /etc/kino-agent /usr/local/bin/kino-agent` |

The unit is sandboxed (`DynamicUser`, no capabilities, read-only filesystem,
syscall filter): the agent only ever needs to dial out and connect to
`127.0.0.1:22`, so it is granted nothing else.

---

## Manual - Windows (service)

From an **elevated** PowerShell:

```powershell
.\install.ps1 -RelayUrl wss://kino-relay.example.com [-Token <token>]
```

Registers a real Windows service that starts at boot and restarts on failure
(the equivalent of systemd's `Restart=always`). Re-running keeps the existing
agent id and token unless you pass new ones.

> On Windows the token ends up on the service command line and in `config.json`,
> both readable by local users of that machine. It's a relay-access secret, not
> an SSH credential - use a kino-control token scoped to this agent id so a
> leak exposes only this host's own relay slot.

| | |
|---|---|
| Binary | `C:\Program Files\kino-agent\kino-agent.exe` |
| Config | `C:\ProgramData\kino-agent\config.json` |
| Logs | `C:\ProgramData\kino-agent\kino-agent.log` |
| Status | `Get-Service kino-agent` |
| Tail logs | `Get-Content 'C:\ProgramData\kino-agent\kino-agent.log' -Tail 50 -Wait` |
| Remove | `Stop-Service kino-agent; sc.exe delete kino-agent` |

A Windows service has no console, so the agent logs to a file rather than stdout -
`install.ps1` passes `--log-file` for you.

### Windows needs an SSH server

Unlike most Linux distros, Windows does not run one by default. Install it once:

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service -Name sshd -StartupType Automatic
Start-Service sshd
```

`install.ps1` warns if it can't find one. Without it the agent will connect to the
relay happily and then fail every session.

---

## Running it by hand

Useful for debugging - the same binary runs in the foreground on both platforms:

```bash
kino-agent --relay-url wss://kino-relay.example.com --agent-id my-agent-id
```

Config can come from flags or the environment (`KINO_RELAY_URL`, `KINO_AGENT_ID`,
`KINO_TOKEN`, `KINO_LOG_FILE`). Logging defaults to `info`; set `RUST_LOG=debug`
for more.
