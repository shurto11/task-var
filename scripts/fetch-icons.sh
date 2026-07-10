#!/usr/bin/env bash
# Simple Icons から task-var 用のアイコンSVGを assets/ へダウンロードする。
# cdn.simpleicons.org はブランドカラーの fill 付きSVGを返すので、
# 実行時は resvg でそのままレンダリングすればカラーになる。
set -euo pipefail

dir="$(cd "$(dirname "$0")/.." && pwd)/assets"
mkdir -p "$dir"

# 表示名:slug (ssbrowse はブランドが無いので googlechrome を代替に使う)
# eduroam は Simple Icons に無いため assets/eduroam.svg を手書きで同梱している(上書きしない)
icons=(
    "tmux:tmux"
    "spotify:spotify"
    "shorts:youtubeshorts"
    "bluetooth:bluetooth"
    "ssbrowse:googlechrome"
    "calendar:googlecalendar"
)

for entry in "${icons[@]}"; do
    name="${entry%%:*}"
    slug="${entry##*:}"
    out="$dir/$name.svg"
    echo "fetch $slug -> $out"
    curl -fsSL "https://cdn.simpleicons.org/$slug" -o "$out"
done

echo "done: $(ls "$dir" | wc -l) files in $dir"
