#!/bin/bash
# task-var を再起動する。見た目を env var で詰めるとき用。
#
#   scripts/restart.sh                       # 既定値で起動
#   TASKVAR_TITLE_PX=24 scripts/restart.sh   # 曲名を 24px にして起動
#   TASKVAR_TITLE_PX=24 TASKVAR_ARTIST_PX=18 scripts/restart.sh
#
# 環境変数はそのまま引き継ぐので、~/bin/tmux-autostart を書き換えずに試せる。
# 気に入った値が決まったら、その行の nohup の前に付けて永続させる。
set -u

BIN="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/release/task-var"
LOG=/tmp/tmux-autostart

if [ ! -x "$BIN" ]; then
    echo "ビルドされていません: $BIN" >&2
    echo "  cargo build --release" >&2
    exit 1
fi

mkdir -p "$LOG"
pkill -x task-var
sleep 0.3
nohup "$BIN" >"$LOG/task-var.log" 2>&1 &
sleep 0.7

pid=$(pgrep -x task-var || true)
if [ -z "$pid" ]; then
    echo "起動に失敗しました。ログ:" >&2
    tail -5 "$LOG/task-var.log" >&2
    exit 1
fi
echo "task-var 起動 (pid $pid) — ログ: $LOG/task-var.log"
env | grep '^TASKVAR_' | sed 's/^/  /' || true
