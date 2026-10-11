The original Kernel Panic soundtrack: 72 Vorbis tracks from the KP 4.4
Windows installer (Music/KP), copied unmodified by scripts/bake-music.sh
and renamed to <artist-slug>--<title-slug>.ogg (the display names are in
catalog.json). Credits in credits.txt; no licence statement came with it.

The .ogg files live in Git LFS: run `git lfs pull` after cloning, or they
are 130-byte pointers the game cannot decode. During a match the game
plays them in random order with a two-second gap, like the upstream
music.lua widget; the folder may be empty (the log then says "No music
found!"). catalog.json and credits.txt also feed the web player,
kernel-panic/web/music.html.
