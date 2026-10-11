#!/usr/bin/env sh
# Re-encode the original Kernel Panic sound effects for the game.
#
# Upstream ships 26 effects as 16-bit mono PCM `.wav` (1.3 MB). The game
# loads Vorbis (Bevy's default audio decoder, ~240 KB at q4) from
# `kernel-panic/assets/sounds/<lowercase name>.ogg`; names are lower-cased
# because Spring's VFS is case-insensitive and Bevy's asset paths are not.
# The file list the game expects is `SOUND_FILES` in
# `kernel-panic/src/audio/mod.rs`.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
src="$root/upstream/Kernel-Panic/sounds"
dst="$root/kernel-panic/assets/sounds"
mkdir -p "$dst"
for wav in "$src"/*.wav; do
    name=$(basename "$wav" .wav | tr 'A-Z' 'a-z')
    ffmpeg -loglevel error -y -i "$wav" -c:a libvorbis -q:a 4 "$dst/$name.ogg"
done
ls "$dst"/*.ogg | wc -l
