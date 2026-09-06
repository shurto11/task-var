//! clawd02.png(白・オレンジ・黒の 3 色パレット)を任意サイズ・任意ボディ色で
//! 焼き直すスプライト。touch-claude の `img.rs` を引き継いだもの。
//!
//! 違いは縮小の仕方だけ。touch-claude は画面右上に 128px 幅で出していたので
//! 最近傍で足りたが、バー内の 1 行は 20px そこそこしかない。そこまで縮めると
//! 最近傍では耳や脚が飛ぶので、出力 1px が覆う元領域の「黒率・ボディ率」を
//! 面積平均してアルファに落とす(=ボックスフィルタ)。

use anyhow::{Context, Result};

const WHITE: u8 = 0;
const BLACK: u8 = 1;
const BODY: u8 = 2;

/// 3 色に分類済みの元画像。
pub struct Sprite {
    w: u32,
    h: u32,
    class: Vec<u8>,
    /// 元画像のボディ色(BGR)。処理中(既定 = オレンジ)はこの色をそのまま使う。
    pub body: [u8; 3],
}

/// 元画像の縦横比。行の高さからスプライトの幅を求めるのに使う。
pub const ASPECT: (u32, u32) = (514, 318);

/// ビルド時に埋め込んだ clawd02.png を読む。`TASKVAR_CLAWD_IMG` で差し替えられる。
pub fn load() -> Result<Sprite> {
    let data: Vec<u8> = match std::env::var("TASKVAR_CLAWD_IMG") {
        Ok(p) if !p.is_empty() => {
            std::fs::read(&p).with_context(|| format!("画像を読めません: {p}"))?
        }
        _ => include_bytes!("../assets/clawd02.png").to_vec(),
    };
    let img = image::load_from_memory(&data).context("clawd02.png のデコードに失敗")?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut class = Vec::with_capacity((w * h) as usize);
    let mut body = [80u8, 108, 217]; // 読み取れなかったときの保険(clawd のオレンジ)
    let mut found = false;
    for p in img.pixels() {
        let [r, g, b, a] = p.0;
        let c = if a < 128 || (r >= 200 && g >= 200 && b >= 200) {
            WHITE
        } else if r <= 60 && g <= 60 && b <= 60 {
            BLACK
        } else {
            if !found {
                body = [b, g, r];
                found = true;
            }
            BODY
        };
        class.push(c);
    }
    Ok(Sprite { w, h, class, body })
}

impl Sprite {
    /// (out_w, out_h) の BGRA を返す。白は背景なので透明(α=0)、
    /// 黒と ボディ色 は被覆率をそのまま α に載せる(呼び出し側で下地へ合成する)。
    pub fn render(&self, out_w: u32, out_h: u32, body: [u8; 3]) -> Vec<u8> {
        let mut buf = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
        for oy in 0..out_h {
            let sy0 = oy * self.h / out_h;
            let sy1 = ((oy + 1) * self.h).div_ceil(out_h).max(sy0 + 1).min(self.h);
            for ox in 0..out_w {
                let sx0 = ox * self.w / out_w;
                let sx1 = ((ox + 1) * self.w).div_ceil(out_w).max(sx0 + 1).min(self.w);
                let (mut black, mut fill, mut total) = (0u32, 0u32, 0u32);
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        match self.class[(sy * self.w + sx) as usize] {
                            BLACK => black += 1,
                            BODY => fill += 1,
                            _ => {}
                        }
                        total += 1;
                    }
                }
                let ink = black + fill;
                if ink == 0 || total == 0 {
                    continue; // 白 = 透明のまま
                }
                let d = ((oy * out_w + ox) * 4) as usize;
                let mix = |b: u8, f: u8| ((b as u32 * black + f as u32 * fill) / ink) as u8;
                buf[d] = mix(0, body[0]);
                buf[d + 1] = mix(0, body[1]);
                buf[d + 2] = mix(0, body[2]);
                buf[d + 3] = (ink * 255 / total) as u8;
            }
        }
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_keeps_the_silhouette() {
        let s = load().unwrap();
        // 元画像はオレンジ(ボディ)を含む
        assert!(s.class.contains(&BODY), "ボディ色のピクセルが見つからない");

        // バー 1 行ぶんまで縮めても、輪郭(不透明ピクセル)が残る
        let (w, h) = (32u32, 20u32);
        let buf = s.render(w, h, [0, 0, 255]);
        assert_eq!(buf.len(), (w * h * 4) as usize);
        let opaque = buf.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(opaque > (w * h / 8) as usize, "縮小でシルエットが消えた: {opaque}px");

        // 指定したボディ色が出ている(黒とだけ混ざるので R が優勢のはず)
        assert!(
            buf.chunks_exact(4).any(|p| p[3] > 200 && p[2] > 128 && p[0] < 64),
            "指定したボディ色が反映されていない"
        );
    }
}
