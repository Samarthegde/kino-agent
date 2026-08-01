# Changelog

All notable changes to kino-agent are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
- **The one-line installer failed on Debian and Ubuntu.** `bootstrap.sh` invoked
  `install.sh` with `sh`, but the installer is a bash script - so on any distro
  where `/bin/sh` is dash it died immediately with
  `set: Illegal option -o pipefail`, after the machine had already been
  registered. It now runs the installer with `bash`. No new release needed:
  the installer is fetched from `main` at install time.

## [0.1.1] - 2026-07-31

### Added
- **`--token` / `KINO_TOKEN`** - bearer token for relays that require auth
  (their static `RELAY_TOKEN`, or a scoped agent token issued by
  [kino-control](https://github.com/Samarthegde/kino-control)). Sent as an
  `Authorization` header on both the control and data sockets.
- **`--control-url` / `KINO_CONTROL_URL` - relay discovery.** Instead of being
  told which relay to use, the agent asks kino-control for the enrolled relays
  (returned policy-ordered), latency-probes the top few, parks on the fastest
  healthy one, and reports its location so the SSH manager can find it. Every
  reconnect re-picks, so an agent migrates automatically when its relay dies or
  a better one appears. `--relay-url` becomes optional and doubles as the
  fallback when kino-control is unreachable.
- **`--agent-key` / `KINO_AGENT_KEY` - Kino Cloud mode.** The machine
  credential installed by the one-line installer. The agent trades it for a
  short-lived relay token at startup (retrying while the network comes up) and
  refreshes in the background at ~3/4 of each token's life, so access ends
  within a day if the machine is removed or a subscription lapses. Makes
  `--agent-id` optional - kino-control states the machine's identity on every
  refresh - and replaces `--token` entirely.
- **Installers understand the new modes.** `bootstrap.sh`, `install.sh`,
  `bootstrap.ps1`, and `install.ps1` accept `--token`, `--control-url`, and
  `--agent-key` (`-Token`, `-ControlUrl`, `-AgentKey` on Windows), reuse
  existing values when re-run, and keep secrets in the root-only env file on
  Linux rather than the service command line.

### Changed
- **License: MIT - GPL-3.0.**
- README documents the Kino Cloud install (`curl -fsSL
  https://kino.samarthkombemane.com/install/<key> | sudo sh`) alongside the self-hosted
  relay flow.

## [0.1.0] - 2026-07-13

### Added
- Initial release: a self-contained agent that exposes a machine's local SSH
  daemon through a public relay using only **outbound** connections, so hosts
  behind NAT, CGNAT, or a firewall need no inbound port.
- Persistent control channel with 30-second pings and automatic reconnection,
  per-session data sockets bridged to `127.0.0.1:22`.
- Runs as a hardened systemd service (`DynamicUser`, no capabilities, read-only
  filesystem, syscall filter) or a real Windows service, with one-line
  installers for both and CI-built binaries for Linux x86_64/aarch64 and
  Windows x86_64.
