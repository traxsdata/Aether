#!/bin/sh
# build-craft.sh - fetch, build and package Craft apps for æther.
#
#   tools/build-craft.sh gridcraft photocraft
#
# Each app is cloned from github.com/storytold/<name> into $CRAFT_SRC
# (default build/craft-src; an existing checkout is used as it is), built
# unmodified against the eframe stand-in by tools/craft-build.sh, stripped,
# and packaged by tools/mkapp.py as build/apps/<name>.app (untrusted, 1 GiB
# memory limit). Only apps with a shell in tools/craft-shells/ build so far.
set -eu
cd "$(dirname "$0")/.."
SRC=${CRAFT_SRC:-build/craft-src}
mkdir -p "$SRC" build/apps

color() {
    case $1 in
    photocraft) echo 3B82F6 ;; gridcraft) echo 22A06B ;; vectorcraft) echo F97316 ;;
    filmcraft) echo E11D48 ;; lightcraft) echo EAB308 ;; pdfcraft) echo DC2626 ;;
    effectcraft) echo 8B5CF6 ;; designcraft) echo EC4899 ;; figcraft) echo A855F7 ;;
    wordcraft) echo 2563EB ;; soundcraft) echo 14B8A6 ;; deckcraft) echo F59E0B ;;
    cadcraft) echo 64748B ;; *) echo 38E1F5 ;;
    esac
}

for name in "$@"; do
    repo="$SRC/$name"
    [ -d "$repo/.git" ] || git clone --depth 1 "https://github.com/storytold/$name" "$repo"
    rev=$(git -C "$repo" rev-parse --short HEAD)
    version=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$repo/Cargo.toml" | head -1)
    title=$(echo "$name" | sed 's/craft$//; s/^./\U&/')Craft
    elf="build/craft-out/$name.elf"
    mkdir -p build/craft-out
    tools/craft-build.sh "$repo" "$name" "$elf"
    strip -o "build/craft-out/$name-stripped.elf" "$elf"
    icon="assets/icons/craft/$name.png"
    if [ -f "$icon" ]; then
        tools/mkapp.py --name "$name" --title "$title" --version "${version:-0}" \
            --source "github.com/storytold/$name@$rev" --quota 1024 --color "$(color "$name")" \
            --icon "$icon" \
            "build/craft-out/$name-stripped.elf" "build/apps/$name.app"
    else
        tools/mkapp.py --name "$name" --title "$title" --version "${version:-0}" \
            --source "github.com/storytold/$name@$rev" --quota 1024 --color "$(color "$name")" \
            "build/craft-out/$name-stripped.elf" "build/apps/$name.app"
    fi
done
