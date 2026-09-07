# task-var

フレームバッファの画面下部に常駐するタスクバー。tmux セッションの切り替え、
Spotify の再生情報・操作、動いている Claude Code の一覧を 1 本の帯にまとめる。

ubuntu server + fbterm + tmux の環境で、タッチパネル付きの画面をそのまま
「スマホのホームバー」のように使うために作った。X もウィンドウマネージャも
使わず、`/dev/fb0` へ直接描く。

![task-var のタスクバー](docs/screenshot.png)

実際の `/dev/fb0` から切り出した画面。左から clawd 行・アイコン列・再生情報パネル。

```
┌──────────────────────────────────────────────────────────────────────┐
│ [clawd × 4]        ● ● ● ● ● ●        [ アート | 曲名  | ⇄ ⏮ ⏸ ⏭ ⟳ ] │
│  動いている        セッション          アーティスト  ──────────────   │
│  Claude Code       切り替え                    Spotify 再生情報       │
└──────────────────────────────────────────────────────────────────────┘
```

## できること

### アイコン列(中央)

tmux / YouTube Shorts / Bluetooth / ssbrowse / eduroam / Google カレンダーの
6 個。タップすると対応する tmux セッションへ遷移し、無ければ作ってから
プログラムを起動する。

白円の下の横線がセッションの状態を表す。

| 横線 | 意味 |
|------|------|
| 水色・長い(直径の 1/2) | そのセッションを表示中 |
| 灰色・短い(直径の 1/4) | 開いているが表示していない |
| なし | セッションが無い |

色だけでなく長さでも差が付いているので、並んでいても一目で見分けられる。
tmux アイコンだけは閉じることが無いので、常に水色か灰色のどちらかになる。

### 再生情報パネル(右)

spotatui が `/tmp/spotatui_np.json` に書く情報を 2 行 3 列で表示する。

- 列①: アルバムアート
- 列②: 曲名 / アーティスト名
- 列③: シャッフル・前・再生停止・次・リピートの 5 ボタン / 進捗バー

操作は MPRIS 越しに送る。パネルの背景はアルバムアートから採った色から
暗色へ落ちる斜めのグラデーション(明度は固定するので白文字は常に読める)。
ボタン以外の場所をタップすると spotatui のセッションへ遷移する。

### Claude セッション枠(左)

動いている Claude Code をキャラクターで最大 4 個、2 行 2 列に並べる。
右にはそのペインで何をしているかを出す(Claude Code が端末タイトルへ書く
作業の要約。取れなければカレントディレクトリ名)。

| 色 | 状態 |
|----|------|
| オレンジ | 処理中(その場で跳ねる) |
| 青 | 質問・許可待ち |
| 黄 | 処理終了 |
| 灰 | 終了をタッチで確認済み |

タップするとその claude が動いているウィンドウ・ペインへ遷移する。

## 動作要件

- Rust (edition 2021)
- fbterm + tmux(`/dev/fb0` へ書けること)
- [touch-server](../touch-server) — タッチ入力の配信元。クライアントとして登録する
- fb-server — 全画面クライアントとの描画領域の調停に使う(任意)
- spotatui — 再生情報パネルの表示データと MPRIS の出どころ(任意)

## ビルドと起動

```bash
./scripts/fetch-icons.sh     # Simple Icons から assets/*.svg を取得(初回のみ)
cargo build --release
./target/release/task-var    # 引数なしでデーモン
```

起動すると `stty rows` で端末の行数をバーの高さぶん縮めて下部を専有し、
終了時(Ctrl-C や pkill でも)元の行数へ戻す。

見た目を試すときは `scripts/restart.sh` を使う。既存プロセスを落として
環境変数を引き継いだまま起動し直す。

```bash
TASKVAR_BAR_H=88 scripts/restart.sh
```

## サブコマンド

| コマンド | 用途 |
|----------|------|
| `task-var` | デーモン(バーを描く本体) |
| `task-var hook-start\|question\|answer\|stop\|end` | Claude Code の hooks から状態を送る |
| `task-var status` | 今の clawd 行を表示する |

`hook-*` と `status` は `$XDG_RUNTIME_DIR/task-var.sock` へ 1 行送るだけの
軽量クライアント。デーモンが居なければ黙って諦める(hook がデーモンを
起こすと画面が動いてしまうため、起動はしない)。

## Claude Code の hooks 設定

`~/.claude/settings.json` に以下を設定すると、左枠にキャラクターが並ぶ。
`$TMUX_PANE` を添えて送るので、どのペインで動いているかまで分かる。

