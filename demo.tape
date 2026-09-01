# Demo recording for the README. Build the release binary first, then run
# `vhs demo.tape` from the repo root. Records against a throwaway HOME in
# /tmp so the session in the gif is always fresh and no personal torrents
# appear.
#
# Two things worth knowing before editing this.
#
# The session starts from a real `.torrent` (the Debian netinst image)
# rather than from a magnet picked out of the search results.
# A magnet has no metadata until librqbit finds a peer holding it, which can
# take minutes or never happen at all — and until it does there is no row to
# show. Recordings built on that produced an empty list and a detach that
# correctly refused because the session had nothing in it. A `.torrent`
# carries its metadata inline, so the download is on screen immediately and
# the recording is the same every time. The search is still demonstrated;
# it just isn't load-bearing for what follows.
#
# Debian netinst specifically: it is small (~700 MB, so a recording cannot
# pull much), heavily seeded, and served by real BitTorrent peers. An
# archive.org torrent will sit at 0 B/s forever — that host serves over HTTP
# webseeds, which librqbit does not implement (BEP 19).
#
# The rate cap is applied early on purpose, for two reasons. Uncapped, this
# saturates the link, and a recording made on a metered connection should not
# spend a gigabyte of someone's data to show a progress bar moving. And the
# cap needs time to become visible: the limiter stops requesting immediately,
# but pieces already in flight from a thousand peers keep arriving for
# fifteen seconds or so. Applying it before the search means the drain
# happens while the search is on screen.
#
# The tail detaches and reattaches, which starts a real background process.
# The cleanup at the bottom stops it — do not remove that, or a recording
# leaves a daemon running against the throwaway HOME.

Output assets/demo.gif

Set Shell zsh
Set FontSize 16
Set Width 1200
Set Height 700
Set Theme "Catppuccin Mocha"

Hide
Type "rm -rf /tmp/torrenttui-demo && mkdir -p /tmp/torrenttui-demo && curl -sL -o /tmp/torrenttui-demo/demo.torrent https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/debian-13.6.0-amd64-netinst.iso.torrent && alias torrenttui='HOME=/tmp/torrenttui-demo ./target/release/torrenttui' && clear"
Enter
Sleep 3s
Show

Type@120ms "torrenttui /tmp/torrenttui-demo/demo.torrent"
Sleep 500ms
Enter
Sleep 6s

# Bandwidth limits: cap the download at 500 KB/s. librqbit's token bucket
# makes the speed settle smoothly at the cap — no pause/unpause flicker.
Type "t"
Sleep 1.2s
Type@140ms "500"
Sleep 700ms
Enter
Sleep 700ms
Enter
Sleep 3s

# Command palette: press ':', fuzzy-search every action with its keybinding
Type ":"
Sleep 1.2s
Type@110ms "search"
Sleep 900ms
Enter
Sleep 1s

# Built-in search: queries public indexer APIs directly — no Prowlarr,
# no Jackett, no accounts, no API keys.
Type@110ms "ubuntu iso"
Sleep 700ms
Enter
Sleep 5s

# Browse the results, then back to the download list.
Type "j"
Sleep 700ms
Type "j"
Sleep 900ms
Escape

# Back on the list, and this is the part worth waiting for: the rate has
# settled at the cap. Coming down from ~15 MB/s takes a good fifteen seconds
# — a thousand peers all have requests in flight, and those keep landing
# after the limiter stops asking for more. The search above is deliberately
# placed inside that window so the gif spends the drain doing something
# useful instead of watching a number fall.
Sleep 9s

# Detach. Ctrl+D always confirms first — regardless of confirm_on_quit —
# and says plainly that seeding continues and how to stop it. Nothing
# survives a plain `q`.
Ctrl+D
Sleep 3s
Type "y"
Sleep 4s

# The window is gone; the session is not.
Type@120ms "torrenttui --status"
Sleep 400ms
Enter
Sleep 3.5s

# Reattach: the background session hands the torrents straight back.
# Fastresume means nothing is re-checked.
Type@120ms "torrenttui"
Sleep 400ms
Enter
Sleep 7s

# Quit for real this time (a download is active, so confirm).
Type "q"
Sleep 1.2s
Type "y"
Sleep 300ms

Hide
Type "torrenttui --stop >/dev/null 2>&1; rm -rf /tmp/torrenttui-demo"
Enter
