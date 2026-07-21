//! fb-server クライアント。touch_client.rs を踏襲。
//!
//! 起動時に自分の名前("task-var")を hello として申告し、
//! {"visible":bool} を受け取って main へ渡す。
//! (Hello 送信 → set_read_timeout → chunk 読み → \n 区切り JSON → 切断で張り直し)

use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// フレームバッファ上の矩形(ピクセル座標、左上原点)。fb-server の重なり調停用。
#[derive(Serialize, Clone, Copy)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// 接続直後に1行だけ送る申告メッセージ。
/// `rect` を申告すると、fb-server が下位レイヤーへ「この矩形を避けて描け」と
/// 伝える(task-var のバーは固定領域なので接続時に一度だけ申告する)。
#[derive(Serialize)]
struct Hello {
    hello: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rect: Option<Rect>,
}

/// サーバーから届く可視性通知。
#[derive(Deserialize)]
struct RawVisible {
    visible: bool,
}

/// ソケットパス: `$FB_SERVER_SOCK` > `$XDG_RUNTIME_DIR/fb-server.sock` > `/tmp/...`。
fn socket_path() -> String {
    if let Ok(p) = std::env::var("FB_SERVER_SOCK") {
        if !p.is_empty() {
            return p;
        }
    }
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(d) if !d.is_empty() => format!("{d}/fb-server.sock"),
        _ => "/tmp/fb-server.sock".to_string(),
    }
}

/// fb-client スレッドを起動する(detached)。切断・接続失敗時は再接続し続ける。
/// fb-server が起動していない間は既定で visible=true のまま(main 側の初期値に従う)。
pub fn spawn(name: &'static str, rect: Option<Rect>, tx: Sender<bool>) {
    std::thread::spawn(move || loop {
        if let Err(e) = session(name, rect, &tx) {
            eprintln!("task-var: fb-server 接続待ち ({e})");
        }
        std::thread::sleep(Duration::from_millis(500));
    });
}

/// 1接続ぶんの受信ループ。EOF / エラーで戻る(呼び出し側が張り直す)。
fn session(name: &'static str, rect: Option<Rect>, tx: &Sender<bool>) -> std::io::Result<()> {
    let stream = UnixStream::connect(socket_path())?;
    let hello = Hello { hello: name, rect };
    let line = serde_json::to_string(&hello).unwrap_or_default();
    (&stream).write_all(format!("{line}\n").as_bytes())?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match (&stream).read(&mut chunk) {
            Ok(0) => return Ok(()), // サーバー切断
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let l: Vec<u8> = buf.drain(..=pos).collect();
                    if let Ok(v) = serde_json::from_slice::<RawVisible>(&l) {
                        // 受信側(main)が落ちていたら終了
                        if tx.send(v.visible).is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                // タイムアウトは何もせず継続(再接続判定用の周期起こしのみ)
            }
            Err(e) => return Err(e),
        }
    }
}
