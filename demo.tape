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

# Built-in search: no Prowlarr/Jackett, no accounts — just press s
Type "s"
Sleep 800ms
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
Sleep 6.5s

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
