#!/bin/bash
# task-var を再起動する。見た目を env var で詰めるとき用。
#
#   scripts/restart.sh                       # 既定値で起動
#   TASKVAR_BAR_H=88 scripts/restart.sh      # バー全体を 88px に(既定 64px)
#   TASKVAR_ICON_D=56 scripts/restart.sh     # アイコンの白円を 56px に
#   TASKVAR_BAR_H=120 TASKVAR_GAP=32 scripts/restart.sh  # 高いバー + 広い間隔
#   TASKVAR_TITLE_PX=20 scripts/restart.sh   # 曲名を 20px に(既定 26px)
#   TASKVAR_TITLE_PX=20 TASKVAR_ARTIST_PX=14 scripts/restart.sh
#   TASKVAR_CTRL_GAP=12 scripts/restart.sh   # ボタンと進捗バーの間を 12px に(既定 5px)
#   TASKVAR_BTN_D_PLAY=35 scripts/restart.sh # 再生/停止ボタンを 35px に(既定 30px)
#   TASKVAR_CLAWD_PX=14 scripts/restart.sh   # clawd 枠の見出しを 20px 以外に
#
# バー高さ(TASKVAR_BAR_H)は 24px 〜 画面の半分。端末のセル境界へスナップするので
# 指定より少し高くなることがある。アイコン(TASKVAR_ICON_D)は無指定だとバー高さの
# 73% になるので、高さだけ変えても大きさは付いてくる。曲名・アーティスト名は逆に
# 決め打ち(既定 26px / 16px)で、バーの高さや行の高さには連動しない。
#   TASKVAR_CLAWD_ROWS=3 scripts/restart.sh  # 3 行にする(行高が上がり見出しも大きくなる)
#   TASKVAR_CLAWD_COLS=1 scripts/restart.sh  # 1 列にする(既定は 2 行 2 列の格子)
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
