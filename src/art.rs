//! アルバムアートの取得。
//!
//! 取得は curl のサブプロセスで行う(HTTPS のためだけに TLS スタックを
//! 依存に足さない。busctl / tmux / stty と同じ方針)。デコードとリサイズだけ
//! image クレートを使う。
//!
//! 再取得の判定は **art_url の文字列比較**。spotatui の JSON はポーリングごとに
//! 書き直されるのでファイルの mtime は常に変わってしまい、mtime を見ると
//! 毎秒ダウンロードし直すことになる(spotatui-pip が 2f3949f で直した既知の罠)。

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

/// 取得結果(URL と side*side*4 の BGRA)。
type Fetched = (String, Vec<u8>);

pub struct Art {
    tx: Sender<String>,
    done: Arc<Mutex<Option<Fetched>>>,
    /// 依頼済みの URL。同じ URL を何度も投げないため。
    asked: Mutex<Option<String>>,
}

impl Art {
    /// 取得スレッドを起動する(detached)。`side` は出力する正方形の一辺(px)。
    pub fn spawn(side: u32) -> Self {
        let (tx, rx) = mpsc::channel::<String>();
        let done: Arc<Mutex<Option<Fetched>>> = Arc::new(Mutex::new(None));
        let sink = done.clone();
        std::thread::spawn(move || {
            while let Ok(url) = rx.recv() {
                // 依頼が溜まっていたら最後のものだけ取りに行く(曲送り連打対策)
                let url = rx.try_iter().last().unwrap_or(url);
                match fetch(&url, side) {
                    Some(bgra) => *sink.lock().unwrap() = Some((url, bgra)),
                    None => eprintln!("task-var: アルバムアートの取得に失敗: {url}"),
                }
            }
        });
        Self { tx, done, asked: Mutex::new(None) }
    }

    /// この URL のアートが要ることを伝える。未取得なら取得スレッドへ投げる。
    pub fn request(&self, url: &str) {
        let mut asked = self.asked.lock().unwrap();
        if asked.as_deref() == Some(url) {
            return;
        }
        *asked = Some(url.to_string());
        let _ = self.tx.send(url.to_string());
    }

    /// 取得が終わっていれば 1 回だけ取り出す。
    pub fn take(&self) -> Option<Fetched> {
        self.done.lock().unwrap().take()
    }
}

/// curl で落として side x side の BGRA へ。
fn fetch(url: &str, side: u32) -> Option<Vec<u8>> {
    let out = std::process::Command::new("curl")
        .args(["-sL", "--max-time", "10", url])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let img = image::load_from_memory(&out.stdout).ok()?;
    let scaled = img.resize_exact(side, side, image::imageops::FilterType::Lanczos3).to_rgba8();
    let mut bgra = Vec::with_capacity((side * side * 4) as usize);
    for p in scaled.pixels() {
        let [r, g, b, _] = p.0;
        bgra.extend_from_slice(&[b, g, r, 0]);
    }
    Some(bgra)
}

/// 背景グラデーションの起点色。アルバムアートからは**色相と彩度だけ**を受け取り、
/// 明度はこちらで固定する。アートによっては明るすぎて白文字が読めなくなるため。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Accent {
    /// パネル下地の左端。
    pub fill: [u8; 3],
    /// 枠線の左端。fill と同色で一段明るい。
    pub edge: [u8; 3],
}

/// 固定する明度。fill は白文字とのコントラスト比 8:1 前後を保つ値。
const FILL_L: f32 = 0.20;
const EDGE_L: f32 = 0.32;
/// 彩度の下限と上限。上げすぎると滲み、下げすぎるとアートの色が伝わらない。
const S_MIN: f32 = 0.45;
const S_MAX: f32 = 0.85;

impl Default for Accent {
    /// アート未取得のときは従来どおり Spotify グリーン(#1ED760 の色相)。
    fn default() -> Self {
        Self::from_hs(145.0, 0.75)
    }
}

impl Accent {
    fn from_hs(h: f32, s: f32) -> Self {
        Self { fill: from_hsl(h, s, FILL_L), edge: from_hsl(h, s, EDGE_L) }
    }
}

