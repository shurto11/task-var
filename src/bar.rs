//! タスクバーのレイアウト・描画・当たり判定。
//!
//! バーは画面下部の全幅の帯(BGRA バッファ)。左に clawd 枠、中央にセッション切替の
//! 白円アイコンを横一列(外枠リングは持たない)、右に Spotify の再生情報パネルを置く。
//! 白円の下の下線: 表示中セッション=水色 / 存在するが非表示=灰色 /
//! セッション無し=下線なし。灰色は水色の半分の長さにして、色だけでなく
//! 長さでも見分けが付くようにする。
//! tmux アイコンはセッションに対応せず閉じることが無いので、下線が消えることはない
//! (名前付きセッション以外を表示中=水色、名前付きセッションを見ている間=灰色)。
//!
//! 再生情報パネルは 2 行 3 列:
//!   列① アルバムアート(2 行ぶち抜き) / 列② 曲名・アーティスト /
//!   列③ 操作ボタン 5 個・進捗バー
//!
//! clawd 枠には動いている Claude Code をキャラクター + 作業の要約で最大 4 行ぶん
//! 縦に並べる(旧 touch-claude)。枠は左端から、アイコン列の手前までの残り全部。
//! アイコン列はその clawd 枠と再生情報パネルの間の中央に置く。

use crate::actions::{IconDef, ICONS};
use crate::art::Accent;
use crate::clawd::{Row, St};
use crate::icons::{self, Glyph};
use crate::mpris::{Ctrl, Loop, PlayerState};
use crate::np::NowPlaying;
use crate::sprite::{self, Sprite};
use crate::text::Font;
use crate::tmux::State;
use anyhow::Result;

// 色はすべて BGR 順(バッファが BGRA のため)。
/// バーの地色 #D6E0FA。
const BG: [u8; 3] = [0xFA, 0xE0, 0xD6];
const WHITE: [u8; 3] = [255, 255, 255];
/// 白円の上に乗せるグリフの色(再生/停止ボタン)。地色とは独立。
const BLACK: [u8; 3] = [0, 0, 0];
const BLUE: [u8; 3] = [255, 144, 30]; // #1E90FF 水色の下線
const GRAY: [u8; 3] = [128, 128, 128];

// アイコン下の下線。外枠リングの代わりに、開いているかどうかはこれが示す。
/// 下線の太さ(px)。
const MARK_H: u32 = 4;
/// グリフの下端と下線の間隔(px)。
const MARK_GAP: u32 = 4;
/// 水色(表示中)の下線の長さ。タイル幅に対する割合。
const MARK_PCT: u32 = 50;
/// 灰色(開いているだけ)の下線の長さ。水色の半分。
const MARK_OPEN_PCT: u32 = MARK_PCT / 2;
/// 円の外側どこまでをアイコンのタッチとして拾うか(px)。
const ICON_HIT_PAD: u32 = 8;
/// グリフの大きさ。白円の直径に対する割合。
const ICON_GLYPH_PCT: u32 = 58;
/// 白円の直径の既定。バー高さに対する割合(既定の 88px バーで 64px になる)。
/// `TASKVAR_BAR_H` だけ変えてもアイコンが一緒に育つように比で持つ。
const ICON_D_PCT: u32 = 73;
/// 白円の直径の下限。これ以下にはしない(グリフが潰れて見分けられなくなる)。
const ICON_D_MIN: u32 = 16;
/// アイコン列が広がっても、左右の枠にはこれだけの幅を残す。
/// アイコンを大きくしすぎたときに再生情報パネルが消えるのを防ぐ。
const NP_MIN_W: u32 = 200;

// 影。明るい地色の上でアイコンの白円と 2 つの枠が浮いて見えるように真下へ敷く。
/// 影の色(#1E2D5A 相当)。地色が青みなので黒ではなく暗い青にする。
const SHADOW: [u8; 3] = [0x5A, 0x2D, 0x1E];
/// 円(枠は縁)の真下での影の濃さの既定。ここからぼかし幅ぶんかけて 0 へ落とす。
/// `TASKVAR_SHADOW_A` で変えられる(→ `shadow_a()`)。
const SHADOW_A: f32 = 0.05;
/// 影のぼかし幅(px)の既定。円や枠の外側へこのぶん広がる。
/// `TASKVAR_SHADOW_BLUR` で変えられる(→ `shadow_blur()`)。
const SHADOW_BLUR: f32 = 2.0;
/// 影を下へずらす量(px)。光が上から当たっているように見せる。
const SHADOW_DY: f32 = 3.0;

// 枠の色。Spotify の #121212 ではなく、地色 #D6E0FA と同じ色相(222°)の暗色へ
// 落とす。無彩色の黒だと明るい地色の上で色が浮いて、板が穴のように見えるため。
// ブランド色そのもの(グリーン)は動かさない。
const PANEL: [u8; 3] = [0x33, 0x1E, 0x16]; // #161E33 ベース
const PANEL_EDGE: [u8; 3] = [0x5E, 0x40, 0x33]; // #33405E 枠線
const GREEN: [u8; 3] = [0x60, 0xD7, 0x1E]; // #1ED760 アクセント
const SUB_TEXT: [u8; 3] = [0xB3, 0xB3, 0xB3]; // #B3B3B3 アーティスト名
const GROOVE: [u8; 3] = [0x78, 0x55, 0x4A]; // #4A5578 進捗バーの溝
const ART_BG: [u8; 3] = [0x50, 0x30, 0x26]; // #263050 アート未取得時の下地
// 背景のグラデーション。左端の色はアルバムアートから採る(art::Accent)。
/// グラデーションが単色へ落ち着くまでの横幅(パネル幅に対する割合)。
/// 大きいほど境目が右へ寄る。ボタン列(左から約 60%)の手前で落とし切るのを
/// やめて、ボタンのあたりでようやく素の暗色へ着くところまで伸ばしてある。
const GRAD_SPREAD: f32 = 0.8;

/// 操作ボタンのグリフ(assets/ の手書き SVG)。
const G_SHUFFLE: usize = 0;
const G_PREV: usize = 1;
const G_PLAY: usize = 2;
const G_PAUSE: usize = 3;
const G_NEXT: usize = 4;
const G_REPEAT: usize = 5;
const G_REPEAT1: usize = 6;
const CTRL_SVGS: [&[u8]; 7] = [
    include_bytes!("../assets/shuffle.svg"),
    include_bytes!("../assets/prev.svg"),
    include_bytes!("../assets/play.svg"),
    include_bytes!("../assets/pause.svg"),
    include_bytes!("../assets/next.svg"),
    include_bytes!("../assets/repeat.svg"),
    include_bytes!("../assets/repeat-one.svg"),
];
/// 列③に並ぶボタンの順番。当たり判定のスロット順でもある。
pub const CTRLS: [Ctrl; 5] =
    [Ctrl::Shuffle, Ctrl::Prev, Ctrl::PlayPause, Ctrl::Next, Ctrl::Repeat];

/// ボタンごとの既定の大きさ。並びは `CTRLS` と揃える。
/// 再生/停止を大きく、曲送りを小さく、トグルはその中間にしてある。
const BTN_D_DEFAULT: [u32; 5] = [20, 16, 30, 16, 20];

/// ボタンごとの大きさを指定する env var。並びは `CTRLS` と揃える。
/// 未指定のものは `TASKVAR_BTN_D`(5 つ共通の上書き)、それも無ければ
/// `BTN_D_DEFAULT` へ落ちる。
const BTN_ENV: [&str; 5] = [
    "TASKVAR_BTN_D_SHUFFLE",
    "TASKVAR_BTN_D_PREV",
    "TASKVAR_BTN_D_PLAY",
    "TASKVAR_BTN_D_NEXT",
    "TASKVAR_BTN_D_REPEAT",
];

/// 曲名・アーティスト名の大きさ。行の高さやバーの高さからは決めず、
/// ここで決め打ちする(`TASKVAR_TITLE_PX` / `TASKVAR_ARTIST_PX` で変えられる)。
/// バーを高くしても文字はそのまま、大きくしたければ自分で指定する。
const TITLE_PX: f32 = 26.0;
const ARTIST_PX: f32 = 16.0;
/// 文字サイズの許容範囲(env で極端な値を渡されたときの歯止め)。
const TEXT_PX_RANGE: (f32, f32) = (6.0, 200.0);
/// 列に収まらないときに縮められる下限。指定サイズに対する割合で持つので、
/// 大きさを変えても縮み方は変わらない。ここまで縮めても入らなければ
/// 諦めて末尾を `…` で詰める。
const SHRINK_MIN_PCT: f32 = 0.75;

/// 再生/停止グリフは白円の中に入れるので、他より小さく描く。
const PLAY_GLYPH_PCT: u32 = 58;

// clawd 枠(動いている Claude Code の一覧)。
/// 縦に並べる行数。`TASKVAR_CLAWD_ROWS` で変えられる。
const CLAWD_ROWS: usize = 2;
/// 横に並べる列数。`TASKVAR_CLAWD_COLS` で変えられる。
/// 既定は 2 行 2 列で、最大 4 個を 2x2 に並べる。
const CLAWD_COLS: usize = 2;
const CLAWD_PAD_X: u32 = 8;
const CLAWD_PAD_Y: u32 = 4;
/// 行間(キャラの上下に食わせる余白)。
const CLAWD_ROW_GAP: u32 = 2;
/// 列間(隣のセルの見出しと詰まって見えないよう、キャラ間隔より広く取る)。
const CLAWD_COL_GAP: u32 = 12;
/// キャラとセッション名の間隔。
const CLAWD_NAME_GAP: u32 = 8;
/// 見出しの文字サイズ。`TASKVAR_CLAWD_PX` で変えられる。
/// 行の高さからは決めず、幅に入らなくても縮めない(セルごとに大きさが
/// 変わると 2x2 の格子がばらついて見えるため)。溢れた分は `…` で詰める。
const CLAWD_NAME_PX: f32 = 20.0;
/// これより狭い場所しか空いていなければ枠ごと出さない。
const CLAWD_MIN_W: u32 = 96;
/// 走りアニメーションの上下動(px)。行が低いので touch-claude の 5px より小さい。
const CLAWD_BOB: u32 = 2;
/// 走りアニメーションの 1 コマの長さ。
pub const CLAWD_BOB_MS: u128 = 300;
/// 見出しの色。枠を持たず地色の上に直接置くので黒。
const CLAWD_NAME: [u8; 3] = [0x1A, 0x1A, 0x14]; // #141A1A
/// 確認済み(灰)の行の見出し。黒より落として控えめにする。
const CLAWD_NAME_SEEN: [u8; 3] = [0x8C, 0x82, 0x78]; // #78828C
/// キャラの影の濃さ。円や枠(`shadow_a()`)に対する比で持つので、
/// `TASKVAR_SHADOW_A` を動かすとキャラの影も一緒に付いてくる。
/// 18px のキャラに同じ濃さを敷くと影の方が目立つため薄くする。
const CLAWD_SHADOW_RATIO: f32 = 0.6;
/// キャラの影のずらし量(px)。キャラが小さいので円より小さく。
const CLAWD_SHADOW_DX: u32 = 1;
const CLAWD_SHADOW_DY: u32 = 2;

