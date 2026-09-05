//! タスクバーのレイアウト・描画・当たり判定。
//!
//! バーは画面下部の全幅の帯(BGRA バッファ)。左側にセッション切替の白円アイコンを
//! 横一列、右側に Spotify の再生情報パネルを置く。
//! 外枠リング: 表示中セッション=青 / 存在するが非表示=灰 / セッション無し=枠なし。
//! tmux アイコンはセッションに対応しないため、「名前付きセッション以外を表示中」の
//! ときに青リングにする(通常の作業セッションにいる状態を表す)。
//!
//! 再生情報パネルは 2 行 3 列:
//!   列① アルバムアート(2 行ぶち抜き) / 列② 曲名・アーティスト /
//!   列③ 操作ボタン 5 個・進捗バー
//! アイコン列は左寄せ固定。中央寄せだとパネルに必要な幅(既定 560px)が
//! 取れず、曲名が数文字で切れてしまうため。

use crate::actions::{IconDef, ICONS};
use crate::icons::{self, Glyph};
use crate::mpris::{Ctrl, Loop, PlayerState};
use crate::np::NowPlaying;
use crate::text::Font;
use crate::tmux::State;
use anyhow::Result;

// 色はすべて BGR 順(バッファが BGRA のため)。
const BG: [u8; 3] = [0, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];
const BLUE: [u8; 3] = [255, 144, 30]; // #1E90FF
const GRAY: [u8; 3] = [128, 128, 128];
/// リングの太さ(px)。
const RING_W: f32 = 4.0;

// Spotify のブランドカラー。
const PANEL: [u8; 3] = [0x12, 0x12, 0x12]; // #121212 ベース
const PANEL_EDGE: [u8; 3] = [0x28, 0x28, 0x28]; // #282828 枠線
const GREEN: [u8; 3] = [0x60, 0xD7, 0x1E]; // #1ED760 アクセント
const SUB_TEXT: [u8; 3] = [0xB3, 0xB3, 0xB3]; // #B3B3B3 アーティスト名
const GROOVE: [u8; 3] = [0x53, 0x53, 0x53]; // #535353 進捗バーの溝
const ART_BG: [u8; 3] = [0x28, 0x28, 0x28]; // アート未取得時の下地

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

/// 再生/停止グリフは白円の中に入れるので、他より小さく描く。
const PLAY_GLYPH_PCT: u32 = 58;

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// 指定があるときだけ Some。既定を持たない上書き用の env var に使う。
fn env_opt_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).filter(|v| *v > 0)
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
    col3_x: u32,
    col3_w: u32,
    row1_y: u32,
    row1_h: u32,
    row2_y: u32,
    row2_h: u32,
    btn_d: u32,
    btn_xs: [u32; 5],
    btn_y: u32,
    prog: Rect,
    title_px: f32,
    artist_px: f32,
}

const MARGIN: u32 = 12; // 画面右端からの余白
const INSET: u32 = 4; // バー上下からの余白
const PAD: u32 = 8; // パネル内側の余白
const GAP: u32 = 12; // 列間
const BTN_GAP: u32 = 10;

/// パネルの幅を決める。
///
/// 既定はパネル幅(`np_w`)が主で、列②(曲名・アーティスト)はその余りを取る。
/// `text_w` が指定されたときは列②を主にし、パネル幅の方を逆算する。
/// どちらの場合も、アイコン列に重ならない範囲 `avail` で頭打ちにする。
fn panel_width(avail: u32, np_w: u32, text_w: Option<u32>, art: u32, col3: u32) -> u32 {
    match text_w {
        Some(t) => PAD * 2 + art + GAP + t + GAP + col3,
        None => np_w,
    }
    .min(avail)
}