/// アルバムアート(side*side*4 の BGRA)から代表色を選ぶ。
///
/// 平均色は必ず濁るので、彩度のある中間調のピクセルだけを色相 10 度ごとに
/// 集計し、重みが最大のビンとその両隣を採る(Android の Palette の Vibrant に
/// 近い考え方)。色相は円周上のベクトル和で平均するので、赤のように 0/360 度を
/// またぐ色でも分裂しない。
pub fn accent(bgra: &[u8]) -> Accent {
    const BINS: usize = 36;
    let (mut w, mut x, mut y, mut sat) = ([0f32; BINS], [0f32; BINS], [0f32; BINS], [0f32; BINS]);
    for p in bgra.chunks_exact(4) {
        let (h, s, l) = to_hsl([p[0], p[1], p[2]]);
        // 地の黒/白と無彩色は代表色にしない
        if !(0.08..=0.92).contains(&l) || s < 0.15 {
            continue;
        }
        // 鮮やかで中間調のものほど重く見る
        let weight = s * (1.0 - (l - 0.5).abs() * 1.6).max(0.0);
        let b = (h / 10.0) as usize % BINS;
        let rad = h.to_radians();
        w[b] += weight;
        x[b] += rad.cos() * weight;
        y[b] += rad.sin() * weight;
        sat[b] += s * weight;
    }
    // 隣のビンも含めた重みで選ぶ。境目でたまたま割れた色を拾い損ねないため。
    let around = |b: usize| {
        let (p, n) = ((b + BINS - 1) % BINS, (b + 1) % BINS);
        (w[p] + w[b] + w[n], x[p] + x[b] + x[n], y[p] + y[b] + y[n], sat[p] + sat[b] + sat[n])
    };
    let best = (0..BINS).max_by(|&a, &b| around(a).0.total_cmp(&around(b).0)).unwrap_or(0);
    let (tw, tx, ty, ts) = around(best);
    if tw <= 0.0 {
        // 白黒のアート。無理に色を付けず、わずかに冷たい灰色にする。
        return Accent::from_hs(210.0, 0.06);
    }
    Accent::from_hs(ty.atan2(tx).to_degrees(), (ts / tw).clamp(S_MIN, S_MAX))
}

/// BGR → (色相 0..360, 彩度 0..1, 明度 0..1)。
fn to_hsl(bgr: [u8; 3]) -> (f32, f32, f32) {
    let (b, g, r) = (bgr[0] as f32 / 255.0, bgr[1] as f32 / 255.0, bgr[2] as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let (l, d) = ((max + min) / 2.0, max - min);
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d) % 6.0
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    ((h * 60.0).rem_euclid(360.0), s.clamp(0.0, 1.0), l)
}

/// (色相, 彩度, 明度) → BGR。
fn from_hsl(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [b, g, r].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 単色で埋めた 8x8 の BGRA。
    fn solid(bgr: [u8; 3]) -> Vec<u8> {
        [bgr[0], bgr[1], bgr[2], 0].repeat(64)
    }

    #[test]
    fn accent_takes_the_hue_from_the_art() {
        let red = accent(&solid([0x20, 0x20, 0xD0])); // 赤いジャケット
        assert!(red.fill[2] > red.fill[1] + 30, "赤が拾えていない: {:?}", red.fill);
        let blue = accent(&solid([0xD0, 0x40, 0x20]));
        assert!(blue.fill[0] > blue.fill[2] + 30, "青が拾えていない: {:?}", blue.fill);
    }

    #[test]
    fn accent_fixes_the_lightness_whatever_the_art() {
        // 目に痛い蛍光色でも、白文字が乗る明るさまで落とす
        for bgr in [[0x00, 0xFF, 0x00], [0xFF, 0xFF, 0x00], [0x30, 0x20, 0xF0]] {
            let a = accent(&solid(bgr));
            let (_, _, l) = to_hsl(a.fill);
            assert!((l - FILL_L).abs() < 0.02, "{bgr:?} の明度が固定されていない: {l}");
            let (_, _, le) = to_hsl(a.edge);
            assert!(le > l, "枠線が下地より暗い");
        }
    }

    #[test]
    fn accent_stays_neutral_for_a_monochrome_cover() {
        let gray = accent(&solid([0x80, 0x80, 0x80]));
        let (_, s, _) = to_hsl(gray.fill);
        assert!(s < 0.2, "白黒のアートに色が付いている: {s}");
    }

    #[test]
    fn hsl_round_trips() {
        for bgr in [[0x28, 0x5A, 0x0C], [0xFF, 0x00, 0x7F], [0x11, 0x22, 0x33]] {
            let (h, s, l) = to_hsl(bgr);
            let back = from_hsl(h, s, l);
            assert!(
                bgr.iter().zip(back).all(|(a, b)| a.abs_diff(b) <= 1),
                "{bgr:?} -> {back:?}"
            );
        }
    }
}