// 状態ごとのキャラの色(BGR)。処理中は元画像のオレンジをそのまま使う。
const CLAWD_ASK: [u8; 3] = [217, 144, 74]; // #4A90D9
const CLAWD_DONE: [u8; 3] = [76, 201, 242]; // #F2C94C
const CLAWD_SEEN: [u8; 3] = [158, 158, 158]; // #9E9E9E

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// 指定があるときだけ Some。既定を持たない上書き用の env var に使う。
fn env_opt_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).filter(|v| *v > 0)
}

/// 上下のずらし量。負値も取れる。
fn env_i32(name: &str, default: i32) -> i32 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

/// 影の濃さ(`TASKVAR_SHADOW_A`)。描画のたびに env を引かないよう 1 度だけ読む。
fn shadow_a() -> f32 {
    static V: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *V.get_or_init(|| shadow_a_of(std::env::var("TASKVAR_SHADOW_A").ok().as_deref()))
}

/// 影のぼかし幅(`TASKVAR_SHADOW_BLUR`, px)。同じく 1 度だけ読む。
fn shadow_blur() -> f32 {
    static V: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *V.get_or_init(|| shadow_blur_of(std::env::var("TASKVAR_SHADOW_BLUR").ok().as_deref()))
}

/// 濃さの値決め。0(影を完全に消す)も有効なので `env_f32` は使えない。
/// 1 より上は不透明を超えるだけなので頭打ちにする。
fn shadow_a_of(v: Option<&str>) -> f32 {
    v.and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| *v >= 0.0)
        .unwrap_or(SHADOW_A)
        .min(1.0)
}

/// ぼかし幅の値決め。0 だと落とし込みの割り算が 0 除算になるので下限を持ち、
/// 上は画面を覆い尽くさないところで止める。
fn shadow_blur_of(v: Option<&str>) -> f32 {
    v.and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(SHADOW_BLUR)
        .clamp(0.5, 64.0)
}

/// 文字サイズ用。0 以下や解析できない値は既定へ落とす。
fn env_f32(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

/// バーローカル座標の矩形。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x as f64
            && x < (self.x + self.w) as f64
            && y >= self.y as f64
            && y < (self.y + self.h) as f64
    }
}

/// パネル内の各要素の位置(バーローカル座標)。
struct NpLayout {
    panel: Rect,
    art: Rect,
    col2_x: u32,
    col2_w: u32,
    row1_y: u32,
    row1_h: u32,
    row2_y: u32,
    row2_h: u32,
    /// 各ボタンの一辺。並びは `CTRLS` と同じ。
    btn_d: [u32; 5],
    btn_xs: [u32; 5],
    /// 各ボタンの上端。大きさが違っても上段の行の中央に揃える。
    btn_y: [u32; 5],
    prog: Rect,
    title_px: f32,
    artist_px: f32,
}

/// バーの左右端からの余白。clawd 枠の左端と再生情報パネルの右端で同じ値を使う
/// (左右対称にするため)。`TASKVAR_MARGIN` で変えられる。
const MARGIN: u32 = 12;
const INSET: u32 = 4; // バー上下からの余白
const PAD: u32 = 8; // パネル内側の余白
const GAP: u32 = 12; // 列間
const BTN_GAP: u32 = 10;
/// ボタン列と進捗バーの間隔。`TASKVAR_CTRL_GAP` で詰めたり広げたりできる。
const CTRL_GAP: u32 = 5;

/// パネルの幅を決める。
///
/// 既定はパネル幅(`np_w`)が主で、列②(曲名・アーティスト)はその余りを取る。
/// `text_w` が指定されたときは列②を主にし、パネル幅の方を逆算する。
/// どちらの場合も、左側(clawd 枠・アイコン列)に重ならない範囲 `avail` で
/// 頭打ちにする。
fn panel_width(avail: u32, np_w: u32, text_w: Option<u32>, art: u32, col3: u32) -> u32 {
    match text_w {
        Some(t) => PAD * 2 + art + GAP + t + GAP + col3,
        None => np_w,
    }
    .min(avail)
}

impl NpLayout {
    /// バー幅 w・高さ h、バー端からの余白 margin、パネルに使える最大幅 avail
    /// から算出する。avail は呼び手が「同じ幅の clawd 枠とアイコン列が左に並ぶ」
    /// ぶんを差し引いて渡す(`Bar::new`)。
    fn new(w: u32, h: u32, margin: u32, avail: u32) -> Self {
        // バー高さは env で変えられるので、低くしても破綻しないよう飽和で詰める。
        let panel_h = h.saturating_sub(INSET * 2);
        let content_h = panel_h.saturating_sub(PAD * 2).max(1);
        // 行の分割は content_h だけで決まる(上段=曲名/ボタン、下段=アーティスト/進捗バー)。
        let row1_h = (content_h / 2 + 2).min(content_h);
        let row2_h = content_h - row1_h;

        // 列①のアルバムアートは行をぶち抜く正方形、列③はボタン 5 個ぶん。
        // この 2 つは先に決まるので、パネル幅はそこから逆算できる。
        // ボタンは自分の行に収まる大きさまで(2x3 グリッドの升目をはみ出さない)。
        // 進捗バーの高さは先に決める。ボタンの上限がこれに依存するため。
        let prog_h =
            env_u32("TASKVAR_PROG_H", (content_h / 18).clamp(3, 8)).clamp(1, (content_h / 2).max(1));
        // ボタンは「進捗バーと合わせてパネルの高さに収まる」ところまで。
        // ボタン列と進捗バーの間隔。パネルからはみ出さないところまでで頭打ち。
        let ctrl_gap = env_u32("TASKVAR_CTRL_GAP", CTRL_GAP).min(panel_h.saturating_sub(prog_h));
        let btn_cap = panel_h.saturating_sub(ctrl_gap + prog_h);
        // 個別指定 > 5 つ共通の TASKVAR_BTN_D > ボタンごとの既定
        let base = env_opt_u32("TASKVAR_BTN_D");
        let want: [u32; 5] = std::array::from_fn(|i| {
            env_opt_u32(BTN_ENV[i]).or(base).unwrap_or(BTN_D_DEFAULT[i]).min(btn_cap)
        });
        let want_col3 = want.iter().sum::<u32>() + BTN_GAP * 4;
        let panel_w = panel_width(
            avail,
            env_u32("TASKVAR_NP_W", 400),
            env_opt_u32("TASKVAR_TEXT_W"),
            content_h,
            want_col3,
        );
        let panel = Rect { x: w - margin - panel_w, y: INSET, w: panel_w, h: panel_h };

        let content_x = panel.x + PAD;
        let content_y = panel.y + PAD;
        let content_w = panel_w - PAD * 2;

        let art_side = content_h.min(content_w);
        let art = Rect { x: content_x, y: content_y, w: art_side, h: art_side };

        // 列②③はアートの右の残り。パネルが狭いときは列③(ボタン)を先に確保し、
        // ボタンも入らないほど狭ければボタン自体を縮める。列②は最後に余りを取る。
        let rest = content_w.saturating_sub(art.w + GAP);
        let col3_w = want_col3.min(rest.saturating_sub(GAP));
        // 入りきらないときは 5 つとも同じ比率で詰める(大小関係は保つ)。
        let btn_d = if col3_w < want_col3 {
            let usable = col3_w.saturating_sub(BTN_GAP * 4);
            let total = want.iter().sum::<u32>().max(1);
            want.map(|d| d * usable / total)
        } else {
            want
        };
        let col2_w = rest.saturating_sub(GAP + col3_w);
        let col2_x = art.x + art.w + GAP;
        let col3_x = col2_x + col2_w + GAP;
        let (row1_y, row2_y) = (content_y, content_y + row1_h);

        // 列③(ボタン + 進捗バー)はテキストの行に合わせず、ひとまとまりとして
        // パネルの縦中央に置く。行に揃えると上へ寄って見えるため。
        // `TASKVAR_CTRL_DY` で上下に微調整できる(パネルからは出ない)。
        let btn_max = btn_d.iter().copied().max().unwrap_or(0);
        let group_h = btn_max + ctrl_gap + prog_h;
        let centered = panel.y + panel_h.saturating_sub(group_h) / 2;
        // 低いバーではボタン群が入り切らない。その場合は上端で止める(min > max 回避)。
        let lowest = (panel.y + panel_h).saturating_sub(group_h).max(panel.y);
        let group_y = (centered as i64 + env_i32("TASKVAR_CTRL_DY", 0) as i64)
            .clamp(panel.y as i64, lowest as i64) as u32;
        let btn_y = btn_d.map(|d| group_y + (btn_max - d) / 2);
        // 並べる位置。列③に入りきらないほど狭いときは右端で止めて、
        // ボタンがパネルの外へ出ないようにする。
        let col3_right = col3_x + col3_w;
        let mut btn_xs = [0u32; 5];
        let mut x = col3_x;
        for (i, slot) in btn_xs.iter_mut().enumerate() {
            *slot = x.min(col3_right.saturating_sub(btn_d[i]));
            x += btn_d[i] + BTN_GAP;
        }

        // 進捗バーはボタン列の直下、同じ幅で。
        let prog = Rect {
            x: col3_x,
            y: group_y + btn_max + ctrl_gap,
            // 端のボタンに合わせる(狭くて縮めたときも列③の名目幅とズレない)
            w: (btn_xs[4] + btn_d[4]).saturating_sub(col3_x),
            h: prog_h,
        };

        Self {
            panel,
            art,
            col2_x,
            col2_w,
            row1_y,
            row1_h,
            row2_y,
            row2_h,
            btn_d,
            btn_xs,
            btn_y,
            prog,
            // 文字の大きさは行の高さから決めない。既定は TITLE_PX / ARTIST_PX の
            // 決め打ちで、TASKVAR_TITLE_PX / TASKVAR_ARTIST_PX で好きな値にできる
            // (バーを低くしても縮まないので、その場合は自分で下げる)。
            title_px: env_f32("TASKVAR_TITLE_PX", TITLE_PX)
                .clamp(TEXT_PX_RANGE.0, TEXT_PX_RANGE.1),
            artist_px: env_f32("TASKVAR_ARTIST_PX", ARTIST_PX)
                .clamp(TEXT_PX_RANGE.0, TEXT_PX_RANGE.1),
        }
    }

    /// ボタン i の当たり判定(縦はパネル全高)。左右は隣との中間まで受け持つので、
    /// 大きさが違っても隙間なく列③を分け合い、見た目より広く押せる。
    fn ctrl_slot(&self, i: usize) -> Rect {
        let half = BTN_GAP / 2;
        Rect {
            x: self.btn_xs[i].saturating_sub(half),
            y: self.panel.y,
            w: self.btn_d[i] + BTN_GAP,
            h: self.panel.h,
        }
    }
}

/// clawd 枠の位置(バーローカル座標)。
struct ClawdLayout {
    panel: Rect,
    /// 1 個ぶんの区画(枠の内側)。左上から列優先(上→下、左→右)に並ぶ。
    /// 先に左の列が埋まるので、2 個までなら右の列は空いたままになる。
    /// 当たり判定もこれで行う。
    cells: Vec<Rect>,
    /// 1 列に並ぶ数(= 行数)。`i + grid_rows` が右隣の列の同じ段になるので、
    /// 「右の列が空いているか」の判定に使う。
    grid_rows: usize,
    /// キャラの大きさ(全セル共通)。
    sprite_w: u32,
    sprite_h: u32,
    name_px: f32,
}

