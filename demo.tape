# Demo recording for the README. Rebuild the binary first, then run
# `vhs demo.tape` from the repo root. Records against a throwaway HOME in
# /tmp so the session in the gif is always fresh and no personal torrents
# appear.

Output assets/demo.gif

Set Shell zsh
Set FontSize 16
Set Width 1200
Set Height 700
Set Theme "Catppuccin Mocha"

Hide
Type "rm -rf /tmp/torrenttui-demo && mkdir -p /tmp/torrenttui-demo && alias torrenttui='HOME=/tmp/torrenttui-demo ./target/release/torrenttui' && clear"
Enter
Show

Type@120ms "torrenttui"
Sleep 500ms
Enter
Sleep 2.5s

# Command palette: press ':', fuzzy-search every action with its keybinding
Type ":"
Sleep 1.2s
Type@110ms "search"
Sleep 900ms
Enter
Sleep 1s

# The palette ran "Search torrent indexers" — now type the query.
# Built-in search: no Prowlarr/Jackett, no accounts.
Type@110ms "arch linux iso"
Sleep 700ms
Enter
Sleep 5s

# Browse the results, grab the top hit
Type "j"
Sleep 600ms
Type "k"
Sleep 700ms
Enter
Sleep 2.5s

# Back to the list — the download is starting
Type "q"
Sleep 6s

# Bandwidth limits: cap download at 500 KB/s, upload at 100 KB/s. The
# limiter is librqbit's token bucket, so the speed settles smoothly at the
# cap — no pause/unpause flicker — and the change applies instantly.
Type "t"
Sleep 1.2s
Type@140ms "500"
Sleep 600ms
Enter
Sleep 800ms
Type@140ms "100"
Sleep 600ms
Enter
Sleep 7s

# Reveal the torrent's data in the file manager
Type "o"
Sleep 2.5s

# Quit (a download is active, so confirm). Keep the tail tight so the loop
# lands back on the launch without lingering on the shell prompt.
Type "q"
Sleep 1s
Type "y"
Sleep 200ms

Hide
Type "rm -rf /tmp/torrenttui-demo"
Enter
