//! Simple Icons の SVG(assets/ に fetch-icons.sh でダウンロード、ビルド時に埋め込み)を
//! resvg でレンダリングする。cdn.simpleicons.org の SVG はブランドカラーの fill 付きなので
//! そのまま描けばカラーになる。

use anyhow::{Context, Result};
use resvg::{tiny_skia, usvg};

/// レンダリング済みグリフ。`rgba` は px*px*4 バイトのプリマルチプライドRGBA。
pub struct Glyph {
    pub px: u32,
    pub rgba: Vec<u8>,
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