impl ClawdLayout {
    /// バーの左端 x に幅 want で置き、中を rows x cols の格子に割る
    /// (want は再生情報パネルと同じ幅)。1 セルの幅が足りなければ
    /// None(枠ごと出さない)。
    ///
    /// パネルの有無で幅を変えたりはしない。曲が止まって再生情報が消えるたびに
    /// 枠が伸び縮みすると、タップ先が動いて押し間違えるため。
    fn new(h: u32, x: u32, want: u32) -> Option<Self> {
        let panel_w = env_opt_u32("TASKVAR_CLAWD_W").unwrap_or(want);
        if panel_w < CLAWD_MIN_W {
            return None;
        }
        let panel = Rect { x, y: INSET, w: panel_w, h: h.saturating_sub(INSET * 2) };
        let rows = env_u32("TASKVAR_CLAWD_ROWS", CLAWD_ROWS as u32).clamp(1, 8) as usize;
        let cols = env_u32("TASKVAR_CLAWD_COLS", CLAWD_COLS as u32).clamp(1, 4) as usize;
        let content_h = panel.h.saturating_sub(CLAWD_PAD_Y * 2);
        let row_h = content_h / rows as u32;
        if row_h <= CLAWD_ROW_GAP {
            return None;
        }
        let sprite_h = row_h - CLAWD_ROW_GAP;
        let sprite_w = sprite_h * sprite::ASPECT.0 / sprite::ASPECT.1;
        let content_w = panel.w.saturating_sub(CLAWD_PAD_X * 2);
        let gaps = CLAWD_COL_GAP * (cols as u32 - 1);
        let col_w = content_w.saturating_sub(gaps) / cols as u32;
        // キャラだけで埋まってしまうならセッション名が出せないので諦める
        if col_w <= sprite_w + CLAWD_NAME_GAP {
            return None;
        }
        // 行は枠の縦中央に固める(端数は上下へ均等に散らす)
        let top = panel.y + (panel.h - row_h * rows as u32) / 2;
        let left = panel.x + CLAWD_PAD_X;
        // 左の列から縦に埋める。少ないうちは右の列が空くので、そこを
        // 見出しの場所へ回せる(`draw_clawd`)
        let cells = (0..rows * cols)
            .map(|i| Rect {
                x: left + (i / rows) as u32 * (col_w + CLAWD_COL_GAP),
                y: top + (i % rows) as u32 * row_h,
                w: col_w,
                h: row_h,
            })
            .collect();
        // 見出しは 20px 固定。行高や行数には連動させない。
        let name_px = env_f32("TASKVAR_CLAWD_PX", CLAWD_NAME_PX).max(6.0);
        Some(Self { panel, cells, grid_rows: rows, sprite_w, sprite_h, name_px })
    }
}

/// clawd 枠に描く内容。main が毎ティック組み立てる。
pub struct ClawdView<'a> {
    /// 表示する行(`clawd::Model::rows` が選んだもの)。空なら枠ごと出さない。
    pub rows: &'a [Row],
    /// 走りアニメーションの位相。`CLAWD_BOB_MS` ごとに反転させる。
    pub phase: bool,
}

/// パネルに描く内容。main が毎ティック組み立てる。
pub struct NpView<'a> {
    pub np: &'a NowPlaying,
    pub player: PlayerState,
    /// アルバムアート(art 一辺の正方形・BGRA)。未取得なら None。
    pub art: Option<&'a [u8]>,
    /// 背景グラデーションの起点色。アートから採る。未取得なら Spotify グリーン。
    pub accent: Accent,
}

/// タップが当たった対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Icon(usize),
    Ctrl(Ctrl),
    /// パネル内のボタン以外。spotatui のセッションへ遷移する。
    Panel,
    /// clawd 枠の n 行目。その claude が動くペインへ遷移する。
    Clawd(usize),
}

pub struct Bar {
    pub w: u32,
    h: u32,
    /// 白円の直径。
    tile_d: u32,
    /// 各アイコンの白円の左上 x(y は「白円 + 下線」の塊をバー内で垂直センタリング)。
    xs: Vec<u32>,
    tile_y: u32,
    /// 下線の上端 y(白円の下端から `MARK_GAP` ぶん下)。
    mark_y: u32,
    glyphs: Vec<Glyph>,
    ctrl: Vec<Glyph>,
    art_fallback: Glyph,
    font: Option<Font>,
    np: NpLayout,
    clawd: Option<ClawdLayout>,
    /// キャラのスプライト。読めなければ枠を出さない。
    sprite: Option<Sprite>,
}