impl NpLayout {
    /// バー幅 w・高さ h と、アイコン列の右端 icons_right から算出する。
    /// パネルがアイコンに重ならないよう幅を切り詰める。
    fn new(w: u32, h: u32, icons_right: u32) -> Self {
        let panel_h = h.saturating_sub(INSET * 2);
        let content_h = panel_h - PAD * 2;
        // 行の分割は content_h だけで決まる(上段=曲名/ボタン、下段=アーティスト/進捗バー)。
        let row1_h = content_h / 2 + 2;
        let row2_h = content_h - row1_h;

        // 列①のアルバムアートは行をぶち抜く正方形、列③はボタン 5 個ぶん。
        // この 2 つは先に決まるので、パネル幅はそこから逆算できる。
        // ボタンは自分の行に収まる大きさまで(2x3 グリッドの升目をはみ出さない)。
        let want_btn = env_u32("TASKVAR_BTN_D", 32).min(row1_h);
        let want_col3 = want_btn * 5 + BTN_GAP * 4;
        let avail = w.saturating_sub(MARGIN).saturating_sub(icons_right + GAP);
        let panel_w = panel_width(
            avail,
            env_u32("TASKVAR_NP_W", 560),
            env_opt_u32("TASKVAR_TEXT_W"),
            content_h,
            want_col3,
        );
        let panel = Rect { x: w - MARGIN - panel_w, y: INSET, w: panel_w, h: panel_h };

        let content_x = panel.x + PAD;
        let content_y = panel.y + PAD;
        let content_w = panel_w - PAD * 2;

        let art_side = content_h.min(content_w);
        let art = Rect { x: content_x, y: content_y, w: art_side, h: art_side };

        // 列②③はアートの右の残り。パネルが狭いときは列③(ボタン)を先に確保し、
        // ボタンも入らないほど狭ければボタン自体を縮める。列②は最後に余りを取る。
        let rest = content_w.saturating_sub(art.w + GAP);
        let col3_w = want_col3.min(rest.saturating_sub(GAP));
        let btn_d = want_btn.min(col3_w.saturating_sub(BTN_GAP * 4) / 5);
        let col2_w = rest.saturating_sub(GAP + col3_w);
        let col2_x = art.x + art.w + GAP;
        let col3_x = col2_x + col2_w + GAP;
        let (row1_y, row2_y) = (content_y, content_y + row1_h);

        let btn_y = row1_y + (row1_h.saturating_sub(btn_d)) / 2;
        let mut btn_xs = [0u32; 5];
        for (i, x) in btn_xs.iter_mut().enumerate() {
            *x = col3_x + i as u32 * (btn_d + BTN_GAP);
        }

        // 進捗バーはボタン列と同じ幅で、下段の行の中央に置く。
        // 高さは env `TASKVAR_PROG_H` で調整できる(こちらも下段の行に収める)。
        let prog_h = env_u32("TASKVAR_PROG_H", (content_h / 14).clamp(4, 8)).clamp(2, row2_h);
        let prog =
            Rect { x: col3_x, y: row2_y + (row2_h.saturating_sub(prog_h)) / 2, w: col3_w, h: prog_h };

        Self {
            panel,
            art,
            col2_x,
            col2_w,
            col3_x,
            col3_w,
            row1_y,
            row1_h,
            row2_y,
            row2_h,
            btn_d,
            btn_xs,
            btn_y,
            prog,
            // 既定はパネル高に比例させる。係数は 1366x768(バー高 96px →
            // content_h 72px)でちょうど曲名 30px・アーティスト 22px になる値。
            // 一時的に変えたいときは TASKVAR_TITLE_PX / TASKVAR_ARTIST_PX で
            // 上書きできる(行に収まる範囲へ丸めるので両行は重ならない)。
            title_px: env_f32("TASKVAR_TITLE_PX", (content_h as f32 * 0.4167).clamp(12.0, 30.0))
                .clamp(6.0, row1_h as f32),
            artist_px: env_f32("TASKVAR_ARTIST_PX", (content_h as f32 * 0.3056).clamp(10.0, 22.0))
                .clamp(6.0, row2_h as f32),
        }
    }

    /// 列③を 5 等分した当たり判定スロット(縦はパネル全高)。
    /// 見た目のボタン(既定 32px)より広く取って指で押しやすくする。
    fn ctrl_slot(&self, i: usize) -> Rect {
        let slot_w = self.col3_w / 5;
        Rect { x: self.col3_x + i as u32 * slot_w, y: self.panel.y, w: slot_w, h: self.panel.h }
    }
}

/// パネルに描く内容。main が毎ティック組み立てる。
pub struct NpView<'a> {
    pub np: &'a NowPlaying,
    pub player: PlayerState,
    /// アルバムアート(art 一辺の正方形・BGRA)。未取得なら None。
    pub art: Option<&'a [u8]>,
}

/// タップが当たった対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Icon(usize),
    Ctrl(Ctrl),
}

