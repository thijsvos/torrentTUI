#!/usr/bin/env python3
"""End-to-end check for session ownership: the lock, the control channel and
the background session.

These paths are why `torrenttui` cannot corrupt its own state, and almost none
of them are reachable from a unit test — they need two real processes racing
for one lock. They are also the paths most likely to break *per platform*:
`flock` is advisory on unix and `LockFileEx` is mandatory on Windows, and the
detach hand-off depends on which of two processes owns a file.

Deliberately no pty, so this runs unchanged on Linux, macOS and Windows in CI.
The one thing it cannot cover is the TUI itself — pressing Ctrl+D and closing a
terminal window is still a manual step, documented in CONTRIBUTING.md.

Usage:  python3 scripts/e2e_session.py path/to/torrenttui
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time

MAGNET = "magnet:?xt=urn:btih:" + "a" * 40 + "&dn=E2E"
failures = []


def check(name, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail else ""))
    if not ok:
        failures.append(name)


def main():
    if len(sys.argv) < 2:
        sys.exit("usage: e2e_session.py <path-to-torrenttui>")
    binary = os.path.abspath(sys.argv[1])
    home = tempfile.mkdtemp(prefix="torrenttui-e2e-")
    downloads = os.path.join(home, "dl")
    os.makedirs(downloads, exist_ok=True)

    # An isolated HOME/APPDATA keeps this off the developer's real session.
    env = dict(os.environ, HOME=home, APPDATA=home, XDG_CONFIG_HOME=home)

    def run(*args, timeout=60):
        p = subprocess.run(
            [binary, *args], env=env, capture_output=True, text=True, timeout=timeout
        )
        return p.returncode, (p.stdout + p.stderr).strip()

    daemon = None
    try:
        rc, out = run("--status")
        check("status on a fresh machine reports nothing", rc == 0 and "No TorrentTUI" in out, out)
        # The #44 rule: a read-only query must not create a config directory or
        # write a log line on a machine that has never run the app.
        check("status created nothing", os.listdir(home) == ["dl"], str(os.listdir(home)))

        rc, out = run("--stop")
        check("stop with nothing running is a no-op, not an error", rc == 0, f"rc={rc} {out}")

        daemon = subprocess.Popen(
            [binary, "--headless", "-d", downloads],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        # Give it time to bind and take the lock.
        deadline = time.time() + 30
        while time.time() < deadline:
            rc, out = run("--status")
            if "background session" in out:
                break
            time.sleep(0.5)
        check("headless session starts and is reported", "background session" in out, out)

        rc, out = run("--headless")
        check("a second headless refuses", rc != 0 and "already running" in out, f"rc={rc} {out}")

        # The bug this whole subsystem exists to prevent: two processes sharing
        # one session silently lose torrents.
        rc, out = run("--status")
        first_pid = daemon.pid
        check("status names the owning pid", str(first_pid) in out, out)

        rc, out = run(MAGNET)
        check("a magnet is handed to the running session", rc == 0 and "Added" in out, f"rc={rc} {out}")

        rc, out = run("does-not-exist.torrent")
        check(
            "a .torrent path is refused rather than resolved in the daemon's cwd",
            rc != 0 and "magnet" in out.lower(),
            f"rc={rc} {out}",
        )

        # Must terminate even though the engine is busy resolving a magnet that
        # will never resolve.
        start = time.time()
        rc, out = run("--stop", timeout=60)
        elapsed = time.time() - start
        check("stop works while an unresolvable magnet is being added", rc == 0, f"rc={rc} {out}")
        check("stop is prompt, not a timeout", elapsed < 15, f"{elapsed:.1f}s")

        daemon.wait(timeout=30)
        rc, out = run("--status")
        check("nothing is running afterwards", "No TorrentTUI" in out, out)

        # A crash must not leave a lock that needs manual clearing: the kernel
        # releases it, so the next launch just works.
        daemon = subprocess.Popen(
            [binary, "--headless", "-d", downloads],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        deadline = time.time() + 30
        while time.time() < deadline:
            if "background session" in run("--status")[1]:
                break
            time.sleep(0.5)
        daemon.kill()
        daemon.wait(timeout=30)
        rc, out = run("--status")
        check("a killed session leaves no stale lock", "No TorrentTUI" in out, out)
        daemon = None
    finally:
        if daemon is not None and daemon.poll() is None:
            daemon.kill()
        shutil.rmtree(home, ignore_errors=True)

    print()
    if failures:
        print(f"{len(failures)} check(s) failed: {', '.join(failures)}")
        sys.exit(1)
    print("all session checks passed")


if __name__ == "__main__":
    main()