impl Bar {
    pub fn new(w: u32, h: u32) -> Result<Self> {
        let gap = env_u32("TASKVAR_GAP", 24);
        // 左右端の余白は共通(左は clawd 枠、右は再生情報パネルが接する)。
        let margin = env_u32("TASKVAR_MARGIN", MARGIN);
        // アイコンの大きさは `TASKVAR_ICON_D`、無指定ならバー高さ `TASKVAR_BAR_H`
        // (main.rs)から比で決まる。どちらもバーと左右の枠に収まるところで頭打ち。
        let want_d = env_u32("TASKVAR_ICON_D", h * ICON_D_PCT / 100);
        let tile_d = icon_d(w, h, want_d, gap, margin);
        let n = ICONS.len() as u32;
        let total = n * tile_d + (n - 1) * gap;
        // 縦は「白円 + 余白 + 下線」をひと塊にして中央へ置く。
        let block_h = tile_d + MARK_GAP + MARK_H;
        let tile_y = h.saturating_sub(block_h) / 2;
        let mark_y = tile_y + tile_d + MARK_GAP;
        let glyph_px = (tile_d * ICON_GLYPH_PCT / 100).max(1);
        let glyphs = ICONS.iter().map(|d| icons::render(d.svg, glyph_px)).collect::<Result<_>>()?;

        // 左から clawd 枠・アイコン列・再生情報パネルの順。幅は
        // 「パネル → 同じ幅の clawd 枠 → 残りにアイコン列」の順に決まる。
        // 左右の枠が同じ幅なので、パネルに使える幅は
        // 「アイコン列と余白を除いた残りの半分」が上限になる。
        let avail = w.saturating_sub(margin * 2 + GAP * 2 + total) / 2;
        let np = NpLayout::new(w, h, margin, avail);
        // clawd 枠はパネルと同じ幅で左端へ(左右が揃って中央のアイコン列が引き立つ)。
        let clawd = ClawdLayout::new(h, margin, np.panel.w);
        let ctrl = CTRL_SVGS
            .iter()
            .enumerate()
            .map(|(i, svg)| {
                // グリフはそれが乗るボタンの大きさで焼く(CTRL_SVGS の
                // 各要素は 1 つのボタンにだけ対応する)。
                let px = match i {
                    G_SHUFFLE => np.btn_d[0],
                    G_PREV => np.btn_d[1],
                    G_PLAY | G_PAUSE => np.btn_d[2] * PLAY_GLYPH_PCT / 100,
                    G_NEXT => np.btn_d[3],
                    _ => np.btn_d[4], // G_REPEAT / G_REPEAT1
                };
                icons::render(svg, px.max(1))
            })
            .collect::<Result<_>>()?;
        let art_fallback =
            icons::render(crate::actions::SPOTIFY.svg, (np.art.w * 55 / 100).max(1))?;

        // アイコン列は clawd 枠と再生情報パネルの間の中央。枠を出せなかったときは
        // バーの左端からの残り全部を間とみなす。
        let icons_x = clawd.as_ref().map(|l| l.panel.x + l.panel.w + GAP).unwrap_or(margin);
        let icons_right = np.panel.x.saturating_sub(GAP);
        let x0 = icons_x + icons_right.saturating_sub(icons_x).saturating_sub(total) / 2;
        let xs: Vec<u32> = (0..n).map(|i| x0 + i * (tile_d + gap)).collect();

        let sprite = match sprite::load() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("task-var: clawd の画像を読めません(枠は出しません): {e:#}");
                None
            }
        };

        Ok(Self {
            w,
            h,
            tile_d,
            xs,
            tile_y,
            mark_y,
            glyphs,
            ctrl,
            art_fallback,
            font: Font::load(),
            np,
            clawd,
            sprite,
        })
    }

    /// clawd 枠に並べられる個数(行数 x 列数)。
    /// 0 なら枠を出せない(場所が無い/画像が無い)。
    pub fn clawd_cells(&self) -> usize {
        match (&self.clawd, &self.sprite) {
            (Some(l), Some(_)) => l.cells.len(),
            _ => 0,
        }
    }

    /// clawd 枠の矩形(バーローカル)。キャラだけ動いたときの部分ブリットに使う。
    pub fn clawd_rect(&self) -> Option<Rect> {
        self.clawd.as_ref().map(|l| l.panel)
    }

    /// アルバムアートに要求する一辺(px)。
    pub fn art_side(&self) -> u32 {
        self.np.art.w
    }

    /// 再生情報パネルの矩形(バーローカル)。進捗だけ動いたときの部分ブリットに使う。
    pub fn np_rect(&self) -> Rect {
        self.np.panel
    }

    /// バー全体を buf(w*h*4 BGRA)へ描画する。
    pub fn draw(
        &self,
        buf: &mut [u8],
        state: &State,
        np: Option<&NpView>,
        clawd: Option<&ClawdView>,
    ) {
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&[BG[0], BG[1], BG[2], 0]);
        }
        // 影は全タイルぶんを先に敷く。1 タイルずつ影→円と描くと、
        // 間隔を詰めたときに隣の影が円の上へ乗ってしまうため。
        for &x0 in &self.xs {
            self.draw_shadow(buf, x0);
        }
        for (i, def) in ICONS.iter().enumerate() {
            self.draw_tile(buf, self.xs[i], mark(def, state), &self.glyphs[i]);
        }
        match np {
            Some(view) => self.draw_np(buf, view),
            // spotatui が居なくても枠だけは残す。場所が動かないので、
            // 起動していないことが一目で分かるうえタップ先もずれない。
            None => self.draw_np_idle(buf),
        }
        if let Some(view) = clawd {
            self.draw_clawd(buf, view);
        }
    }

    /// clawd 枠を描く。1 つも無くても場所だけは空けておく
    /// (claude が動いていない間もバーの形を変えないため)。
    fn draw_clawd(&self, buf: &mut [u8], view: &ClawdView) {
        let Some(l) = &self.clawd else { return };

        // 枠(背景・枠線)は持たない。キャラと見出しを地色の上へ直接置き、
        // 影で浮かせる。場所そのものは `ClawdLayout` が確保したまま動かさない
        // ので、claude が居ても居なくてもタップ先はずれない。
        let Some(sprite) = &self.sprite else { return };

        for (i, (row, r)) in view.rows.iter().zip(l.cells.iter()).enumerate() {
            let body = match row.st {
                St::Run => sprite.body,
                St::Ask => CLAWD_ASK,
                St::Done => CLAWD_DONE,
                St::Seen => CLAWD_SEEN,
            };
            // 処理中は少し縮めて上下に振り、その場で跳ねながら走って見せる。
            let (sh, dy) = match (row.st, view.phase) {
                (St::Run, true) => (l.sprite_h.saturating_sub(CLAWD_BOB).max(1), CLAWD_BOB),
                (St::Run, false) => (l.sprite_h.saturating_sub(CLAWD_BOB).max(1), 0),
                _ => (l.sprite_h, 0),
            };
            let bgra = sprite.render(l.sprite_w, sh, body);
            let sy = r.y + (r.h - l.sprite_h) / 2 + dy;
            // キャラの形そのままの影を右下へ薄く敷いてから本体を重ねる
            let shade = sprite.render(l.sprite_w, sh, SHADOW);
            blit_alpha_scaled(
                buf,
                self.w,
                self.h,
                r.x + CLAWD_SHADOW_DX,
                sy + CLAWD_SHADOW_DY,
                l.sprite_w,
                sh,
                &shade,
                shadow_a() * CLAWD_SHADOW_RATIO,
            );
            blit_alpha(buf, self.w, self.h, r.x, sy, l.sprite_w, sh, &bgra);

            // キャラの右に見出し(作業の要約 / ディレクトリ名)。地色が明るいので黒。
            // 確認済み(灰)は文字も落として控えめにする。
            let Some(font) = &self.font else { continue };
            let tx = r.x + l.sprite_w + CLAWD_NAME_GAP;
            // 右隣の列にキャラが居ないなら、見出しはそこも使って枠の右端で切る
            // (2 個までは左の列だけが埋まるので、そのときは枠いっぱいに出る)
            let right = if i + l.grid_rows >= view.rows.len() {
                l.panel.x + l.panel.w - CLAWD_PAD_X
            } else {
                r.x + r.w
            };
            let max = right.saturating_sub(tx) as f32;
            if max <= 4.0 {
                continue;
            }
            // 幅に入らなくても文字は縮めない。入るところまで出して `…` で詰める
            let px = l.name_px;
            let text = font.fit(&row.label, max, px);
            let color = if row.st == St::Seen { CLAWD_NAME_SEEN } else { CLAWD_NAME };
            let base = r.y as f32 + r.h as f32 / 2.0 + px * 0.35;
            font.draw(buf, self.w, self.h, tx as f32, base, px, color, &text);
        }
    }

    /// タイル 1 個ぶんの影。円をそのまま下へずらし、縁を `SHADOW_BLUR` かけて
    /// 滑らかに消す(内側は円が塗りつぶすので見えるのは外へはみ出したぶんだけ)。
    fn draw_shadow(&self, buf: &mut [u8], x0: u32) {
        let r = self.tile_d as f32 / 2.0;
        let cx = x0 as f32 + r;
        let cy = self.tile_y as f32 + r + SHADOW_DY;
        let reach = r + shadow_blur();
        let x_min = (cx - reach).floor().max(0.0) as u32;
        let y_min = (cy - reach).floor().max(0.0) as u32;
        let x_max = ((cx + reach).ceil() as u32).min(self.w.saturating_sub(1));
        let y_max = ((cy + reach).ceil() as u32).min(self.h.saturating_sub(1));
        for y in y_min..=y_max {
            for x in x_min..=x_max {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let t = ((reach - (dx * dx + dy * dy).sqrt()) / shadow_blur()).clamp(0.0, 1.0);
                // 線形のままだと縁が輪郭に見えるので smoothstep で落とす
                blend(buf, self.w, self.h, x, y, SHADOW, t * t * (3.0 - 2.0 * t) * shadow_a());
            }
        }
    }

    /// アイコン 1 個ぶん(白円 → グリフ → 下線)。外枠リングは持たず、
    /// 開いているかどうかは円の下の横線が示す。
    fn draw_tile(&self, buf: &mut [u8], x0: u32, mark: Mark, glyph: &Glyph) {
        let d = self.tile_d;
        let r = d as f32 / 2.0;
        for j in 0..d {
            for i in 0..d {
                let (dx, dy) = (i as f32 + 0.5 - r, j as f32 + 0.5 - r);
                // 円の縁は 1px の線形カバレッジで滑らかに。半端なぶんは地色ではなく
                // **下地**(影)へ溶かす。地色固定にすると円のまわりだけ影が抜けて
                // 白く縁取られてしまうため。
                let cov = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
                blend(buf, self.w, self.h, x0 + i, self.tile_y + j, WHITE, cov);
            }
        }
        let off = (d - glyph.px) / 2;
        premul_glyph(buf, self.w, x0 + off, self.tile_y + off, glyph);

        let Some((color, pct)) = mark.style() else { return };
        let mw = (d * pct / 100).max(MARK_H);
        let rect = Rect { x: x0 + (d - mw) / 2, y: self.mark_y, w: mw, h: MARK_H };
        round_rect(buf, self.w, self.h, rect, MARK_H as f32 / 2.0, color);
    }

    /// パネルの下地。枠線の角丸矩形の内側を 1px 詰めて塗る。
    fn draw_np_bg(&self, buf: &mut [u8], accent: Accent) {
        let p = self.np.panel;
        if p.w < 3 || p.h < 3 {
            return; // バーを極端に低く/アイコンを大きくしてパネルが潰れた
        }
        round_rect_shadow(buf, self.w, self.h, p, 10.0);
        round_rect_grad(buf, self.w, self.h, p, 10.0, accent.edge, PANEL_EDGE);
        let inner = Rect { x: p.x + 1, y: p.y + 1, w: p.w - 2, h: p.h - 2 };
        round_rect_grad(buf, self.w, self.h, inner, 9.0, accent.fill, PANEL);
    }

    /// 列①のアート未取得時の代わり。下地の上に Spotify のアイコンを置く。
    fn draw_art_placeholder(&self, buf: &mut [u8]) {
        let a = self.np.art;
        round_rect(buf, self.w, self.h, a, 6.0, ART_BG);
        let g = &self.art_fallback;
        premul_glyph(buf, self.w, a.x + (a.w - g.px) / 2, a.y + (a.h - g.px) / 2, g);
    }

    /// spotatui が居ないときのパネル。枠と、アルバムアートの場所に置いた
    /// Spotify のアイコンだけ(曲名・ボタン・進捗バーは出さない)。
    /// 背景の起点色はアートが無いときと同じ Spotify グリーン。
    fn draw_np_idle(&self, buf: &mut [u8]) {
        self.draw_np_bg(buf, Accent::default());
        self.draw_art_placeholder(buf);
    }

    /// 再生情報パネルを描く。
    fn draw_np(&self, buf: &mut [u8], view: &NpView) {
        let l = &self.np;
        self.draw_np_bg(buf, view.accent);

        // 列①: アルバムアート(角は 6px の丸め。パネル色へブレンドして落とす)
        match view.art {
            Some(bgra) if bgra.len() >= (l.art.w * l.art.h * 4) as usize => {
                blit_round(buf, self.w, self.h, l.art, bgra, 6.0);
            }
            _ => self.draw_art_placeholder(buf),
        }

        // 列②: 曲名 / アーティスト名
        if let Some(font) = &self.font {
            let max = l.col2_w as f32;
            if max > 4.0 {
                // 収まらないときはまず文字を縮めて入れる。下限まで縮めても
                // 入らない場合だけ `…` で詰める。
                let title_px =
                    font.shrink_to_fit(&view.np.track, max, l.title_px, l.title_px * SHRINK_MIN_PCT);
                let artist_px = font.shrink_to_fit(
                    &view.np.artist,
                    max,
                    l.artist_px,
                    l.artist_px * SHRINK_MIN_PCT,
                );

                let title = font.fit(&view.np.track, max, title_px);
                let base1 = l.row1_y as f32 + l.row1_h as f32 / 2.0 + title_px * 0.35;
                font.draw(buf, self.w, self.h, l.col2_x as f32, base1, title_px, WHITE, &title);

                let artist = font.fit(&view.np.artist, max, artist_px);
                let base2 = l.row2_y as f32 + l.row2_h as f32 / 2.0 + artist_px * 0.35;
                font.draw(buf, self.w, self.h, l.col2_x as f32, base2, artist_px, SUB_TEXT, &artist);
            }
        }

        // 列③ row1: 操作ボタン
        for (i, ctrl) in CTRLS.iter().enumerate() {
            self.draw_ctrl(buf, i, *ctrl, &view.player);
        }

        // 列③ row2: 進捗バー
        let r = l.prog.h as f32 / 2.0;
        round_rect(buf, self.w, self.h, l.prog, r, GROOVE);
        let ratio = view.np.progress_ratio().clamp(0.0, 1.0);
        let filled = (l.prog.w as f32 * ratio).round() as u32;
        if filled >= l.prog.h {
            round_rect(buf, self.w, self.h, Rect { w: filled, ..l.prog }, r, GREEN);
        }
    }

    /// ボタン 1 個。シャッフル/リピートは ON のとき緑、再生/停止は白円 + 黒グリフ。
    fn draw_ctrl(&self, buf: &mut [u8], i: usize, ctrl: Ctrl, p: &PlayerState) {
        let (x, y, d) = (self.np.btn_xs[i], self.np.btn_y[i], self.np.btn_d[i]);
        let (gi, color) = match ctrl {
            Ctrl::Shuffle => (G_SHUFFLE, if p.shuffle { GREEN } else { WHITE }),
            Ctrl::Prev => (G_PREV, WHITE),
            Ctrl::Next => (G_NEXT, WHITE),
            Ctrl::Repeat => match p.repeat {
                Loop::Off => (G_REPEAT, WHITE),
                Loop::Playlist => (G_REPEAT, GREEN),
                Loop::Track => (G_REPEAT1, GREEN),
            },
            Ctrl::PlayPause => {
                // Spotify のプレイヤーバーに倣い、白い塗り円に黒いグリフ
                circle(buf, self.w, self.h, x, y, d, WHITE);
                let g = &self.ctrl[if p.playing { G_PAUSE } else { G_PLAY }];
                // 低いバーではボタンがグリフより小さくなりうる(グリフは最低 1px)
                let off = d.saturating_sub(g.px) / 2;
                tint_glyph(buf, self.w, self.h, x + off, y + off, g, BLACK);
                return;
            }
        };
        let g = &self.ctrl[gi];
        let off = (d.saturating_sub(g.px)) / 2;
        tint_glyph(buf, self.w, self.h, x + off, y + off, g, color);
    }

    /// バーローカル座標 (lx,ly) が何に当たるか。`ctrls_shown` が false のときは
    /// 操作ボタンを描いていない(spotatui が居ない)ので、パネル内はどこを
    /// 押しても遷移になる。`clawd_cells` は
    /// いま描いている clawd の個数(描いていないセルは当たらない)。
    pub fn hit(&self, lx: f64, ly: f64, ctrls_shown: bool, clawd_cells: usize) -> Option<Hit> {
        if let Some(l) = &self.clawd {
            for (i, r) in l.cells.iter().take(clawd_cells).enumerate() {
                if r.contains(lx, ly) {
                    return Some(Hit::Clawd(i));
                }
            }
        }
        if ctrls_shown {
            for (i, ctrl) in CTRLS.iter().enumerate() {
                if self.np.ctrl_slot(i).contains(lx, ly) {
                    return Some(Hit::Ctrl(*ctrl));
                }
            }
        }
        // ボタンに当たらなかったパネル内(アート・曲名・余白)は遷移。
        // 枠だけの状態でも同じで、押せば spotatui のセッションが立ち上がる。
        if self.np.panel.contains(lx, ly) {
            return Some(Hit::Panel);
        }
        // 白円と下線をまとめて受け、その少し外までタッチを許容する。
        for (i, &x0) in self.xs.iter().enumerate() {
            let slot = Rect {
                x: x0.saturating_sub(ICON_HIT_PAD),
                y: self.tile_y.saturating_sub(ICON_HIT_PAD),
                w: self.tile_d + ICON_HIT_PAD * 2,
                h: (self.mark_y + MARK_H - self.tile_y) + ICON_HIT_PAD * 2,
            };
            if slot.contains(lx, ly) {
                return Some(Hit::Icon(i));
            }
        }
        None
    }
}

