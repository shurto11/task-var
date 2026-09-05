//! ab_glyph によるテキスト描画。BGRA バッファへ直接書き込む。
//!
//! spotatui-pip は imageproc の `draw_text_mut` を使っているが、あれは `image`
//! クレートのバッファ前提。task-var のバーは生の BGRA なので、グリフの
//! アウトラインを自前でカバレッジ合成する。
//! フォント探索順と `…` 切り詰めのロジックは spotatui-pip から踏襲した。

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont};

/// フォント候補。CJK が要るので Noto Sans CJK を最優先で探す。
const CANDIDATES: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/OTF/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJKjp-Regular.otf",
    "/usr/share/fonts/noto-cjk/NotoSansCJKjp-Regular.otf",
    "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
    "/usr/share/fonts/opentype/ipaexfont-gothic/ipaexg.ttf",
    "/usr/share/fonts/opentype/ipafont-gothic/ipagp.ttf",
    "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
    "/usr/share/fonts/truetype/vlgothic/VL-PGothic-Regular.ttf",
    "/usr/share/fonts/truetype/vlgothic/VL-Gothic-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf",
];

pub struct Font {
    inner: FontVec,
}

/// TTC(フォントコレクション)は `try_from_vec` が弾くので index 0 で開き直す。
fn try_load(data: Vec<u8>) -> Option<FontVec> {
    FontVec::try_from_vec(data.clone())
        .or_else(|_| FontVec::try_from_vec_and_index(data, 0))
        .ok()
}

impl Font {
    /// `TASKVAR_FONT` > 候補リストの順で探す。見つからなければ None
    /// (呼び出し側はテキスト描画を丸ごと省く)。
    pub fn load() -> Option<Self> {
        if let Ok(p) = std::env::var("TASKVAR_FONT") {
            if let Some(f) = std::fs::read(&p).ok().and_then(try_load) {
                return Some(Self { inner: f });
            }
            eprintln!("task-var: TASKVAR_FONT を読めません: {p}");
        }
        for p in CANDIDATES {
            if let Some(f) = std::fs::read(p).ok().and_then(try_load) {
                return Some(Self { inner: f });
            }
        }
        eprintln!("task-var: 使えるフォントが見つかりません(テキストは描画しません)");
        None
    }

    /// テキストの描画幅(px)。カーニングは見ない(ウィジェット用途では十分)。
    pub fn width(&self, text: &str, px: f32) -> f32 {
        let sf = self.inner.as_scaled(PxScale::from(px));
        text.chars().map(|c| sf.h_advance(sf.glyph_id(c))).sum()
    }

    /// `max_px` に収まる最大の文字サイズを返す。`preferred` は超えず、`min` は下回らない。
    ///
    /// ab_glyph の送り幅は指定サイズに正比例するので、収まらない場合の縮小率は
    /// 幅の比からそのまま求まる(二分探索は要らない)。
    pub fn shrink_to_fit(&self, text: &str, max_px: f32, preferred: f32, min: f32) -> f32 {
        let w = self.width(text, preferred);
        if w <= max_px || w <= 0.0 {
            return preferred;
        }
        // 浮動小数の誤差でぎりぎり溢れないよう少しだけ内側に取る
        (preferred * max_px / w * 0.999).clamp(min.min(preferred), preferred)
    }

    /// `max_px` に収まるよう末尾を `…` で切り詰める。
    pub fn fit(&self, text: &str, max_px: f32, px: f32) -> String {
        if self.width(text, px) <= max_px {
            return text.to_owned();
        }
        let sf = self.inner.as_scaled(PxScale::from(px));
        let ellipsis = '…';
        let ew = sf.h_advance(sf.glyph_id(ellipsis));
        let mut out = String::new();
        let mut used = 0.0f32;
        for c in text.chars() {
            let cw = sf.h_advance(sf.glyph_id(c));
            if used + cw + ew > max_px {
                break;
            }
            out.push(c);
            used += cw;
        }
        out.push(ellipsis);
        out
    }

    /// `buf`(buf_w * buf_h の BGRA)へ、(x, baseline) を基準に単色で描く。
    /// `color` はバッファに合わせて **BGR** 順(bar.rs の各色定数と同じ)。
    /// アルファはカバレッジとして背景に合成し、書き込む α バイトは 0 のまま
    /// (fb は XRGB8888 なので α は使わない)。
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        buf: &mut [u8],
        buf_w: u32,
        buf_h: u32,
        x: f32,
        baseline: f32,
        px: f32,
        color: [u8; 3],
        text: &str,
    ) {
        let scale = PxScale::from(px);
        let sf = self.inner.as_scaled(scale);
        let mut pen = x;
        for c in text.chars() {
            let id = sf.glyph_id(c);
            let adv = sf.h_advance(id);
            let glyph = id.with_scale_and_position(scale, ab_glyph::point(pen, baseline));
            if let Some(outline) = self.inner.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, cov| {
                    if cov <= 0.0 {
                        return;
                    }
                    let px_x = bounds.min.x + gx as f32;
                    let px_y = bounds.min.y + gy as f32;
                    if px_x < 0.0 || px_y < 0.0 {
                        return;
                    }
                    let (ix, iy) = (px_x as u32, px_y as u32);
                    if ix >= buf_w || iy >= buf_h {
                        return;
                    }
                    let off = ((iy * buf_w + ix) * 4) as usize;
                    let cov = cov.min(1.0);
                    for (k, src) in color.into_iter().enumerate() {
                        let dst = buf[off + k] as f32;
                        buf[off + k] = (dst * (1.0 - cov) + src as f32 * cov).round() as u8;
                    }
                });
            }
            pen += adv;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_to_fit_scales_down_only_when_needed() {
        let Some(font) = Font::load() else {
            eprintln!("フォントが無い環境なのでスキップ");
            return;
        };
        let text = "I mean, It's about time";
        let full = font.width(text, 28.0);

        // 収まるなら既定サイズのまま
        assert_eq!(font.shrink_to_fit(text, full + 1.0, 28.0, 14.0), 28.0);

        // 収まらないなら、その幅にちょうど収まるところまで縮める
        let px = font.shrink_to_fit(text, full * 0.6, 28.0, 8.0);
        assert!(px < 28.0, "縮んでいない: {px}");
        assert!(font.width(text, px) <= full * 0.6, "縮めても収まっていない");

        // 下限より下へは行かない(そこは呼び出し側が … で詰める)
        assert_eq!(font.shrink_to_fit(text, 1.0, 28.0, 15.0), 15.0);
    }

    #[test]
    fn fit_truncates_with_an_ellipsis() {
        let Some(font) = Font::load() else {
            eprintln!("フォントが無い環境なのでスキップ");
            return;
        };
        let px = 18.0;
        let long = "I mean, It's about time - BLINGY - The 7th Album";
        let full = font.width(long, px);
        assert!(full > 100.0);

        // 収まるなら手を加えない
        assert_eq!(font.fit(long, full + 1.0, px), long);

        // 収まらないなら … 付きで幅に収める
        let cut = font.fit(long, 100.0, px);
        assert!(cut.ends_with('…'), "末尾が … でない: {cut}");
        assert!(font.width(&cut, px) <= 100.0, "切り詰め後も幅を超えている: {cut}");
        assert!(long.starts_with(cut.trim_end_matches('…')), "先頭が元テキストと一致しない");

        // 日本語(CJK)でも同じ
        let jp = "きらきら武士 - フェアリーズ";
        let cut_jp = font.fit(jp, 60.0, px);
        assert!(cut_jp.ends_with('…'));
        assert!(font.width(&cut_jp, px) <= 60.0);
    }
}
