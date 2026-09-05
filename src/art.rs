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
