# kino-agent

[![build](https://github.com/Samarthegde/kino-agent/actions/workflows/build.yml/badge.svg)](https://github.com/Samarthegde/kino-agent/actions/workflows/build.yml)
[![license: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)

A tiny, self-contained agent that exposes a machine's local SSH daemon to
[Kino SSH Manager](https://github.com/Samarthegde/kino-ssh-manager) through a
public [kino-relay](https://github.com/Samarthegde/kino-relay) - **without
opening any inbound port** on the machine.

The agent makes only *outbound* WebSocket connections to the relay. That means a
host behind NAT, CGNAT, or a locked-down firewall - a homelab box, a laptop, a
cloud VM with no public SSH - becomes reachable from your SSH manager as long as
it can reach the relay.

---

## How it works

1. On start, the agent opens a persistent **control** WebSocket to the relay at
   `/ws/control?agent_id=<id>` and waits.
2. When the manager wants to connect, the relay sends a `new_connection` message
   over that control channel.
3. The agent dials a fresh **data** WebSocket back to the relay
   (`/ws/agent/data?session_id=<id>`), opens a TCP connection to the local SSH
   daemon on `127.0.0.1:22`, and pipes bytes between the two.
4. The relay splices that data socket to the manager's socket. From there it's a
   normal SSH session - end-to-end between your SSH client and `sshd`; the relay
   only moves encrypted bytes and never sees your SSH credentials.

The control channel is kept alive with 30-second WebSocket pings, and the agent
reconnects automatically (every 5s) if the relay or network drops.

---

## Install

### Kino Cloud (easiest)

Add a machine in kino-control (web UI or Kino SSH Manager) and run the
personalized command it gives you:

```bash
curl -fsSL https://kino.dpdns.org/install/<one-time-enroll-key> | sudo sh
```

That registers the machine, installs the agent as a service, and configures it
with `--control-url` + `--agent-key`: the agent discovers the fastest relay
and refreshes its own short-lived tokens - nothing else to configure, ever.

### One-liner (self-hosted relay)

Downloads the prebuilt binary from the latest GitHub release and registers a
service that starts at boot and restarts on failure. No Rust toolchain needed.

**Linux (systemd):**

```bash
curl -fsSL https://raw.githubusercontent.com/Samarthegde/kino-agent/main/packaging/bootstrap.sh \
  | sudo sh -s -- --relay-url wss://relay.example.com
```

**Windows (elevated PowerShell):**

```powershell
$s = irm https://raw.githubusercontent.com/Samarthegde/kino-agent/main/packaging/bootstrap.ps1
& ([scriptblock]::Create($s)) -RelayUrl wss://relay.example.com
```

The installer prints the **agent ID** it generated. Add a host in Kino SSH
Manager with connection mode **Kino Agent**, and paste in the relay URL and that
agent ID. If the relay requires auth, append `--token <token>` (Linux) /
`-Token <token>` (Windows) - see [Configuration](#configuration).

> Windows does not run an SSH server by default - install one, or every session
> will fail. See [packaging/README.md](packaging/README.md#windows-needs-an-ssh-server).

See **[packaging/README.md](packaging/README.md)** for manual installation, file
locations, log commands, and uninstall steps.

### Run it directly (no service)

Handy for trying it out or debugging. The same binary runs in the foreground on
every platform:

```bash
# Fixed relay:
kino-agent --relay-url wss://relay.example.com --agent-id my-agent

# Kino Cloud (relay discovered, tokens refreshed automatically):
kino-agent --control-url https://kino.dpdns.org --agent-key kca_...
```

---

## Configuration

Every option can be set by flag or environment variable (the flag wins):

| Flag | Env var | Required | Description |
|------|---------|----------|-------------|
| `--relay-url` | `KINO_RELAY_URL` | yes* | Fixed relay base URL, e.g. `wss://relay.example.com`. Use `ws://` only for local testing. *Optional when `--control-url` is set (then it's the fallback). |
| `--control-url` | `KINO_CONTROL_URL` | no | [kino-control](https://github.com/Samarthegde/kino-control) base URL. The agent fetches the enrolled relays (policy-ordered), latency-races the top few, parks on the fastest, and reports its location so managers can find it. Re-picks automatically if the relay dies. Requires `--token` or `--agent-key`. |
| `--agent-key` | `KINO_AGENT_KEY` | no | Kino Cloud machine credential (`kca_...`) from the install flow. The agent trades it for short-lived relay tokens at startup and auto-refreshes them (at ~3/4 of each token's life). Makes `--agent-id` optional (kino-control assigns it) and `--token` unnecessary. Requires `--control-url`. |
| `--agent-id`  | `KINO_AGENT_ID`  | yes* | Stable unique id for this host. Any string; a UUID is a good default. *Optional with `--agent-key`, where kino-control assigns and states it. |
| `--token`     | `KINO_TOKEN`     | no  | Bearer token, when the relay requires auth: its static `RELAY_TOKEN`, or an agent token issued by [kino-control](https://github.com/Samarthegde/kino-control). |
| `--log-file`  | `KINO_LOG_FILE`  | no  | Append logs to this file instead of stdout. Set automatically for the Windows service (which has no console). |
| -             | `RUST_LOG`       | no  | Log verbosity (`error`/`warn`/`info`/`debug`/`trace`). Defaults to `info`. |

The `agent_id` is how the relay and manager address this specific machine, so
keep it stable - changing it means updating the host entry in the manager. In
Kino Cloud mode it is assigned once at install and refreshed from kino-control,
so there is nothing to keep track of.

---

## Building from source

Requires a recent stable Rust (edition 2024, i.e. Rust ≥ 1.85):

```bash
cargo build --release
# binary at target/release/kino-agent
```

TLS is built in via [rustls](https://github.com/rustls/rustls) (the `ring`
backend), so there is **no OpenSSL system dependency** - the binary is portable
across distros. `wss://` works out of the box.

### Cross-compiling for Windows from Linux

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt-get install -y gcc-mingw-w64-x86-64   # provides the cross toolchain
cargo build --release --target x86_64-pc-windows-gnu
```

CI (`.github/workflows/build.yml`) builds and lints Linux x86_64, Linux aarch64,
and Windows x86_64 on every push, and attaches the binaries to a GitHub release
on any `v*` tag.

---

## Project layout

```
src/
  main.rs         Entry point, connect/serve/reconnect loop, session bridging
  win_service.rs  Windows Service Control Manager integration (cfg(windows))
packaging/
  bootstrap.sh    Linux one-liner installer (downloads a release, delegates)
  bootstrap.ps1   Windows one-liner installer
  install.sh      systemd service installer (local/built binary)
  install.ps1     Windows service installer
  kino-agent.service  Hardened systemd unit
  README.md       Full installation & operations guide
.github/workflows/build.yml   CI matrix + release publishing
CHANGELOG.md      Release history
```

---

## Security notes

- **The relay is trusted to route, not to read.** SSH is end-to-end encrypted
  between your client and `sshd`, so a relay operator cannot read your session or
  credentials. But a *malicious* relay could refuse or redirect connections - run
  your own relay, or trust the operator.
- **Host-key verification still applies.** Kino SSH Manager pins each agent host's
  SSH fingerprint (keyed by agent id), so a swapped or spoofed backend is caught
  the same way OpenSSH's `known_hosts` catches it.
- **The agent needs no privileges.** The systemd unit runs it as a dynamic,
  unprivileged user with a read-only filesystem and a syscall filter - it only
  dials out and connects to `127.0.0.1:22`.
- **Use a relay with auth enabled for anything public.** The relay accepts a
  bearer token (`--token`): either its static `RELAY_TOKEN` or a scoped agent
  token issued by [kino-control](https://github.com/Samarthegde/kino-control).
  On a relay running *without* auth, anyone who can reach it and knows an
  agent's id can open an SSH transport to that host - where your `sshd`'s own
  authentication is the gate. Keep `sshd` locked down regardless.

Found a vulnerability? Please report it privately (see
[CONTRIBUTING.md](CONTRIBUTING.md)) rather than opening a public issue.

---

## Contributing

Contributions are welcome - see **[CONTRIBUTING.md](CONTRIBUTING.md)**.

## License

[GNU GPL-3.0](LICENSE) © 2026 Samarth Kombemane
