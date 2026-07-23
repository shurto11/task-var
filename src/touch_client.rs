//! touch-server クライアント。touch-key/src/touch_client.rs を踏襲。
//!
//! タスクバーの占有領域(画面下部)を region として touch-server に申告し、その
//! 矩形内で起きたタッチのうち up だけを `Tap` 判定用に main へ渡す。
//!
//! region は共有(`Arc<Mutex<FracRect>>`)で、main が書き換えると張り直して
//! 申告し直す(バー表示中は帯全体、非表示中はスワイプ検知用の下端だけ)。
//! `priority` を申告することで、後から起動した全画面クライアント(fbhalf 等)に
//! バー領域のタッチを奪われないようにする。
//! (Hello 送信 → set_read_timeout → chunk 読み → \n 区切り JSON → 切断で張り直し)

use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 画面に対する 0..1 の割合で表した矩形(touch-server の FracRect に対応)。
#[derive(Serialize, Clone, Copy, PartialEq)]
pub struct FracRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// 接続直後に 1 行だけ送る申告メッセージ。
#[derive(Serialize)]
struct Hello {
    hello: &'static str,
    pane: Option<String>,
    region: Option<FracRect>,
    /// region が重なったときの優先度(既定 0、大きいほど優先)。
    priority: i32,
}

/// サーバーから届くイベント。up の始点・終点だけ使う。
#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "type")]
    typ: String,
    #[serde(default)]
    fx0: f64,
    #[serde(default)]
    fy0: f64,
    #[serde(default)]
    fx1: f64,
    #[serde(default)]
    fy1: f64,
}

/// main へ渡すタッチ終了イベント。座標は全画面 frac(0..1)。
/// タップ判定(始点と終点の距離)は受信側で行う。
pub struct Up {
    pub fx0: f64,
    pub fy0: f64,
    pub fx1: f64,
    pub fy1: f64,
}

/// ソケットパス: `$TOUCH_SERVER_SOCK` > `$XDG_RUNTIME_DIR/touch-server.sock` > `/tmp/...`。
fn socket_path() -> String {
    if let Ok(p) = std::env::var("TOUCH_SERVER_SOCK") {
        if !p.is_empty() {
            return p;
        }
    }
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(d) if !d.is_empty() => format!("{d}/touch-server.sock"),
        _ => "/tmp/touch-server.sock".to_string(),
    }
}

/// touch-client スレッドを起動する(detached)。切断・接続失敗時は再接続し続ける。
/// `region` は共有。main が書き換えると張り直して新しい矩形で申告し直す。
pub fn spawn(region: Arc<Mutex<FracRect>>, priority: i32, tx: Sender<Up>) {
    std::thread::spawn(move || loop {
        match session(&region, priority, &tx) {
            Err(e) => {
                eprintln!("task-var: touch-server 接続待ち ({e})");
                std::thread::sleep(Duration::from_millis(500));
            }
            // region 変更による張り直しは素早く(バー表示直後のタップを取りこぼさない)
            Ok(()) => std::thread::sleep(Duration::from_millis(50)),
        }
    });
}

/// 1 接続ぶんの受信ループ。EOF / エラー / region 変更で戻る(呼び出し側が張り直す)。
fn session(region: &Arc<Mutex<FracRect>>, priority: i32, tx: &Sender<Up>) -> std::io::Result<()> {
    let declared = *region.lock().unwrap();
    let stream = UnixStream::connect(socket_path())?;
    let hello = Hello {
        hello: "task-var",
        pane: std::env::var("TMUX_PANE").ok(),
        region: Some(declared),
        priority,
    };
    let line = serde_json::to_string(&hello).unwrap_or_default();
    (&stream).write_all(format!("{line}\n").as_bytes())?;
    // region 変更を検知するため、ブロックしっぱなしにせず定期的に起こす
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match (&stream).read(&mut chunk) {
            Ok(0) => return Ok(()), // サーバー切断
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let l: Vec<u8> = buf.drain(..=pos).collect();
                    if let Some(ev) = parse(&l) {
                        // 受信側(main)が落ちていたら終了
                        if tx.send(ev).is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                // region が変わっていたら、新しい矩形で申告し直す(張り直し)
                if *region.lock().unwrap() != declared {
                    return Ok(());
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// 1 行 JSON を Up へ。up 以外の type は None。
fn parse(line: &[u8]) -> Option<Up> {
    let ev: RawEvent = serde_json::from_slice(line).ok()?;
    (ev.typ == "up").then_some(Up { fx0: ev.fx0, fy0: ev.fy0, fx1: ev.fx1, fy1: ev.fy1 })
}
