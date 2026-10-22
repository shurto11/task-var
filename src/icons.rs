//! Simple Icons の SVG(assets/ に fetch-icons.sh でダウンロード、ビルド時に埋め込み)を
//! resvg でレンダリングする。cdn.simpleicons.org の SVG はブランドカラーの fill 付きなので
//! そのまま描けばカラーになる。
//!
//! Melon のロゴだけは商標をリポジトリに含めないため埋め込まず、
//! fetch-melon-logo.sh が落とした PNG を実行時に読む(`load_logo`)。

use anyhow::{Context, Result};
use resvg::{tiny_skia, usvg};

/// レンダリング済みグリフ。`rgba` は px*px*4 バイトのプリマルチプライドRGBA。
pub struct Glyph {
    pub px: u32,
    pub rgba: Vec<u8>,
}

/// 読み込んだロゴ。`rgba` は w*h*4 バイトのプリマルチプライドRGBA。
pub struct Logo {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// 地の上にロゴを置いた画像を読み、まわりの地を切り落として max_w×max_h に収まる
/// 大きさへ(縦横比は保って)縮める。地の色は左上の画素で判断する。
/// 地が不透明なら切り落とした内側の地もそのまま残るので、同じ色の上へ置く。
pub fn load_logo(path: &str, max_w: u32, max_h: u32) -> Result<Logo> {
    let img = image::open(path).with_context(|| format!("{path} を開けない"))?.to_rgba8();
    let bg = *img.get_pixel(0, 0);
    let ink = |x: u32, y: u32| {
        img.get_pixel(x, y).0.iter().zip(bg.0).any(|(a, b)| a.abs_diff(b) > 8)
    };
    let (w, h) = img.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if ink(x, y) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    anyhow::ensure!(x0 < x1 && y0 < y1, "{path} が地の色だけでロゴが無い");
    let (cw, ch) = (x1 - x0, y1 - y0);
    let scale = (max_w as f32 / cw as f32).min(max_h as f32 / ch as f32);
    let lw = ((cw as f32 * scale).round() as u32).clamp(1, max_w.max(1));
    let lh = ((ch as f32 * scale).round() as u32).clamp(1, max_h.max(1));
    let crop = image::imageops::crop_imm(&img, x0, y0, cw, ch).to_image();
    let scaled = image::imageops::resize(&crop, lw, lh, image::imageops::FilterType::Lanczos3);
    let rgba = scaled
        .pixels()
        .flat_map(|p| {
            let [r, g, b, a] = p.0;
            let pm = |c: u8| (c as u32 * a as u32 / 255) as u8;
            [pm(r), pm(g), pm(b), a]
        })
        .collect();
    Ok(Logo { w: lw, h: lh, rgba })
}

/// SVG を px×px に収まるようアスペクト維持でレンダリングして中央寄せする。
pub fn render(svg: &[u8], px: u32) -> Result<Glyph> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg, &opt).context("SVG の解析に失敗")?;
    let mut pixmap = tiny_skia::Pixmap::new(px, px).context("Pixmap 確保失敗")?;
    let size = tree.size();
    let scale = (px as f32 / size.width()).min(px as f32 / size.height());
    let tx = (px as f32 - size.width() * scale) / 2.0;
    let ty = (px as f32 - size.height() * scale) / 2.0;
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(Glyph { px, rgba: pixmap.take() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_logo_trims_the_ground_and_fits_the_box() {
        // 白地 40x40 の (10,5) に緑の 8x12 の塊。地は切り落とされ、縦横比を保って箱に収まる
        let mut img = image::RgbaImage::from_pixel(40, 40, image::Rgba([255, 255, 255, 255]));
        for y in 5..17 {
            for x in 10..18 {
                img.put_pixel(x, y, image::Rgba([4, 230, 50, 255]));
            }
        }
        let path = std::env::temp_dir().join(format!("task-var-logo-{}.png", std::process::id()));
        img.save(&path).unwrap();
        let path = path.to_str().unwrap();

        let logo = load_logo(path, 12, 9).unwrap();
        assert_eq!((logo.w, logo.h), (6, 9), "縦に合わせて 8x12 → 6x9");
        assert_eq!(logo.rgba.len(), (6 * 9 * 4) as usize);
        // 地を切り落としたので、真ん中も四隅も緑(白い余白が残っていない)
        let at = |x: u32, y: u32| &logo.rgba[((y * logo.w + x) * 4) as usize..][..4];
        assert_eq!(at(3, 4), [4, 230, 50, 255]);
        let green = |p: &[u8]| p[1] > 150 && p[0] < 100;
        assert!(green(at(0, 0)) && green(at(5, 8)), "{:?} / {:?}", at(0, 0), at(5, 8));

        // 横長の箱なら横は余る(高さで決まる)
        assert_eq!(load_logo(path, 100, 24).map(|l| (l.w, l.h)).unwrap(), (16, 24));
        std::fs::remove_file(path).unwrap();

        // 無いファイルはエラー(呼び手が札を白地だけにする)
        assert!(load_logo("/nonexistent/melon.png", 10, 10).is_err());
    }
}