/// 1 ピクセルを被覆率 cov で合成する。α バイトは 0 のまま(fb は XRGB8888)。
fn blend(buf: &mut [u8], w: u32, h: u32, x: u32, y: u32, color: [u8; 3], cov: f32) {
    if cov <= 0.0 || x >= w || y >= h {
        return;
    }
    let cov = cov.min(1.0);
    let off = ((y * w + x) * 4) as usize;
    for k in 0..3 {
        let dst = buf[off + k] as f32;
        buf[off + k] = (dst * (1.0 - cov) + color[k] as f32 * cov).round() as u8;
    }
    buf[off + 3] = 0;
}

/// 矩形内のピクセル (i,j) の、角丸半径 r に対する被覆率。
fn round_cov(i: u32, j: u32, rect: Rect, r: f32) -> f32 {
    let (px, py) = (i as f32 + 0.5, j as f32 + 0.5);
    let dx = (r - px).max(px - (rect.w as f32 - r)).max(0.0);
    let dy = (r - py).max(py - (rect.h as f32 - r)).max(0.0);
    (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0)
}

/// 矩形内のピクセル (i,j) における `from`→`to` のグラデーション色。
/// 横方向を主に、少しだけ斜めへ倒す。`GRAD_SPREAD` から右はもう `to` の単色。
fn grad_at(i: u32, j: u32, rect: Rect, from: [u8; 3], to: [u8; 3]) -> [u8; 3] {
    let u = i as f32 / rect.w.max(1) as f32;
    let v = j as f32 / rect.h.max(1) as f32;
    let t = ((u * 0.85 + v * 0.15) / GRAD_SPREAD).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t); // falloff の折れ目を目立たせない
    std::array::from_fn(|k| (from[k] as f32 + (to[k] as f32 - from[k] as f32) * t).round() as u8)
}

/// 角丸矩形をグラデーションで塗る(角は 1px の線形カバレッジ)。
fn round_rect_grad(
    buf: &mut [u8],
    w: u32,
    h: u32,
    rect: Rect,
    r: f32,
    from: [u8; 3],
    to: [u8; 3],
) {
    let r = r.min(rect.w as f32 / 2.0).min(rect.h as f32 / 2.0).max(0.0);
    for j in 0..rect.h {
        for i in 0..rect.w {
            let c = grad_at(i, j, rect, from, to);
            blend(buf, w, h, rect.x + i, rect.y + j, c, round_cov(i, j, rect, r));
        }
    }
}

/// 角丸矩形を単色で塗る。
/// 角丸矩形(2 つの枠)の影。アイコンの円と同じで、矩形を `SHADOW_DY` だけ下へ
/// ずらし、縁を `SHADOW_BLUR` かけて消す。距離は角丸矩形の符号付き距離で測る。
fn round_rect_shadow(buf: &mut [u8], w: u32, h: u32, rect: Rect, r: f32) {
    let (cx, cy) = (
        rect.x as f32 + rect.w as f32 / 2.0,
        rect.y as f32 + rect.h as f32 / 2.0 + SHADOW_DY,
    );
    let (hx, hy) = (rect.w as f32 / 2.0, rect.h as f32 / 2.0);
    let x0 = (cx - hx - shadow_blur()).floor().max(0.0) as u32;
    let y0 = (cy - hy - shadow_blur()).floor().max(0.0) as u32;
    let x1 = ((cx + hx + shadow_blur()).ceil() as u32).min(w.saturating_sub(1));
    let y1 = ((cy + hy + shadow_blur()).ceil() as u32).min(h.saturating_sub(1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = (x as f32 + 0.5 - cx).abs() - (hx - r);
            let dy = (y as f32 + 0.5 - cy).abs() - (hy - r);
            // 角丸矩形の符号付き距離(内側が負、外側が正)
            let out = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
            let d = out + dx.max(dy).min(0.0) - r;
            // 縁(d=0)で最大、外へ SHADOW_BLUR ぶんかけて 0 へ落とす
            let t = ((shadow_blur() - d) / shadow_blur()).clamp(0.0, 1.0);
            blend(buf, w, h, x, y, SHADOW, t * t * (3.0 - 2.0 * t) * shadow_a());
        }
    }
}

fn round_rect(buf: &mut [u8], w: u32, h: u32, rect: Rect, r: f32, color: [u8; 3]) {
    round_rect_grad(buf, w, h, rect, r, color, color);
}

/// 塗りつぶし円(再生/停止ボタンの下地)。
fn circle(buf: &mut [u8], w: u32, h: u32, x0: u32, y0: u32, d: u32, color: [u8; 3]) {
    let c = d as f32 / 2.0;
    for j in 0..d {
        for i in 0..d {
            let (dx, dy) = (i as f32 + 0.5 - c, j as f32 + 0.5 - c);
            let cov = (c - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            blend(buf, w, h, x0 + i, y0 + j, color, cov);
        }
    }
}

/// BGRA 画像を角丸で貼る。角の外は下地(すでに描いたパネル)をそのまま残すので、
/// 背景がグラデーションでも角が浮かない。
fn blit_round(buf: &mut [u8], w: u32, h: u32, rect: Rect, src: &[u8], r: f32) {
    for j in 0..rect.h {
        for i in 0..rect.w {
            let s = ((j * rect.w + i) * 4) as usize;
            let cov = round_cov(i, j, rect, r);
            blend(buf, w, h, rect.x + i, rect.y + j, [src[s], src[s + 1], src[s + 2]], cov);
        }
    }
}

/// ストレートアルファの BGRA を下地へ合成する(clawd のスプライト用)。
/// α をカバレッジとして扱うので、縮小でぼけた輪郭が枠の色へなじむ。
#[allow(clippy::too_many_arguments)]
/// `blit_alpha` の α を a 倍して重ねる版。キャラの影に使う。
#[allow(clippy::too_many_arguments)]
fn blit_alpha_scaled(
    buf: &mut [u8],
    w: u32,
    h: u32,
    x0: u32,
    y0: u32,
    sw: u32,
    sh: u32,
    src: &[u8],
    a: f32,
) {
    for j in 0..sh {
        for i in 0..sw {
            let s = ((j * sw + i) * 4) as usize;
            if src[s + 3] == 0 {
                continue;
            }
            let cov = src[s + 3] as f32 / 255.0 * a;
            blend(buf, w, h, x0 + i, y0 + j, [src[s], src[s + 1], src[s + 2]], cov);
        }
    }
}

fn blit_alpha(buf: &mut [u8], w: u32, h: u32, x0: u32, y0: u32, sw: u32, sh: u32, src: &[u8]) {
    for j in 0..sh {
        for i in 0..sw {
            let s = ((j * sw + i) * 4) as usize;
            let a = src[s + 3];
            if a == 0 {
                continue;
            }
            blend(buf, w, h, x0 + i, y0 + j, [src[s], src[s + 1], src[s + 2]], a as f32 / 255.0);
        }
    }
}

/// プリマルチプライド RGBA のグリフをそのまま合成する(ブランドカラー付き SVG 用)。
fn premul_glyph(buf: &mut [u8], w: u32, x0: u32, y0: u32, glyph: &Glyph) {
    for j in 0..glyph.px {
        for i in 0..glyph.px {
            let s = ((j * glyph.px + i) * 4) as usize;
            let (sr, sg, sb, sa) =
                (glyph.rgba[s], glyph.rgba[s + 1], glyph.rgba[s + 2], glyph.rgba[s + 3]);
            if sa == 0 {
                continue;
            }
            let off = (((y0 + j) * w + x0 + i) * 4) as usize;
            let inv = (255 - sa) as u32;
            buf[off] = (sb as u32 + buf[off] as u32 * inv / 255) as u8;
            buf[off + 1] = (sg as u32 + buf[off + 1] as u32 * inv / 255) as u8;
            buf[off + 2] = (sr as u32 + buf[off + 2] as u32 * inv / 255) as u8;
        }
    }
}

/// 単色 SVG のグリフを、アルファをマスクとして任意色で描く。
/// ON/OFF で色が変わるボタンを 1 回のレンダリングで賄うため。
fn tint_glyph(buf: &mut [u8], w: u32, h: u32, x0: u32, y0: u32, glyph: &Glyph, color: [u8; 3]) {
    for j in 0..glyph.px {
        for i in 0..glyph.px {
            let a = glyph.rgba[((j * glyph.px + i) * 4 + 3) as usize];
            if a == 0 {
                continue;
            }
            blend(buf, w, h, x0 + i, y0 + j, color, a as f32 / 255.0);
        }
    }
}

/// アイコン下の下線(バー描画とテストの両方から使う)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// 表示中のセッション。水色で、`MARK_PCT` の長さ。
    Active,
    /// 開いているが表示していないセッション。灰色で、その半分の長さ。
    Open,
    /// セッションが無い。下線を引かない。
    None,
}

impl Mark {
    /// 下線の色と、タイル幅に対する長さの割合。`None` なら引かない。
    fn style(self) -> Option<([u8; 3], u32)> {
        match self {
            Mark::Active => Some((BLUE, MARK_PCT)),
            Mark::Open => Some((GRAY, MARK_OPEN_PCT)),
            Mark::None => Option::None,
        }
    }
}

/// 白円の直径を決める。`want` は `TASKVAR_ICON_D`(無指定ならバー高さ比)。
/// 縦は「円 + 余白 + 下線」がバーに収まるところまで、横はアイコン列を並べても
/// 左右の枠に `NP_MIN_W` ずつ残るところまでで頭打ちにする。
fn icon_d(w: u32, h: u32, want: u32, gap: u32, margin: u32) -> u32 {
    let n = ICONS.len() as u32;
    let by_h = h.saturating_sub(MARK_GAP + MARK_H);
    let room = w.saturating_sub(margin * 2 + GAP * 2 + NP_MIN_W * 2 + (n - 1) * gap);
    want.min(by_h).min(room / n).max(ICON_D_MIN)
}

