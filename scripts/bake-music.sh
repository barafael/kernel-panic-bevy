#!/usr/bin/env sh
# Bring the original Kernel Panic soundtrack into the game.
#
# The KP 4.4 Windows installer ("Kernel_Panic_4.4_Including_Spring_85.0_
# Installer28_136mb.exe", extract with `7z x`) ships 72 Vorbis tracks in
# `Music/KP/` plus `__Music_Credits__.txt`; the upstream music widget
# (`LuaUI/Widgets/music.lua`) played them in random order during a match.
#
# This copies the tracks byte-for-byte into `kernel-panic/assets/music/`
# (tracked by Git LFS, see .gitattributes), renaming each
# `<Artist> - <Title>.ogg` to `<artist-slug>--<title-slug>.ogg` because
# Bevy's web asset reader does not percent-encode paths and the original
# names carry `& ' ; ! , ( ) [ ]`. The display names, durations and
# sizes go to `catalog.json`, and the credits (Latin-1 upstream) to
# `credits.txt`; both feed the web player, `kernel-panic/web/music.html`.
#
# Usage: scripts/bake-music.sh <dir with the installer's Music/KP files>
set -eu
[ $# -eq 1 ] || { echo "usage: $0 <source-dir>" >&2; exit 2; }
src=$1
root=$(cd "$(dirname "$0")/.." && pwd)
dst="$root/kernel-panic/assets/music"
mkdir -p "$dst"
rm -f "$dst"/*.ogg "$dst/catalog.json" "$dst/credits.txt"

slug() {
    printf '%s' "$1" | tr 'A-Z' 'a-z' | sed -E 's/[^a-z0-9]+/-/g; s/^-//; s/-$//'
}
json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

tmp=$(mktemp)
n=0
for f in "$src"/*.ogg; do
    base=$(basename "$f" .ogg)
    case "$base" in
        *" - "*) artist=${base%% - *}; title=${base#* - } ;;
        *) artist="Unknown"; title=$base ;;
    esac
    file="$(slug "$artist")--$(slug "$title").ogg"
    cp "$f" "$dst/$file"
    seconds=$(ffprobe -v error -show_entries format=duration -of default=nw=1:nk=1 "$f" | cut -d. -f1)
    bytes=$(stat -c %s "$dst/$file")
    printf '%s\t%s\t{"artist":"%s","title":"%s","file":"%s","seconds":%s,"bytes":%s}\n' \
        "$artist" "$title" "$(json_escape "$artist")" "$(json_escape "$title")" "$file" "$seconds" "$bytes" >> "$tmp"
    n=$((n + 1))
done

{
    printf '{\n  "source": "Kernel Panic 4.4 installer, Music/KP (unmodified)",\n  "credits": "credits.txt",\n  "tracks": [\n'
    sort -t "$(printf '\t')" -k1,1 -k2,2 "$tmp" | cut -f3 | sed '$!s/$/,/; s/^/    /'
    printf '  ]\n}\n'
} > "$dst/catalog.json"
rm -f "$tmp"

iconv -f latin1 -t utf-8 "$src/__Music_Credits__.txt" | tr -d '\r' > "$dst/credits.txt"

echo "$n tracks -> $dst"
du -sh "$dst" | cut -f1
