# Security Policy

## Supported Versions

Only the latest released version of TorrentTUI is supported. Security fixes ship as patch releases on the most recent `v0.x` line.

## Reporting a Vulnerability

Please report security issues privately via GitHub's [Private Vulnerability Reporting](https://github.com/thijsvos/torrentTUI/security/advisories/new).

**Do not** open a public issue for security problems.

You can expect an acknowledgement within 7 days. Once a fix is available, a coordinated disclosure date will be agreed with the reporter.

## In Scope

- Crashes triggered by malicious torrent metadata, peer messages, or `.torrent` files
- Path traversal or arbitrary file access via torrent contents, magnet links, or config inputs
- Sensitive data exposure (peer IPs, info hashes, user paths) through logs, notifications, or error messages
- Cryptographic / TLS misconfiguration
- Resource-exhaustion / denial-of-service via crafted inputs
- Injection vectors in desktop notifications (e.g. Pango markup on Linux libnotify)
- Misuse of the embedded HTTP streaming API (`network.http_api_bind`) — e.g. unintended exposure when bound to a non-loopback interface, or any route returning content the user did not consent to share

## Out of Scope

- Vulnerabilities in `librqbit`, `rustls`, or other third-party crates — please report those upstream. We watch RUSTSEC and patch transitive dependencies in regular releases.
- Issues that require a malicious local user already on your machine.
- BitTorrent protocol-level concerns inherent to the protocol itself (e.g. swarm tracking by trackers).

## Hardening

If you are running TorrentTUI in a privacy-sensitive environment, see the **Privacy** section in the [README](./README.md) for notes on log filtering, UPnP, notifications, and the HTTP streaming API bind address.

The embedded HTTP API used for media streaming is bound to `127.0.0.1` by default, mounted read-only, and protected by **HTTP basic auth with a random per-session password**. Read-only alone is not sufficient — librqbit registers `POST /rust_log` and `POST /torrents/resolve_magnet` outside its `read_only` gate, and the former accepts a `text/plain` body, making it reachable cross-origin from a browser without a preflight; the credentials close that. If you change `network.http_api_bind` to expose it beyond loopback, do so only on a trusted network: basic auth over plaintext HTTP is readable on the wire. Note also that the stream URL carries the session credentials in the media player's argv, so any local user can read them with `ps` while the player runs — consistent with the local-attacker exclusion below.
