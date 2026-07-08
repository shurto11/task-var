//! /dev/fb0 への直接描画。dopagaki の fb.rs を踏襲。
//!
//! fbterm 環境では stride(1376px) ≠ width(1366px) なので行オフセットは必ず
//! stride 基準で計算する。ピクセルは BGRA(リトルエンディアン XRGB8888)。
//! この環境は 32bpp 固定。非 32bpp は起動時にエラーにする。
//!
//! 画面回転(fbterm screen-rotate)に対応: `width`/`height` は回転適用後の
//! **論理画面**サイズを公開し、`blit` は論理座標を受けて内部で物理 fb 座標へ
//! 転置する。touch-server も論理座標で配信するため、呼び出し側は回転を意識
//! しなくてよい。

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;

/// 画面回転量 (fbterm の screen-rotate 値: 0=なし, 1=時計回り90°, 2=180°, 3=反時計回り90°)。
/// 環境変数 TOUCH_ROTATE があれば最優先、無ければ ~/.fbtermrc の screen-rotate= に追従。
fn screen_rotate() -> u8 {
    if let Ok(v) = std::env::var("TOUCH_ROTATE") {
        if let Ok(n) = v.trim().parse::<u8>() {
            return n % 4;
        }
    }
    let Some(home) = std::env::var_os("HOME") else { return 0 };
    let path = std::path::Path::new(&home).join(".fbtermrc");
    let Ok(text) = std::fs::read_to_string(&path) else { return 0 };
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("screen-rotate=") {
            if let Ok(n) = v.trim().parse::<u8>() {
                return n % 4;
            }
        }
    }
    0
}

pub struct Framebuffer {
    file: File,
    /// 論理画面幅(回転適用後)。回転 90°/270° では物理の高さ。
    pub width: u32,
    /// 論理画面高(回転適用後)。
    pub height: u32,
    /// 回転量(0..=3)。ログ表示用。
    pub rotate: u8,
    /// 物理 1 行あたりのピクセル数(stride バイト / 4)
    stride: u32,
}

impl Framebuffer {
    pub fn open() -> Result<Self> {
        let bpp: u32 = std::fs::read_to_string("/sys/class/graphics/fb0/bits_per_pixel")
            .context("fb0 bits_per_pixel 読み取り失敗")?
            .trim()
            .parse()?;
        if bpp != 32 {
            bail!("未対応の bpp={bpp}(32bpp BGRA のみ対応)");
        }
        let vsize = std::fs::read_to_string("/sys/class/graphics/fb0/virtual_size")
            .context("fb0 virtual_size 読み取り失敗")?;
        let (w, h) = vsize.trim().split_once(',').context("virtual_size の形式が不正")?;
        let stride_bytes: u32 = std::fs::read_to_string("/sys/class/graphics/fb0/stride")
            .context("fb0 stride 読み取り失敗")?
            .trim()
            .parse()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fb0")
            .context("/dev/fb0 を開けません(video グループ権限が必要)")?;
        let (phys_w, phys_h): (u32, u32) = (w.parse()?, h.trim().parse()?);
        let rotate = screen_rotate();
        let (width, height) = if rotate % 2 == 1 { (phys_h, phys_w) } else { (phys_w, phys_h) };
        Ok(Self { file, width, height, rotate, stride: stride_bytes / 4 })
    }

    /// bgra(w*h*4 バイト)を論理座標 (x,y) に書き込む。回転があれば物理座標へ転置。
    pub fn blit(&self, x: u32, y: u32, w: u32, h: u32, bgra: &[u8]) -> Result<()> {
        let (x, y, w, h) = self.clamp(x, y, w, h);
        if w == 0 || h == 0 {
            return Ok(());
        }
        if self.rotate == 0 {
            let row_bytes = (w * 4) as usize;
            for j in 0..h {
                let src = &bgra[j as usize * row_bytes..][..row_bytes];
                let off = (((y + j) * self.stride + x) * 4) as u64;
                self.file.write_at(src, off)?;
            }
            return Ok(());
        }
        let (px0, py0, pw, ph) = phys_rect(self.rotate, self.width, self.height, x, y, w, h);
        // 論理バッファを物理向きへ転置してから行単位で書き込む
        let mut phys = vec![0u8; (pw * ph * 4) as usize];
        let (w, h, pw_us) = (w as usize, h as usize, pw as usize);
        for j in 0..h {
            for i in 0..w {
                let (dx, dy) = phys_pos(self.rotate, w, h, i, j);
                let src = (j * w + i) * 4;
                let dst = (dy * pw_us + dx) * 4;
                phys[dst..dst + 4].copy_from_slice(&bgra[src..src + 4]);
            }
        }
        let row_bytes = (pw * 4) as usize;
        for j in 0..ph {
            let src = &phys[j as usize * row_bytes..][..row_bytes];
            let off = (((py0 + j) * self.stride + px0) * 4) as u64;
            self.file.write_at(src, off)?;
        }
        Ok(())
    }

    fn clamp(&self, x: u32, y: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let x = x.min(self.width);
        let y = y.min(self.height);
        (x, y, w.min(self.width - x), h.min(self.height - y))
    }
}

/// 論理画面 (lw x lh) 上の矩形 (x,y,w,h) が、回転後の物理 fb 上で占める矩形。
fn phys_rect(rotate: u8, lw: u32, lh: u32, x: u32, y: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
    match rotate {
        1 => (lh - y - h, x, h, w),
        2 => (lw - x - w, lh - y - h, w, h),
        _ => (y, lw - x - w, h, w), // 3
    }
}

/// 論理矩形内のピクセル (i,j) の、物理矩形内ローカル位置。
fn phys_pos(rotate: u8, w: usize, h: usize, i: usize, j: usize) -> (usize, usize) {
    match rotate {
        1 => (h - 1 - j, i),
        2 => (w - 1 - i, h - 1 - j),
        _ => (j, w - 1 - i), // 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 論理画面全体のピクセル (x,y) が物理 fb のどこへ写るかの参照式。
    /// touch-server の rotate_frac(タッチ→論理)の逆変換に一致する。
    fn reference(rotate: u8, lw: u32, lh: u32, x: u32, y: u32) -> (u32, u32) {
        match rotate {
            1 => (lh - 1 - y, x),
            2 => (lw - 1 - x, lh - 1 - y),
            3 => (y, lw - 1 - x),
            _ => (x, y),
        }
    }

    #[test]
    fn phys_mapping_matches_reference() {
        // 論理 6x4(縦画面想定)、下部 2 行を矩形として全回転を検査
        let (lw, lh) = (6u32, 4u32);
        let (x, y, w, h) = (0u32, 2u32, 6u32, 2u32);
        for rotate in 1u8..=3 {
            let (px0, py0, pw, ph) = phys_rect(rotate, lw, lh, x, y, w, h);
            // 物理矩形が物理画面 (回転で lw/lh 入れ替え) に収まる
            let (pw_max, ph_max) = if rotate % 2 == 1 { (lh, lw) } else { (lw, lh) };
            assert!(px0 + pw <= pw_max && py0 + ph <= ph_max, "rotate={rotate} 矩形が範囲外");
            for j in 0..h {
                for i in 0..w {
                    let (dx, dy) = phys_pos(rotate, w as usize, h as usize, i as usize, j as usize);
                    let got = (px0 + dx as u32, py0 + dy as u32);
                    let want = reference(rotate, lw, lh, x + i, y + j);
                    assert_eq!(got, want, "rotate={rotate} 論理({},{}) の写像が不一致", x + i, y + j);
                }
            }
        }
    }
}
