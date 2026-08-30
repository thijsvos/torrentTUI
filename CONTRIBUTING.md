# Contributing to TorrentTUI

Thanks for your interest in contributing! This document is a quick reference; for security issues please see [SECURITY.md](./SECURITY.md) instead.

## Development setup

```bash
git clone https://github.com/thijsvos/torrentTUI.git
cd torrentTUI
cargo build
cargo test --all
```

The minimum supported toolchain is whatever `rustc 1.95+` installs through `rustup`. CI uses `dtolnay/rust-toolchain@stable`.

## Before opening a PR

The project mirrors GitHub Actions' lint settings; running these locally avoids a CI round trip:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo clippy --all-targets --all-features
RUSTFLAGS="-D warnings" cargo test --all
cargo audit
```

`RUSTFLAGS` matters: CI sets it globally, including for the test job, so a
warning in test code passes locally without it and fails in CI. `cargo audit`
is a third CI job that a dependency bump can trip; if an advisory ever needs
to be ignored deliberately, record it in `.cargo/audit.toml` with the
rationale and the condition for removing it.

If you touch the engine, smoke-test against a public-domain torrent (e.g. one of [archive.org's](https://archive.org/) `.torrent` files) before submitting.

## Pull-request guidelines

- Keep PRs scoped: one logical change per PR is much easier to review than a sweeping refactor.
- Use the PR template: Summary (bullets) + Test plan (checklist).
- Reference an issue in the description (`Closes #N`) when one exists.
- For UI changes, include a short note about how you verified the change in a real terminal — TUI regressions don't always show up in unit tests.
- Prefer small, descriptive commit messages. Conventional Commits (`feat:`, `fix:`, `chore:`, `docs:`) are encouraged but not required; the auto-generated release changelog is cleaner when commits are structured.

## Areas that especially welcome help

- Cross-platform polish: Windows-specific terminal quirks, macOS notification-permission handling.
- Test coverage on `engine/torrent.rs` (the `run_engine` command loop in particular).
- Performance profiling on libraries with thousands of peers.
- Translations / accessibility (currently English-only, no high-contrast mode).

## Release process (maintainer)

1. Bump `version` in `Cargo.toml`.
2. Merge to `main`. The `release.yml` workflow auto-tags `vX.Y.Z` and builds the multi-arch matrix.
3. The release page is created with auto-generated changelog from the commit log.

## Code of conduct

Be excellent to each other. Disrespectful behaviour in issues, PRs, or discussions will get shut down.