| hook | コマンド | 動作 |
|------|---------|------|
| `UserPromptSubmit` | `task-var hook-start` | 処理開始 = オレンジ |
| `Notification` / `PreToolUse: AskUserQuestion\|ExitPlanMode` | `task-var hook-question` | 質問・許可待ち = 青 |
| `PostToolUse` (matcher `*`) | `task-var hook-answer` | 回答後に処理再開 = オレンジ |
| `Stop` | `task-var hook-stop` | 終了 = 黄 |
| `SessionEnd` | `task-var hook-end` | 行を消す |

## 環境変数

すべて任意。無指定なら既定値で動く。

### 全体

| 変数 | 既定 | 内容 |
|------|------|------|
| `TASKVAR_BAR_H` | 64px | バーの高さ。24px 〜 画面の半分。端末のセル境界へスナップする |
| `TASKVAR_MARGIN` | 12px | バー左右端からの余白 |
| `TASKVAR_SHADOW_A` | 0.05 | 影の濃さ。0 で影なし、上限 1 |
| `TASKVAR_SHADOW_BLUR` | 2px | 影のぼかし幅。0.5 〜 64px |
| `TASKVAR_FONT` | 自動 | 文字に使う TTF/OTF のパス |
| `TASKVAR_TTY` | 自動 | 行数を縮める対象の tty |
| `TASKVAR_SHRINK` | — | `0` で端末の縮小を無効化する |

影の濃さは白円・左右の枠・パネルと clawd のキャラで共通。キャラの影は
円の 0.6 倍の濃さになるよう追従する。

### アイコン列

| 変数 | 既定 | 内容 |
|------|------|------|
| `TASKVAR_ICON_D` | バー高の 73% | 白円の直径。下限 16px |
| `TASKVAR_GAP` | 24px | アイコン同士の間隔 |

大きくしすぎても、縦は「白円 + 余白 + 下線」がバーに収まるところまで、
横は左右の枠に 200px ずつ残るところまでで頭打ちになる。

### 再生情報パネル

| 変数 | 既定 | 内容 |
|------|------|------|
| `TASKVAR_NP_W` | 400px | パネルの幅 |
| `TASKVAR_TEXT_W` | — | 曲名・アーティストの列幅。指定するとパネル幅を逆算する |
| `TASKVAR_TITLE_PX` | 26px | 曲名の文字サイズ(6〜200px) |
| `TASKVAR_ARTIST_PX` | 16px | アーティスト名の文字サイズ |
| `TASKVAR_PROG_H` | 4px | 進捗バーの高さ |
| `TASKVAR_CTRL_GAP` | 5px | ボタン列と進捗バーの間隔 |
| `TASKVAR_CTRL_DY` | 0 | ボタン + 進捗バーの上下位置の微調整(負値可) |
| `TASKVAR_BTN_D` | — | ボタン 5 個共通の直径 |
| `TASKVAR_BTN_D_SHUFFLE` ほか | 20/16/30/16/20px | ボタンごとの直径(`_PREV` / `_PLAY` / `_NEXT` / `_REPEAT`) |

文字サイズはバーの高さに連動しない決め打ち。列に収まらないときは指定の
75% まで縮め、それでも入らなければ末尾を `…` で詰める。

### Claude セッション枠

| 変数 | 既定 | 内容 |
|------|------|------|
| `TASKVAR_CLAWD_ROWS` | 2 | 行数(1〜8) |
| `TASKVAR_CLAWD_COLS` | 2 | 列数(1〜4) |
| `TASKVAR_CLAWD_W` | パネル幅と同じ | 枠の幅 |
| `TASKVAR_CLAWD_PX` | 20px | 見出しの文字サイズ(下限 6px) |
| `TASKVAR_CLAWD_IMG` | 埋め込み | キャラクター画像(3 色パレットの PNG) |

## 開発

```bash
cargo test        # 描画・レイアウト・当たり判定まで含めて 34 本
cargo build --release && scripts/restart.sh
```

テストは実際にバッファへ描いてピクセルを読む。バーの高さを 24px から
384px まで舐めてレイアウトが破綻しないことも確認している
(`layout_survives_any_bar_height`)。

`TASKVAR_TEST_ART=<画像パス> cargo test` でアルバムアートを流し込むと、
アートから採る背景色を実データで確かめられる。`TASKVAR_TEST_DUMP=<パス>` を
付ければ描画結果を PPM で書き出せる。

## 制限

- fbterm の再描画で消されることがあるが、1 秒ポーリングで描き直すので
  1 秒以内に復活する
- touch-key と併用すると、両者とも下部の端末行を縮めるので起動順によって
  行数がずれる
- 全画面クライアント(fbhalf / ssbrowse)の間はバーを隠し、画面下端の
  上スワイプで 3 秒だけ出す(端末の縮小も解除して全高を明け渡す)