fn mark(def: &IconDef, state: &State) -> Mark {
    match def.session {
        Some(sess) => {
            if state.current.as_deref() == Some(sess) {
                Mark::Active
            } else if state.existing.iter().any(|s| s == sess) {
                Mark::Open
            } else {
                Mark::None
            }
        }
        // tmux アイコン: 名前付きセッション以外(=通常の作業セッション)を表示中なら水色。
        // spotify はアイコン列に居ないが名前付きセッションではあるので数に入れる。
        // tmux スイッチャーは閉じることが無いので、下線が消えることはない
        // (名前付きセッションを見ている間は灰色になるだけ)。
        None => {
            let named: Vec<&str> = ICONS
                .iter()
                .chain(std::iter::once(crate::actions::spotify()))
                .filter_map(|d| d.session)
                .collect();
            match state.current.as_deref() {
                Some(cur) if !named.contains(&cur) => Mark::Active,
                _ => Mark::Open,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(current: &str, existing: &[&str]) -> State {
        State {
            client: Some("/dev/tty1".into()),
            current: Some(current.into()),
            existing: existing.iter().map(|s| s.to_string()).collect(),
            first_session: existing.first().map(|s| s.to_string()),
        }
    }

    fn now_playing() -> NowPlaying {
        NowPlaying {
            track: "I mean, It's about time".into(),
            artist: "NCT 127".into(),
            art_url: None,
            progress_ms: 42_620,
            duration_ms: 128_000,
            written_at_ms: 0,
            is_playing: true,
        }
    }

    #[test]
    fn marks_follow_session_state() {
        // ICONS: [tmux, shorts, bluetooth, ssbrowse, eduroam, calendar]
        // (spotify は再生情報パネルが受け持つのでアイコン列には並べない)
        let st = state("bluetooth", &["bluetooth", "shorts"]);
        assert_eq!(mark(&ICONS[2], &st), Mark::Active, "表示中は水色");
        assert_eq!(mark(&ICONS[1], &st), Mark::Open, "存在するが非表示は灰色");
        assert_eq!(mark(&ICONS[3], &st), Mark::None, "セッション無しは下線なし");
        assert_eq!(mark(&ICONS[0], &st), Mark::Open, "名前付きセッション表示中のtmuxは灰色");
        // spotify も名前付きセッション。アイコンが無くても tmux は灰色のまま
        let sp = state("spotify", &["spotify"]);
        assert_eq!(mark(&ICONS[0], &sp), Mark::Open, "spotify 表示中のtmuxは灰色");
        let st2 = state("main", &["main", "spotify"]);
        assert_eq!(mark(&ICONS[0], &st2), Mark::Active, "通常セッション表示中のtmuxは水色");
        // tmux アイコンだけは、どの状態でも下線が消えない
        for cur in ["main", "spotify", "bluetooth"] {
            assert_ne!(mark(&ICONS[0], &state(cur, &[cur])), Mark::None, "tmux の下線が消えた");
        }
    }

    /// 下線の帯の色。`x` はタイル中心からの相対位置(px)。
    fn mark_px(buf: &[u8], bar: &Bar, i: usize, dx: i32) -> [u8; 3] {
        let x = (bar.xs[i] + bar.tile_d / 2) as i32 + dx;
        let off = ((bar.mark_y + MARK_H / 2) * bar.w + x as u32) as usize * 4;
        [buf[off], buf[off + 1], buf[off + 2]]
    }

    #[test]
    fn icon_size_follows_the_bar_height() {
        // 無指定ならバー高さ比。既定の 88px バーでは従来どおり 64px
        let want = |h: u32| h * ICON_D_PCT / 100;
        assert_eq!(icon_d(1366, 88, want(88), 24, MARGIN), 64);
        assert_eq!(icon_d(1366, 128, want(128), 24, MARGIN), 93, "バーを高くすると育つ");
        assert_eq!(icon_d(1366, 48, want(48), 24, MARGIN), 35, "バーを低くすると縮む");

        // 縦は「円 + 余白 + 下線」がバーに収まるところまで
        for h in [24u32, 40, 88, 200] {
            let d = icon_d(1366, h, 9999, 24, MARGIN);
            assert!(d + MARK_GAP + MARK_H <= h, "h={h} で下線がバーからはみ出す");
        }
        // 横は、アイコン列を並べても左右の枠に NP_MIN_W ずつ残るところまで
        let d = icon_d(1366, 400, 9999, 24, MARGIN);
        let n = ICONS.len() as u32;
        let total = n * d + (n - 1) * 24;
        assert!(
            total + MARGIN * 2 + GAP * 2 + NP_MIN_W * 2 <= 1366,
            "アイコン列が枠の場所を食っている: total={total}"
        );
        // 下限は割り込まない(狭い画面でもアイコンは残す)
        assert_eq!(icon_d(320, 88, want(88), 24, MARGIN), ICON_D_MIN);
    }

    #[test]
    fn text_size_is_fixed_not_derived_from_the_bar() {
        // バーの高さを変えても曲名・アーティストの大きさは動かない
        for h in [40u32, 64, 96, 160] {
            let l = NpLayout::new(1366, h, MARGIN, 400);
            assert_eq!(l.title_px, TITLE_PX, "h={h} で曲名の大きさが変わった");
            assert_eq!(l.artist_px, ARTIST_PX, "h={h} でアーティスト名の大きさが変わった");
        }
    }

    #[test]
    fn layout_survives_any_bar_height() {
        // TASKVAR_BAR_H は 24px〜画面の半分まで動かせる(main.rs)。
        // どの高さでも組み立てと描画が成立し、アイコン列が左右の枠と重ならない。
        let w = 1366;
        let heights = (24..=200).step_by(8).chain([256, 384]);
        for h in heights {
            let bar = Bar::new(w, h).unwrap();
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let np = now_playing();
            let view = NpView {
                np: &np,
                player: PlayerState { playing: true, shuffle: false, repeat: Loop::Off },
                art: None,
                accent: Accent::default(),
            };
            let rows = vec![Row { pane: "%1".into(), label: "main".into(), st: St::Run }];
            let clawd = ClawdView { rows: &rows, phase: false };
            bar.draw(&mut buf, &state("main", &["main"]), Some(&view), Some(&clawd));

            // 白円 + 下線はバーの中に収まる
            assert!(bar.mark_y + MARK_H <= h, "h={h} で下線がバーの外");
            // アイコン列は左の clawd 枠と右のパネルに重ならない
            let icons_right = bar.xs[ICONS.len() - 1] + bar.tile_d;
            assert!(icons_right <= bar.np_rect().x, "h={h} でパネルに重なった");
            if let Some(l) = &bar.clawd {
                assert!(l.panel.x + l.panel.w <= bar.xs[0], "h={h} で clawd 枠に重なった");
            }
        }
    }

    /// `TASKVAR_TEST_ART` が指す画像を side x side の BGRA にして返す。
    fn test_art(side: u32) -> Option<Vec<u8>> {
        let path = std::env::var("TASKVAR_TEST_ART").ok()?;
        let img = image::open(&path)
            .unwrap_or_else(|e| panic!("{path} を開けない: {e}"))
            .resize_exact(side, side, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        Some(img.pixels().flat_map(|p| [p.0[2], p.0[1], p.0[0], 0]).collect())
    }

    #[test]
    fn draw_and_hit() {
        // 実機と同じ 1366x96(fbterm のセル高 16px にスナップされた値)
        let (w, h) = (1366u32, 96u32);
        let bar = Bar::new(w, h).unwrap();
        let mut buf = vec![0u8; (w * h * 4) as usize];
        // shorts=表示中(水色) / bluetooth=開いているだけ(灰色) / 他=下線なし
        let st = state("shorts", &["shorts", "bluetooth"]);
        let np = now_playing();
        // TASKVAR_TEST_ART=画像パス で実際のジャケットを流し込める
        // (アートから採る背景色を目視で確かめるため)。
        let art = test_art(bar.art_side());
        let view = NpView {
            np: &np,
            player: PlayerState { playing: true, shuffle: true, repeat: Loop::Playlist },
            art: art.as_deref(),
            accent: art.as_deref().map(crate::art::accent).unwrap_or_default(),
        };
        bar.draw(&mut buf, &st, Some(&view), None);

        let px = |buf: &[u8], x: u32, y: u32| -> [u8; 3] {
            let off = ((y * w + x) * 4) as usize;
            [buf[off], buf[off + 1], buf[off + 2]]
        };
        let (cx, cy) = (bar.xs[1] + bar.tile_d / 2, bar.tile_y + bar.tile_d / 2);
        // 白円内・グリフ外の点は白(グリフは d*58% なので中心から ±d*0.29 まで)
        assert_eq!(px(&buf, cx + bar.tile_d * 38 / 100, cy), WHITE);
        // 外枠リングは持たない。円の縁まで白のまま
        assert_eq!(px(&buf, cx + bar.tile_d / 2 - 2, cy), WHITE, "外枠リングが残っている");

        // 水色は円の直径の半分、灰色はさらにその半分
        let half = (bar.tile_d * MARK_PCT / 100 / 2) as i32;
        let open_half = (bar.tile_d * MARK_OPEN_PCT / 100 / 2) as i32;
        // shorts=表示中 → 水色
        assert_eq!(mark_px(&buf, &bar, 1, 0), BLUE, "表示中の下線が水色でない");
        assert_eq!(mark_px(&buf, &bar, 1, half - 2), BLUE, "表示中の下線が短い");
        assert_ne!(mark_px(&buf, &bar, 1, half + 2), BLUE, "表示中の下線が長い");
        // bluetooth=開いているだけ → 水色の半分の長さの灰色
        assert_eq!(mark_px(&buf, &bar, 2, 0), GRAY, "非表示セッションの下線が灰色でない");
        assert_eq!(mark_px(&buf, &bar, 2, open_half - 2), GRAY, "灰色の下線が短い");
        assert_ne!(mark_px(&buf, &bar, 2, open_half + 2), GRAY, "灰色の下線が長い");
        assert_ne!(mark_px(&buf, &bar, 2, half - 2), GRAY, "灰色が水色と同じ長さのまま");
        // ssbrowse=セッション無し → 下線なし(円の影が届く場所なので地色より暗い)
        let none = mark_px(&buf, &bar, 3, 0);
        assert!(none != BLUE && none != GRAY, "セッション無しに下線が出ている: {none:?}");
        // tmux は名前付きセッション(shorts)を表示中なので灰色。消えることはない
        assert_eq!(mark_px(&buf, &bar, 0, 0), GRAY, "tmux の下線が消えている");

        // アイコン列は clawd 枠とパネルの間の中央。左右の余りが揃っている
        let icons_right = bar.xs[ICONS.len() - 1] + bar.tile_d;
        let l = bar.clawd.as_ref().unwrap();
        let left_gap = bar.xs[0] - (l.panel.x + l.panel.w);
        let right_gap = bar.np_rect().x - icons_right;
        assert!(left_gap.abs_diff(right_gap) <= 1, "中央でない: 左 {left_gap} / 右 {right_gap}");
        // アイコン列から離れた場所は素通しの地色
        assert_eq!(px(&buf, icons_right + 4, 2), BG);
        assert_eq!(px(&buf, l.panel.x + l.panel.w + 4, 2), BG);
        // 円の真下には影。地色より暗く、下へ離れるほど薄くなって地色へ戻る
        // (下線と重なると影だけを見られないので、下線の無い ssbrowse で測る)
        let ncx = bar.xs[3] + bar.tile_d / 2;
        let below = |dy: u32| px(&buf, ncx, bar.tile_y + bar.tile_d + dy);
        let near = below(1);
        assert!(near.iter().zip(BG).all(|(a, b)| *a < b), "円の下に影が無い: {near:?}");
        let far = below((shadow_blur() + SHADOW_DY).ceil() as u32);
        assert_eq!(far, BG, "影がぼかし幅より先まで届いている");
        assert!(near[2] < below(3)[2], "下へ行くほど薄くなっていない");

        // パネルは地色ではなく Spotify のパネル色で塗られている
        let p = bar.np_rect();
        assert!(p.x > icons_right, "パネルはアイコン列より右");
        // 背景は左端が accent(既定は Spotify グリーン、アートがあればその色)、
        // 右端が素の暗色。左端はまだ落ち始めたばかりなので accent とほぼ一致する。
        let left = px(&buf, p.x + 4, p.y + p.h / 2);
        assert!(
            left.iter().zip(view.accent.fill).all(|(a, b)| a.abs_diff(b) <= 12),
            "左端が accent の色になっていない: {left:?} vs {:?}",
            view.accent.fill
        );
        assert_eq!(px(&buf, p.x + p.w - 4, p.y + p.h / 2), PANEL, "右端は素のパネル色");
        // 枠にもアイコンと同じ影。パネルのすぐ左は地色より暗い
        let side = px(&buf, p.x - 2, p.y + p.h / 2);
        assert!(side.iter().zip(BG).all(|(a, b)| *a < b), "枠の外に影が無い: {side:?}");

        // 進捗バー: 塗り部分は緑、末尾側は溝の色
        let prog = bar.np.prog;
        assert_eq!(px(&buf, prog.x + 2, prog.y + prog.h / 2), GREEN);
        assert_eq!(px(&buf, prog.x + prog.w - 2, prog.y + prog.h / 2), GROOVE);

        // 当たり判定: アイコン中心はヒット、アイコン列の左外は外れ
        assert_eq!(bar.hit(cx as f64, cy as f64, true, 0), Some(Hit::Icon(1)));
        assert_eq!(bar.hit(2.0, cy as f64, true, 0), None);

        // 列③の 5 スロットの中心はそれぞれのボタンを返す
        for (i, ctrl) in CTRLS.iter().enumerate() {
            let s = bar.np.ctrl_slot(i);
            let (sx, sy) = ((s.x + s.w / 2) as f64, (s.y + s.h / 2) as f64);
            assert_eq!(bar.hit(sx, sy, true, 0), Some(Hit::Ctrl(*ctrl)), "スロット {i}");
            // ボタンを描いていないときは、同じ場所でも遷移扱いになる
            assert_eq!(bar.hit(sx, sy, false, 0), Some(Hit::Panel), "枠だけのスロット {i}");
        }

        // ボタン以外のパネル内(アート・曲名・左端の余白)は遷移扱い
        let a = bar.np.art;
        let art_c = ((a.x + a.w / 2) as f64, (a.y + a.h / 2) as f64);
        assert_eq!(bar.hit(art_c.0, art_c.1, true, 0), Some(Hit::Panel), "アルバムアート");
        let p = bar.np.panel;
        let text_y = (p.y + p.h / 2) as f64;
        assert_eq!(bar.hit(bar.np.col2_x as f64 + 4.0, text_y, true, 0), Some(Hit::Panel), "曲名");
        assert_eq!(bar.hit((p.x + 2) as f64, text_y, true, 0), Some(Hit::Panel), "パネル左端");
        // パネルの外はパネル判定にならない。アイコン列が隣に来たので、
        // 間の隙間はアイコン側の許容範囲(円の外 8px)が受け持つ
        assert_ne!(bar.hit((p.x - 4) as f64, text_y, true, 0), Some(Hit::Panel), "パネルの左外");
        // 枠だけの状態でもパネル内は遷移のまま
        assert_eq!(bar.hit(art_c.0, art_c.1, false, 0), Some(Hit::Panel), "枠だけのアート");

        // TASKVAR_TEST_DUMP=path で目視確認用の PPM を書き出す。
        // トグルの ON/OFF でグリフと色が変わるので、両方の状態を出す
        // (path と path.off の 2 枚)。clawd 枠も 4 状態そろえて入れる。
        if let Ok(path) = std::env::var("TASKVAR_TEST_DUMP") {
            let clawd_rows = [
                ("touch-claude廃止とセッション表示枠", St::Run),
                ("フレームバッファの周辺知識", St::Ask),
                ("task-var", St::Done),
                ("dopagaki", St::Seen),
            ]
            .iter()
            .enumerate()
            .map(|(i, (s, st))| Row { pane: format!("%{i}"), label: s.to_string(), st: *st })
            .collect::<Vec<_>>();
            let clawd = ClawdView { rows: &clawd_rows, phase: false };
            bar.draw(&mut buf, &st, Some(&view), Some(&clawd));
            let dump = |buf: &[u8], to: &str| {
                let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
                for p in buf.chunks_exact(4) {
                    ppm.extend_from_slice(&[p[2], p[1], p[0]]); // BGRA → RGB
                }
                std::fs::write(to, ppm).unwrap();
            };
            dump(&buf, &path);
            // シャッフル OFF / 1 曲リピート / 停止中、走りアニメの逆位相
            let off = NpView {
                np: &np,
                player: PlayerState { playing: false, shuffle: false, repeat: Loop::Track },
                art: art.as_deref(),
                accent: art.as_deref().map(crate::art::accent).unwrap_or_default(),
            };
            bar.draw(&mut buf, &st, Some(&off), Some(&ClawdView { rows: &clawd_rows, phase: true }));
            dump(&buf, &format!("{path}.off"));
        }
    }

    #[test]
    fn empty_panel_keeps_the_frame_and_the_spotify_icon() {
        let (w, h) = (1366u32, 96u32);
        let bar = Bar::new(w, h).unwrap();
        let mut buf = vec![0u8; (w * h * 4) as usize];
        bar.draw(&mut buf, &state("spotify", &["spotify"]), None, None);
        let px = |x: u32, y: u32| -> [u8; 3] {
            let off = ((y * w + x) * 4) as usize;
            [buf[off], buf[off + 1], buf[off + 2]]
        };
        let p = bar.np_rect();

        // 枠は出る。左端は既定 accent(Spotify グリーン)、右端は素のパネル色
        let left = px(p.x + 3, p.y + p.h / 2);
        let want = Accent::default().fill;
        assert!(
            left.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 12),
            "左端が既定 accent になっていない: {left:?} vs {want:?}"
        );
        assert_eq!(px(p.x + p.w - 6, p.y + p.h - 4), PANEL, "右端がパネル色に落ちていない");

        // アルバムアートの場所には Spotify のアイコン(下地でないピクセルがある)
        let a = bar.art_side();
        let art = bar.np.art;
        assert_eq!(a, art.w);
        assert!(
            (art.x..art.x + art.w).any(|x| (art.y..art.y + art.h).any(|y| px(x, y) != ART_BG)),
            "アルバム画像の場所にアイコンが描かれていない"
        );

        // 曲名の列とボタンの場所は空。下地はグラデーションなので一色ではないが、
        // 文字やグリフのような明るいピクセルは 1 つも無い。
        let l = &bar.np;
        let dark = |x: u32, y: u32| px(x, y).iter().all(|&v| v < 150);
        assert!(
            (l.col2_x..l.col2_x + l.col2_w).all(|x| (p.y + 4..p.y + p.h - 4).all(|y| dark(x, y))),
            "枠だけのはずが曲名の列に何か描かれている"
        );
        // ボタンの場所はほぼ素のパネル色(グラデーションの残りぶんだけ僅かに寄る)
        let btn = l.btn_xs[2] + l.btn_d[2] / 2;
        let at_btn = px(btn, l.btn_y[2] + l.btn_d[2] / 2);
        assert!(
            at_btn.iter().zip(PANEL).all(|(a, b)| a.abs_diff(b) <= 8),
            "ボタンが描かれている: {at_btn:?}"
        );
        let prog = l.prog;
        assert_ne!(px(prog.x + 2, prog.y + prog.h / 2), GREEN, "進捗バーが描かれている");

        // ボタンが無くても、パネルを押せば spotatui へ遷移する
        let (cx, cy) = ((p.x + p.w / 2) as f64, (p.y + p.h / 2) as f64);
        assert_eq!(bar.hit(cx, cy, false, 0), Some(Hit::Panel));
    }

    #[test]
    fn clawd_frame_sits_left_of_the_icons() {
        let bar = Bar::new(1366, 96).unwrap();
        assert_eq!(bar.clawd_cells(), CLAWD_ROWS * CLAWD_COLS, "既定は 2 行 2 列");
        let l = bar.clawd.as_ref().unwrap();

        // 左から clawd 枠・アイコン列・再生情報パネルの順に並ぶ
        assert_eq!(l.panel.w, bar.np_rect().w, "枠の幅がパネルと揃っていない");
        // バー端からの余白も左右で同じ(枠の左端 = パネルの右端から数えた距離)
        let np = bar.np_rect();
        assert_eq!(l.panel.x, bar.w - (np.x + np.w), "左右の余白が違う");
        assert_eq!(l.panel.y, np.y, "上の余白が違う");
        assert_eq!(l.panel.h, np.h, "高さが違う");
        assert!(l.panel.x + l.panel.w <= bar.xs[0], "アイコン列に重なっている");
        let icons_right = bar.xs[ICONS.len() - 1] + bar.tile_d;
        assert!(icons_right <= bar.np_rect().x, "アイコン列が再生情報パネルに重なっている");
        assert_eq!(bar.clawd_rect(), Some(l.panel));

        // セルはすべて枠の内側。上下の余りは均等に散る
        for (i, r) in l.cells.iter().enumerate() {
            assert!(r.y >= l.panel.y && r.y + r.h <= l.panel.y + l.panel.h, "{i} が縦にはみ出す");
            assert!(r.x >= l.panel.x && r.x + r.w <= l.panel.x + l.panel.w, "{i} が横にはみ出す");
            assert!(l.sprite_w + CLAWD_NAME_GAP < r.w, "{i} に見出しの場所が無い");
        }
        // 左上 → 左下 → 右上 → 右下 の順(左の列から縦に埋める)
        assert_eq!(l.cells[0].x, l.cells[1].x, "1 列目の x が揃っていない");
        assert_eq!(l.cells[1].y, l.cells[0].y + l.cells[0].h, "2 個目が真下に来ていない");
        assert_eq!(l.cells[0].y, l.cells[CLAWD_ROWS].y, "1 行目の高さが揃っていない");
        assert_eq!(
            l.cells[CLAWD_ROWS].x - (l.cells[0].x + l.cells[0].w),
            CLAWD_COL_GAP,
            "列間が CLAWD_COL_GAP になっていない"
        );

        // 見出しは 20px 固定(行高にも行数にも連動しない)
        assert_eq!(l.name_px, CLAWD_NAME_PX, "既定の文字サイズが変わっている");
        assert_eq!(ClawdLayout::new(64, 12, 400).unwrap().name_px, CLAWD_NAME_PX, "バーが低くても同じ");

        let above = l.cells[0].y - l.panel.y;
        let last = l.cells[CLAWD_ROWS - 1]; // 1 列目の一番下
        let below = (l.panel.y + l.panel.h) - (last.y + last.h);
        assert!(above.abs_diff(below) <= 1, "上下の余白が揃っていない: 上 {above} / 下 {below}");

        // 左右の余りが揃う(枠とパネルが同じ幅で、アイコン列が中央にあるため)
        let left_gap = bar.xs[0] - (l.panel.x + l.panel.w);
        let right_gap = bar.np_rect().x - icons_right;
        assert!(left_gap.abs_diff(right_gap) <= 1, "左右非対称: 左 {left_gap} / 右 {right_gap}");

        // パネルが細って場所が無くなれば枠ごと諦める
        assert!(ClawdLayout::new(96, 24, CLAWD_MIN_W - 1).is_none(), "狭くても枠を出す");
    }

    #[test]
    fn clawd_cells_draw_and_hit() {
        let (w, h) = (1366u32, 96u32);
        let bar = Bar::new(w, h).unwrap();
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let st = state("main", &["main"]);
        let rows = vec![
            Row { pane: "%1".into(), label: "main".into(), st: St::Run },
            Row { pane: "%2".into(), label: "task-var".into(), st: St::Done },
        ];
        bar.draw(&mut buf, &st, None, Some(&ClawdView { rows: &rows, phase: false }));

        let px = |buf: &[u8], x: u32, y: u32| -> [u8; 3] {
            let off = ((y * w + x) * 4) as usize;
            [buf[off], buf[off + 1], buf[off + 2]]
        };
        let l = bar.clawd.as_ref().unwrap();
        let sprite = bar.sprite.as_ref().unwrap();

        // 枠は持たない。キャラも見出しも無い場所は地色のまま
        assert_eq!(px(&buf, l.panel.x + 1, l.panel.y + 1), BG, "枠の下地が残っている");
        assert_eq!(px(&buf, l.panel.x + l.panel.w - 6, l.panel.y + l.panel.h - 4), BG);

        // 各行のキャラが状態の色で塗られている
        let has = |buf: &[u8], r: Rect, c: [u8; 3]| {
            (0..l.sprite_w).any(|i| (0..r.h).any(|j| px(buf, r.x + i, r.y + j) == c))
        };
        let r0 = l.cells[0];
        assert!(has(&buf, l.cells[0], sprite.body), "処理中のキャラが元のオレンジで描かれていない");
        assert!(has(&buf, l.cells[1], CLAWD_DONE), "終了のキャラが黄で描かれていない");
        // 描いていない 3 つ目(2 段目の左)は地色のまま
        assert!(!has(&buf, l.cells[2], sprite.body) && !has(&buf, l.cells[2], CLAWD_DONE));

        // キャラの右下には薄い影(地色より暗いが、キャラ本体ほど濃くない)
        let shaded = (0..l.sprite_w + CLAWD_SHADOW_DX).any(|i| {
            (0..r0.h).any(|j| {
                let c = px(&buf, r0.x + i, r0.y + j);
                c != BG && c.iter().zip(BG).all(|(a, b)| *a < b)
            })
        });
        assert!(shaded, "キャラの影が描かれていない");

        // 見出しはキャラの右に黒で出る(地色でないピクセルがある)
        let r = l.cells[0];
        let name_x = r.x + l.sprite_w + CLAWD_NAME_GAP;
        assert!(
            (name_x..r.x + r.w).any(|x| (r.y..r.y + r.h).any(|y| px(&buf, x, y) != BG)),
            "見出しが描かれていない(フォントが無い環境かもしれない)"
        );

        // 当たり判定: 描いた 2 つだけが当たる(0 = 左上、1 = 右上)
        let center = |r: Rect| ((r.x + r.w / 2) as f64, (r.y + r.h / 2) as f64);
        let (x0, y0) = center(l.cells[0]);
        assert_eq!(bar.hit(x0, y0, false, rows.len()), Some(Hit::Clawd(0)));
        let (x1, y1) = center(l.cells[1]);
        assert_eq!(bar.hit(x1, y1, false, rows.len()), Some(Hit::Clawd(1)));
        let (x2, y2) = center(l.cells[2]);
        assert_eq!(bar.hit(x2, y2, false, rows.len()), None, "描いていないセルは当たらない");
        assert_eq!(bar.hit(x0, y0, false, 0), None, "枠が空なら当たらない");

        // 1 つも無ければ場所ごと地色のまま(枠を持たないので何も残らない)。
        // 場所は `ClawdLayout` が確保したままなのでタップ先はずれない
        let mut empty = vec![0u8; (w * h * 4) as usize];
        bar.draw(&mut empty, &st, None, Some(&ClawdView { rows: &[], phase: false }));
        assert!(
            (l.panel.x..l.panel.x + l.panel.w)
                .all(|x| (l.panel.y..l.panel.y + l.panel.h).all(|y| px(&empty, x, y) == BG)),
            "1 つも無いのに何か描かれている"
        );
    }

    #[test]
    fn the_label_takes_the_empty_column_next_to_it() {
        let (w, h) = (1366u32, 96u32);
        let bar = Bar::new(w, h).unwrap();
        let l = bar.clawd.as_ref().unwrap();
        let st = state("main", &["main"]);
        // 1 セルの幅には到底入らない見出し
        let row = |i: usize, s: St| Row {
            pane: format!("%{i}"),
            label: "task-var の clawd 枠を 2 行 2 列に並べ替える".into(),
            st: s,
        };
        let draw = |rows: &[Row]| {
            let mut buf = vec![0u8; (w * h * 4) as usize];
            bar.draw(&mut buf, &st, None, Some(&ClawdView { rows, phase: false }));
            buf
        };
        // その段(セルの高さ)の x0..x1 に何か描かれているか
        let ink = |buf: &[u8], x0: u32, x1: u32, cell: Rect| {
            (x0..x1).any(|x| {
                (cell.y..cell.y + cell.h).any(|y| {
                    let off = ((y * w + x) * 4) as usize;
                    [buf[off], buf[off + 1], buf[off + 2]] != BG
                })
            })
        };
        // 列の境(列間の隙間)と、右の列で見出しが来る辺り
        let (top, bottom) = (l.cells[0], l.cells[1]);
        let (gap_x0, gap_x1) = (top.x + top.w, l.cells[CLAWD_ROWS].x);
        let right_text = l.cells[CLAWD_ROWS].x + l.sprite_w + CLAWD_NAME_GAP;
        let right_end = l.panel.x + l.panel.w - CLAWD_PAD_X;

        // 2 個までは右の列が空くので、見出しはそこも使って枠の右端で切る
        let two = draw(&[row(1, St::Run), row(2, St::Done)]);
        assert!(ink(&two, gap_x0, gap_x1, top), "列の境で切れている(見出しが伸びていない)");
        assert!(ink(&two, right_text, right_end, top), "右の列まで届いていない");
        assert!(ink(&two, right_text, right_end, bottom), "下の段が伸びていない");

        // 右上にキャラが入ったら、その段の見出しは自分のセルの中で `…` に詰める。
        // 右下はまだ空なので、下の段は伸びたまま
        let three = draw(&[row(1, St::Run), row(2, St::Done), row(3, St::Ask)]);
        assert!(!ink(&three, gap_x0, gap_x1, top), "右の列が埋まったのに見出しがはみ出している");
        assert!(ink(&three, right_text, right_end, bottom), "空いている右下まで伸びていない");

        // 4 個そろえば、どの段も自分のセルの中で切れる
        let four = draw(&[row(1, St::Run), row(2, St::Done), row(3, St::Ask), row(4, St::Seen)]);
        assert!(!ink(&four, gap_x0, gap_x1, top), "上の段がはみ出している");
        assert!(!ink(&four, gap_x0, gap_x1, bottom), "下の段がはみ出している");
    }

    #[test]
    fn shadow_env_clamps_and_falls_back() {
        // 濃さ: 0 は「影なし」として通す(env_f32 だと既定へ落ちてしまう値)
        assert_eq!(shadow_a_of(Some("0")), 0.0, "0 は影なしとして効く");
        assert_eq!(shadow_a_of(Some(" 0.4 ")), 0.4, "前後の空白は無視する");
        assert_eq!(shadow_a_of(Some("2")), 1.0, "1 より濃くはならない");
        for bad in [None, Some("-1"), Some("abc"), Some("")] {
            assert_eq!(shadow_a_of(bad), SHADOW_A, "{bad:?} は既定へ落ちる");
        }
        // ぼかし幅: 0 除算を避けるため下限を持つ
        assert_eq!(shadow_blur_of(Some("12")), 12.0);
        assert_eq!(shadow_blur_of(Some("0")), 0.5, "0 は下限まで");
        assert_eq!(shadow_blur_of(Some("999")), 64.0, "上限で頭打ち");
        for bad in [None, Some("abc"), Some("")] {
            assert_eq!(shadow_blur_of(bad), SHADOW_BLUR, "{bad:?} は既定へ落ちる");
        }
    }

    #[test]
    fn env_f32_parses_or_falls_back() {
        // 他のテストと衝突しないよう専用の変数名を使う
        const K: &str = "TASKVAR_TEST_ENV_F32";
        std::env::set_var(K, "13.5");
        assert_eq!(env_f32(K, 1.0), 13.5);
        std::env::set_var(K, " 20 ");
        assert_eq!(env_f32(K, 1.0), 20.0, "前後の空白は無視する");
        for bad in ["0", "-3", "abc", ""] {
            std::env::set_var(K, bad);
            assert_eq!(env_f32(K, 7.0), 7.0, "{bad:?} は既定へ落ちる");
        }
        std::env::remove_var(K);
        assert_eq!(env_f32(K, 7.0), 7.0);
    }

    #[test]
    fn controls_sit_centred_and_inside_the_panel() {
        let l = NpLayout::new(1366, 96, MARGIN, 400);
        let (top, bottom) = (l.panel.y, l.panel.y + l.panel.h);
        for (i, &d) in l.btn_d.iter().enumerate() {
            assert!(l.btn_y[i] >= top, "ボタン {i} がパネルの上へ出ている");
            assert!(l.btn_y[i] + d <= l.prog.y, "ボタン {i} が進捗バーへ食い込んでいる");
        }
        assert!(l.prog.y + l.prog.h <= bottom, "進捗バーがパネルの外へ出ている");

        // ボタンと進捗バーはひとまとまりでパネルの縦中央に来る(誤差は丸めぶんのみ)
        let group_top = *l.btn_y.iter().min().unwrap();
        let slack_above = group_top - top;
        let slack_below = bottom - (l.prog.y + l.prog.h);
        assert!(
            slack_above.abs_diff(slack_below) <= 1,
            "上下の余白が揃っていない: 上 {slack_above} / 下 {slack_below}"
        );
        // 進捗バーは端のボタンにぴったり揃う
        assert_eq!(l.prog.x, l.btn_xs[0], "左端がボタン列と合っていない");
        assert_eq!(l.prog.x + l.prog.w, l.btn_xs[4] + l.btn_d[4], "右端が合っていない");
    }

    #[test]
    fn text_width_drives_the_panel_when_given() {
        // 列②に 300px を指定 → その幅ちょうどになるパネル幅が返る
        let want = PAD * 2 + 72 + GAP + 300 + GAP + 200;
        assert_eq!(panel_width(2000, 560, Some(300), 72, 200), want);
        // 実際にレイアウトへ通しても列②は 300px
        let l = NpLayout::new(1366, 96, MARGIN, 400);
        // 既定では列②はパネル幅の余り(定数から算出するので既定値を変えても追従する)
        let col3 = BTN_D_DEFAULT.iter().sum::<u32>() + BTN_GAP * 4;
        assert_eq!(l.col2_w, 400 - PAD * 2 - 72 - GAP - GAP - col3);

        // 使える幅に収まらなければそこで頭打ち
        assert_eq!(panel_width(400, 560, Some(300), 72, 200), 400);
        // 未指定ならパネル幅が主(こちらも avail で頭打ち)
        assert_eq!(panel_width(2000, 560, None, 72, 200), 560);
        assert_eq!(panel_width(400, 560, None, 72, 200), 400);
    }

    #[test]
    fn panel_never_overlaps_the_icons() {
        // 使える幅が減っても、右端を侵食せず幅だけ縮む
        // (env は他のテストと共有されるのでレイアウトを直接組んで確かめる)
        for avail in [160u32, 300, 800] {
            let l = NpLayout::new(1366, 96, MARGIN, avail);
            assert!(l.panel.w <= avail, "avail={avail} で使える幅を超えている");
            assert_eq!(l.panel.x + l.panel.w, 1366 - MARGIN, "avail={avail} で右端がずれた");
            assert!(l.panel.x + l.panel.w <= 1366, "avail={avail} で画面外");
            // 列②が潰れても他の列は成立したまま
            // ボタンと進捗バーはパネルの内側に収まる
            let panel_right = l.panel.x + l.panel.w;
            assert!(l.btn_xs[4] + l.btn_d[4] + PAD <= panel_right, "ボタンがはみ出している");
            assert!(l.prog.x + l.prog.w + PAD <= panel_right, "進捗バーがはみ出している");
        }
    }
}
