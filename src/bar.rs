//! タスクバーのレイアウト・描画・当たり判定。
//!
//! バーは画面下部の全幅の帯(BGRA バッファ)で、中央に白円アイコンを横一列に並べる。
//! 外枠リング: 表示中セッション=青 / 存在するが非表示=灰 / セッション無し=枠なし。
//! tmux アイコンはセッションに対応しないため、「名前付きセッション以外を表示中」の
//! ときに青リングにする(通常の作業セッションにいる状態を表す)。

use crate::actions::{IconDef, ICONS};
use crate::icons::{self, Glyph};
use crate::tmux::State;
use anyhow::Result;

const BG: [u8; 3] = [0, 0, 0]; // BGR
const WHITE: [u8; 3] = [255, 255, 255];
const BLUE: [u8; 3] = [255, 144, 30]; // #1E90FF
const GRAY: [u8; 3] = [128, 128, 128];
/// リングの太さ(px)。
const RING_W: f32 = 4.0;

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub struct Bar {
    pub w: u32,
    circle_d: u32,
    /// 各アイコンタイルの左上 x(y はバー内で垂直センタリング)。
    xs: Vec<u32>,
    tile_y: u32,
    glyphs: Vec<Glyph>,
}

impl Bar {
    pub fn new(w: u32, h: u32) -> Result<Self> {
        let circle_d = env_u32("TASKVAR_ICON_D", 64).clamp(16, h.saturating_sub(8).max(16));
        let gap = env_u32("TASKVAR_GAP", 24);
        let n = ICONS.len() as u32;
        let total = n * circle_d + (n - 1) * gap;
        let x0 = w.saturating_sub(total) / 2;
        let xs = (0..n).map(|i| x0 + i * (circle_d + gap)).collect();
        let tile_y = (h - circle_d) / 2;
        let glyph_px = circle_d * 58 / 100;
        let glyphs = ICONS.iter().map(|d| icons::render(d.svg, glyph_px)).collect::<Result<_>>()?;
        Ok(Self { w, circle_d, xs, tile_y, glyphs })
    }

    /// バー全体を buf(w*h*4 BGRA)へ描画する。
    pub fn draw(&self, buf: &mut [u8], state: &State) {
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&[BG[0], BG[1], BG[2], 0]);
        }
        for (i, def) in ICONS.iter().enumerate() {
            self.draw_tile(buf, self.xs[i], ring_color(def, state), &self.glyphs[i]);
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
        // グリフ(プリマルチプライドRGBA)を円中央へ合成
        let go_x = x0 + (d - glyph.px) / 2;
        let go_y = self.tile_y + (d - glyph.px) / 2;
        for j in 0..glyph.px {
            for i in 0..glyph.px {
                let s = ((j * glyph.px + i) * 4) as usize;
                let (sr, sg, sb, sa) =
                    (glyph.rgba[s], glyph.rgba[s + 1], glyph.rgba[s + 2], glyph.rgba[s + 3]);
                if sa == 0 {
                    continue;
                }
                let off = (((go_y + j) * self.w + go_x + i) * 4) as usize;
                let inv = (255 - sa) as u32;
                buf[off] = (sb as u32 + buf[off] as u32 * inv / 255) as u8;
                buf[off + 1] = (sg as u32 + buf[off + 1] as u32 * inv / 255) as u8;
                buf[off + 2] = (sr as u32 + buf[off + 2] as u32 * inv / 255) as u8;
            }
        }
    }

    /// バーローカル座標 (lx,ly) がどのアイコンに当たるか。円の少し外までタッチを許容する。
    pub fn hit(&self, lx: f64, ly: f64) -> Option<usize> {
        let r = self.circle_d as f64 / 2.0 + 8.0;
        for (i, &x0) in self.xs.iter().enumerate() {
            let cx = x0 as f64 + self.circle_d as f64 / 2.0;
            let cy = self.tile_y as f64 + self.circle_d as f64 / 2.0;
            let (dx, dy) = (lx - cx, ly - cy);
            if dx * dx + dy * dy <= r * r {
                return Some(i);
            }
        }
        None
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

    #[test]
    fn ring_colors_follow_session_state() {
        // ICONS: [tmux, spotify, shorts, bluetooth, ssbrowse]
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
        let (w, h) = (1366u32, 88u32);
        let bar = Bar::new(w, h).unwrap();
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let st = state("spotify", &["spotify"]);
        bar.draw(&mut buf, &st);

        let px = |x: u32, y: u32| -> [u8; 3] {
            let off = ((y * w + x) * 4) as usize;
            [buf[off], buf[off + 1], buf[off + 2]]
        };
        let (cx, cy) = (bar.xs[1] + bar.circle_d / 2, bar.tile_y + bar.circle_d / 2);
        // 白円内・グリフ外の点は白(グリフはd*58%なので中心から±d*0.29まで)
        assert_eq!(px(cx + bar.circle_d * 38 / 100, cy), WHITE);
        // リング帯(半径 d/2 - RING_W/2 付近)は spotify=表示中 → 青
        assert_eq!(px(cx + bar.circle_d / 2 - 2, cy), BLUE);
        // バー左端はアイコン外 → 背景
        assert_eq!(px(2, cy), BG);

        // 当たり判定: アイコン中心はヒット、バー左端は外れ
        assert_eq!(bar.hit(cx as f64, cy as f64), Some(1));
        assert_eq!(bar.hit(2.0, cy as f64), None);

        // TASKVAR_TEST_DUMP=path で目視確認用の PPM を書き出す
        if let Ok(path) = std::env::var("TASKVAR_TEST_DUMP") {
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in buf.chunks_exact(4) {
                ppm.extend_from_slice(&[p[2], p[1], p[0]]); // BGRA → RGB
            }
            std::fs::write(path, ppm).unwrap();
        }
    }
}