pub struct Bar {
    pub w: u32,
    h: u32,
    circle_d: u32,
    /// 各アイコンタイルの左上 x(y はバー内で垂直センタリング)。
    xs: Vec<u32>,
    tile_y: u32,
    glyphs: Vec<Glyph>,
    ctrl: Vec<Glyph>,
    art_fallback: Glyph,
    font: Option<Font>,
    np: NpLayout,
}

impl Bar {
    pub fn new(w: u32, h: u32) -> Result<Self> {
        let circle_d = env_u32("TASKVAR_ICON_D", 64).clamp(16, h.saturating_sub(8).max(16));
        let gap = env_u32("TASKVAR_GAP", 24);
        let n = ICONS.len() as u32;
        let total = n * circle_d + (n - 1) * gap;
        // アイコン列は左寄せ固定(右側の再生情報パネルに幅を譲るため)。
        let x0 = env_u32("TASKVAR_MARGIN", 24).min(w.saturating_sub(total));
        let xs: Vec<u32> = (0..n).map(|i| x0 + i * (circle_d + gap)).collect();
        let tile_y = (h - circle_d) / 2;
        let glyph_px = circle_d * 58 / 100;
        let glyphs = ICONS.iter().map(|d| icons::render(d.svg, glyph_px)).collect::<Result<_>>()?;

        let np = NpLayout::new(w, h, x0 + total);
        let ctrl = CTRL_SVGS
            .iter()
            .enumerate()
            .map(|(i, svg)| {
                let px = if i == G_PLAY || i == G_PAUSE {
                    np.btn_d * PLAY_GLYPH_PCT / 100
                } else {
                    np.btn_d
                };
                icons::render(svg, px.max(1))
            })
            .collect::<Result<_>>()?;
        let art_fallback = icons::render(ICONS[1].svg, (np.art.w * 55 / 100).max(1))?;

        Ok(Self {
            w,
            h,
            circle_d,
            xs,
            tile_y,
            glyphs,
            ctrl,
            art_fallback,
            font: Font::load(),
            np,
        })
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
    pub fn draw(&self, buf: &mut [u8], state: &State, np: Option<&NpView>) {
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&[BG[0], BG[1], BG[2], 0]);
        }
        for (i, def) in ICONS.iter().enumerate() {
            self.draw_tile(buf, self.xs[i], ring_color(def, state), &self.glyphs[i]);
        }
        if let Some(view) = np {
            self.draw_np(buf, view);
        }
    }

    /// 白円 + リング + グリフを 1 タイルぶん描く。円境界は 1px の線形カバレッジで滑らかに。
    fn draw_tile(&self, buf: &mut [u8], x0: u32, ring: Option<[u8; 3]>, glyph: &Glyph) {
        let d = self.circle_d;
        let r_out = d as f32 / 2.0;
        let r_in = r_out - RING_W;
        let c = d as f32 / 2.0;
        let ring_c = ring.unwrap_or(BG);
        for j in 0..d {
            for i in 0..d {
                let dx = i as f32 + 0.5 - c;
                let dy = j as f32 + 0.5 - c;
                let dist = (dx * dx + dy * dy).sqrt();
                let cov_out = (r_out - dist + 0.5).clamp(0.0, 1.0);
                let cov_in = (r_in - dist + 0.5).clamp(0.0, 1.0);
                if cov_out <= 0.0 {
                    continue; // タイル外周はバー背景のまま
                }
                let mut px = [0u8; 3];
                for k in 0..3 {
                    let v = BG[k] as f32 * (1.0 - cov_out)
                        + ring_c[k] as f32 * (cov_out - cov_in)
                        + WHITE[k] as f32 * cov_in;
                    px[k] = v.round() as u8;
                }
                let off = (((self.tile_y + j) * self.w + x0 + i) * 4) as usize;
                buf[off..off + 3].copy_from_slice(&px);
                buf[off + 3] = 0;
            }
        }
        premul_glyph(buf, self.w, x0 + (d - glyph.px) / 2, self.tile_y + (d - glyph.px) / 2, glyph);
    }

    /// 再生情報パネルを描く。
    fn draw_np(&self, buf: &mut [u8], view: &NpView) {
        let l = &self.np;
        // パネル: 枠線の角丸矩形の内側を 1px 詰めて塗る
        round_rect(buf, self.w, self.h, l.panel, 10.0, PANEL_EDGE);
        let inner =
            Rect { x: l.panel.x + 1, y: l.panel.y + 1, w: l.panel.w - 2, h: l.panel.h - 2 };
        round_rect(buf, self.w, self.h, inner, 9.0, PANEL);

        // 列①: アルバムアート(角は 6px の丸め。パネル色へブレンドして落とす)
        match view.art {
            Some(bgra) if bgra.len() >= (l.art.w * l.art.h * 4) as usize => {
                blit_round(buf, self.w, self.h, l.art, bgra, 6.0, PANEL);
            }
            _ => {
                round_rect(buf, self.w, self.h, l.art, 6.0, ART_BG);
                let g = &self.art_fallback;
                premul_glyph(
                    buf,
                    self.w,
                    l.art.x + (l.art.w - g.px) / 2,
                    l.art.y + (l.art.h - g.px) / 2,
                    g,
                );
            }
        }

        // 列②: 曲名 / アーティスト名
        if let Some(font) = &self.font {
            let max = l.col2_w as f32;
            if max > 4.0 {
                let title = font.fit(&view.np.track, max, l.title_px);
                let base1 = l.row1_y as f32 + l.row1_h as f32 / 2.0 + l.title_px * 0.35;
                font.draw(buf, self.w, self.h, l.col2_x as f32, base1, l.title_px, WHITE, &title);

                let artist = font.fit(&view.np.artist, max, l.artist_px);
                let base2 = l.row2_y as f32 + l.row2_h as f32 / 2.0 + l.artist_px * 0.35;
                font.draw(
                    buf,
                    self.w,
                    self.h,
                    l.col2_x as f32,
                    base2,
                    l.artist_px,
                    SUB_TEXT,
                    &artist,
                );
            }
        }

        // 列③ row1: 操作ボタン
        for (i, ctrl) in CTRLS.iter().enumerate() {
            self.draw_ctrl(buf, *ctrl, l.btn_xs[i], l.btn_y, &view.player);
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
    fn draw_ctrl(&self, buf: &mut [u8], ctrl: Ctrl, x: u32, y: u32, p: &PlayerState) {
        let d = self.np.btn_d;
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
                let off = (d - g.px) / 2;
                tint_glyph(buf, self.w, self.h, x + off, y + off, g, BG);
                return;
            }
        };
        let g = &self.ctrl[gi];
        let off = (d.saturating_sub(g.px)) / 2;
        tint_glyph(buf, self.w, self.h, x + off, y + off, g, color);
    }

    /// バーローカル座標 (lx,ly) が何に当たるか。`np_shown` が false のときは
    /// パネルを描いていないので操作ボタンの判定を飛ばす。
    pub fn hit(&self, lx: f64, ly: f64, np_shown: bool) -> Option<Hit> {
        if np_shown {
            for (i, ctrl) in CTRLS.iter().enumerate() {
                if self.np.ctrl_slot(i).contains(lx, ly) {
                    return Some(Hit::Ctrl(*ctrl));
                }
            }
        }
        // 円の少し外までタッチを許容する。
        let r = self.circle_d as f64 / 2.0 + 8.0;
        for (i, &x0) in self.xs.iter().enumerate() {
            let cx = x0 as f64 + self.circle_d as f64 / 2.0;
            let cy = self.tile_y as f64 + self.circle_d as f64 / 2.0;
            let (dx, dy) = (lx - cx, ly - cy);
            if dx * dx + dy * dy <= r * r {
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

/// 角丸矩形を単色で塗る(角は 1px の線形カバレッジ)。
fn round_rect(buf: &mut [u8], w: u32, h: u32, rect: Rect, r: f32, color: [u8; 3]) {
    let r = r.min(rect.w as f32 / 2.0).min(rect.h as f32 / 2.0).max(0.0);
    for j in 0..rect.h {
        for i in 0..rect.w {
            blend(buf, w, h, rect.x + i, rect.y + j, color, round_cov(i, j, rect, r));
        }
    }
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

/// BGRA 画像を角丸で貼る。角の外は `bg`(パネル色)へ落とす。
fn blit_round(buf: &mut [u8], w: u32, h: u32, rect: Rect, src: &[u8], r: f32, bg: [u8; 3]) {
    for j in 0..rect.h {
        for i in 0..rect.w {
            let s = ((j * rect.w + i) * 4) as usize;
            let cov = round_cov(i, j, rect, r);
            let px = [
                (src[s] as f32 * cov + bg[0] as f32 * (1.0 - cov)) as u8,
                (src[s + 1] as f32 * cov + bg[1] as f32 * (1.0 - cov)) as u8,
                (src[s + 2] as f32 * cov + bg[2] as f32 * (1.0 - cov)) as u8,
            ];
            blend(buf, w, h, rect.x + i, rect.y + j, px, 1.0);
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

/// アイコンごとのリング色(バー描画とテストの両方から使う)。
fn ring_color(def: &IconDef, state: &State) -> Option<[u8; 3]> {
    match def.session {
        Some(sess) => {
            if state.current.as_deref() == Some(sess) {
                Some(BLUE)
            } else if state.existing.iter().any(|s| s == sess) {
                Some(GRAY)
            } else {
                None
            }
        }
        // tmux アイコン: 名前付きセッション以外(=通常の作業セッション)を表示中なら青
        None => {
            let named: Vec<&str> = ICONS.iter().filter_map(|d| d.session).collect();
            match state.current.as_deref() {
                Some(cur) if !named.contains(&cur) => Some(BLUE),
                _ => None,
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
    fn ring_colors_follow_session_state() {
        // ICONS: [tmux, spotify, shorts, bluetooth, ssbrowse, eduroam, calendar]
        let st = state("spotify", &["spotify", "bluetooth"]);
        assert_eq!(ring_color(&ICONS[1], &st), Some(BLUE), "表示中は青");
        assert_eq!(ring_color(&ICONS[3], &st), Some(GRAY), "存在するが非表示は灰");
        assert_eq!(ring_color(&ICONS[2], &st), None, "セッション無しは枠なし");
        assert_eq!(ring_color(&ICONS[0], &st), None, "名前付きセッション表示中のtmuxは枠なし");
        let st2 = state("main", &["main", "spotify"]);
        assert_eq!(ring_color(&ICONS[0], &st2), Some(BLUE), "通常セッション表示中のtmuxは青");
    }

    #[test]
    fn draw_and_hit() {
        // 実機と同じ 1366x96(fbterm のセル高 16px にスナップされた値)
        let (w, h) = (1366u32, 96u32);
        let bar = Bar::new(w, h).unwrap();
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let st = state("spotify", &["spotify"]);
        let np = now_playing();
        let view = NpView {
            np: &np,
            player: PlayerState { playing: true, shuffle: true, repeat: Loop::Playlist },
            art: None,
        };
        bar.draw(&mut buf, &st, Some(&view));

        let px = |buf: &[u8], x: u32, y: u32| -> [u8; 3] {
            let off = ((y * w + x) * 4) as usize;
            [buf[off], buf[off + 1], buf[off + 2]]
        };
        let (cx, cy) = (bar.xs[1] + bar.circle_d / 2, bar.tile_y + bar.circle_d / 2);
        // 白円内・グリフ外の点は白(グリフはd*58%なので中心から±d*0.29まで)
        assert_eq!(px(&buf, cx + bar.circle_d * 38 / 100, cy), WHITE);
        // リング帯(半径 d/2 - RING_W/2 付近)は spotify=表示中 → 青
        assert_eq!(px(&buf, cx + bar.circle_d / 2 - 2, cy), BLUE);

        // アイコン列は左寄せ。1 個目の左端が既定マージンにある
        assert_eq!(bar.xs[0], 24, "アイコン列は左寄せ");
        // アイコン列とパネルの間は素通しの背景
        let icons_right = bar.xs[6] + bar.circle_d;
        assert_eq!(px(&buf, icons_right + 4, cy), BG);

        // パネルは黒ではなく Spotify のパネル色で塗られている
        let p = bar.np_rect();
        assert!(p.x > icons_right, "パネルはアイコン列より右");
        assert_eq!(px(&buf, p.x + p.w / 2, p.y + 2), PANEL, "パネル内側はパネル色");

        // 進捗バー: 塗り部分は緑、末尾側は溝の色
        let prog = bar.np.prog;
        assert_eq!(px(&buf, prog.x + 2, prog.y + prog.h / 2), GREEN);
        assert_eq!(px(&buf, prog.x + prog.w - 2, prog.y + prog.h / 2), GROOVE);

        // 当たり判定: アイコン中心はヒット、アイコン列の左外は外れ
        assert_eq!(bar.hit(cx as f64, cy as f64, true), Some(Hit::Icon(1)));
        assert_eq!(bar.hit(2.0, cy as f64, true), None);

        // 列③の 5 スロットの中心はそれぞれのボタンを返す
        for (i, ctrl) in CTRLS.iter().enumerate() {
            let s = bar.np.ctrl_slot(i);
            let (sx, sy) = ((s.x + s.w / 2) as f64, (s.y + s.h / 2) as f64);
            assert_eq!(bar.hit(sx, sy, true), Some(Hit::Ctrl(*ctrl)), "スロット {i}");
            // パネル非表示中はボタン判定をしない
            assert_eq!(bar.hit(sx, sy, false), None, "非表示時のスロット {i}");
        }

        // TASKVAR_TEST_DUMP=path で目視確認用の PPM を書き出す。
        // トグルの ON/OFF でグリフと色が変わるので、両方の状態を出す
        // (path と path.off の 2 枚)。
        if let Ok(path) = std::env::var("TASKVAR_TEST_DUMP") {
            let dump = |buf: &[u8], to: &str| {
                let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
                for p in buf.chunks_exact(4) {
                    ppm.extend_from_slice(&[p[2], p[1], p[0]]); // BGRA → RGB
                }
                std::fs::write(to, ppm).unwrap();
            };
            dump(&buf, &path);
            // シャッフル OFF / 1 曲リピート / 停止中
            let off = NpView {
                np: &np,
                player: PlayerState { playing: false, shuffle: false, repeat: Loop::Track },
                art: None,
            };
            bar.draw(&mut buf, &st, Some(&off));
            dump(&buf, &format!("{path}.off"));
        }
    }

    #[test]
    fn panel_is_omitted_without_now_playing() {
        let (w, h) = (1366u32, 96u32);
        let bar = Bar::new(w, h).unwrap();
        let mut buf = vec![0u8; (w * h * 4) as usize];
        bar.draw(&mut buf, &state("spotify", &["spotify"]), None);
        let p = bar.np_rect();
        let off = (((p.y + 2) * w + p.x + p.w / 2) * 4) as usize;
        assert_eq!([buf[off], buf[off + 1], buf[off + 2]], BG, "パネル無しなら黒のまま");
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
    fn buttons_and_progress_stay_inside_their_rows() {
        let l = NpLayout::new(1366, 96, 24 + 592);
        assert!(l.btn_d <= l.row1_h, "ボタンが上段からはみ出している");
        assert!(l.prog.h <= l.row2_h, "進捗バーが下段からはみ出している");
        assert!(l.btn_y + l.btn_d <= l.row2_y, "ボタンが進捗バーの行へ食い込んでいる");
        assert!(
            l.prog.y + l.prog.h <= l.panel.y + l.panel.h,
            "進捗バーがパネルの外へ出ている"
        );
        // 進捗バーはボタン列と同じ幅・同じ左端に揃う
        assert_eq!((l.prog.x, l.prog.w), (l.col3_x, l.col3_w));
    }

    #[test]
    fn text_width_drives_the_panel_when_given() {
        // 列②に 300px を指定 → その幅ちょうどになるパネル幅が返る
        let want = PAD * 2 + 72 + GAP + 300 + GAP + 200;
        assert_eq!(panel_width(2000, 560, Some(300), 72, 200), want);
        // 実際にレイアウトへ通しても列②は 300px
        let l = NpLayout::new(1366, 96, 24 + 592);
        let text_w = l.col2_w; // 既定(TASKVAR_NP_W=560)の余り
        assert_eq!(text_w, 560 - PAD * 2 - 72 - GAP - GAP - 200);

        // 使える幅に収まらなければそこで頭打ち
        assert_eq!(panel_width(400, 560, Some(300), 72, 200), 400);
        // 未指定ならパネル幅が主(こちらも avail で頭打ち)
        assert_eq!(panel_width(2000, 560, None, 72, 200), 560);
        assert_eq!(panel_width(400, 560, None, 72, 200), 400);
    }

    #[test]
    fn panel_never_overlaps_the_icons() {
        // アイコン列が伸びてパネルの取り分が減っても、右端を侵食せず幅だけ縮む
        // (env は他のテストと共有されるのでレイアウトを直接組んで確かめる)
        for icons_right in [600u32, 900, 1200] {
            let l = NpLayout::new(1366, 96, icons_right);
            assert!(l.panel.x > icons_right, "icons_right={icons_right} で重なっている");
            assert!(l.panel.x + l.panel.w <= 1366, "icons_right={icons_right} で画面外");
            // 列②が潰れても他の列は成立したまま
            assert_eq!(l.col3_x + l.col3_w + 8, l.panel.x + l.panel.w);
        }
    }
}
