# Contributing to kino-agent

Thanks for your interest in improving kino-agent! This is a small, security-
sensitive networking daemon, so contributions get a careful review - please don't
be discouraged by questions on a PR.

## Getting started

You need a recent stable Rust toolchain (edition 2024, i.e. **Rust ≥ 1.85**):

```bash
git clone https://github.com/Samarthegde/kino-agent
cd kino-agent
cargo build
```

To try it end to end you also need a running [kino-relay](https://github.com/Samarthegde/kino-relay)
and a local SSH server on `127.0.0.1:22`:

```bash
cargo run -- --relay-url ws://localhost:3000 --agent-id dev
```

## Before you open a pull request

Please make sure these all pass - CI runs the same checks on Linux (x86_64 and
aarch64) and Windows, and treats warnings as errors:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo build --release
```

If your change touches the Windows service code (`src/win_service.rs`, behind
`cfg(windows)`), note that it is **not** compiled on a Linux dev box. Either
cross-compile it or let the CI matrix check it:

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt-get install -y gcc-mingw-w64-x86-64
cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings
```

## Pull request guidelines

- **Keep PRs focused.** One logical change per PR is much easier to review.
- **Explain the "why."** What problem does this solve, and how did you verify it?
  For networking/lifecycle changes, describe how you tested reconnection,
  keepalive, or shutdown behavior.
- **Update docs.** If you add a flag or env var, update the README table and
  `packaging/` as needed.
- **Discuss big changes first.** For anything substantial (new dependencies,
  protocol changes, new platforms), open an issue before writing the code.

## Commit messages

Write clear, imperative-mood messages (`Add ...`, `Fix ...`, not `Added`/`Fixes`).
A short subject line plus a body explaining the reasoning is ideal.

## Reporting security issues

Please **do not** open a public issue for security vulnerabilities. Email
**samarth@nanokernel.net** with details and, if possible, a reproduction. You'll
get an acknowledgement, and we'll coordinate a fix and disclosure.

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](LICENSE).
