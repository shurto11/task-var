#!/usr/bin/env bash
# Melon のロゴ(公式アプリアイコン。白地に緑の o)を melon.com から assets/melon.png へ
# ダウンロードする。task-var は起動時にこれを読んで Melon 枠の札に置く。
#
# ロゴは Kakao Entertainment の商標なので、リポジトリには含めない(.gitignore 済み)。
# 無いまま起動すると札は白地だけになる。
set -euo pipefail

out="$(cd "$(dirname "$0")/.." && pwd)/assets/melon.png"
url="https://cdnimg.melon.co.kr/resource/mobile40/cds/common/image/mobile_apple_180x180.png"

echo "fetch $url -> $out"
curl -fsSL "$url" -o "$out"
