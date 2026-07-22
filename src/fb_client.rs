//! fb-server クライアント。touch_client.rs を踏襲。
//!
//! 起動時に自分の名前("task-var")を hello として申告し、
//! {"visible":bool, "scene":..} を受け取って main へ渡す。
//! バーの描画領域(rect)は表示状態に応じて動的に申告する: 共有 rect を
//! Some(..) にするとその矩形を申告し(下位レイヤーがそこを避ける)、None に
//! すると取り消す。スワイプ表示モードでバーを出す間だけ Some にすることで、
//! fbhalf の全面表示を妨げずにバーだけを重ねられる。
//! (Hello 送信 → set_read_timeout → chunk 読み → \n 区切り JSON → 切断で張り直し)

use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// フレームバッファ上の矩形(物理ピクセル座標、左上原点)。fb-server の調停用。
#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// main へ渡す可視性通知。`scene` は現在のシーン名(挙動分岐用)。
#[derive(Debug, Clone)]
pub struct VisMsg {
    pub visible: bool,
    pub scene: Option<String>,
}

/// 接続直後に1行だけ送る申告メッセージ。
#[derive(Serialize)]
struct Hello {
    hello: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rect: Option<Rect>,
}

/// 描画領域が変わったときに送る更新メッセージ。
#[derive(Serialize)]
struct RectUpdate {
    rect: Option<Rect>,
}

/// サーバーから届く可視性通知。
#[derive(Deserialize)]
struct RawVisible {
    visible: bool,
    #[serde(default)]
    scene: Option<String>,
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
/// `rect` は現在申告すべき描画領域(共有)。main が更新すると次のポーリングで
/// サーバーへ申告し直す。fb-server 未起動中は visible=true のまま。
pub fn spawn(name: &'static str, rect: Arc<Mutex<Option<Rect>>>, tx: Sender<VisMsg>) {
    std::thread::spawn(move || loop {
        if let Err(e) = session(name, &rect, &tx) {
            eprintln!("task-var: fb-server 接続待ち ({e})");
        }
        std::thread::sleep(Duration::from_millis(500));
    });
}

/// 1接続ぶんの受信ループ。EOF / エラーで戻る(呼び出し側が張り直す)。
fn session(
    name: &'static str,
    rect: &Arc<Mutex<Option<Rect>>>,
    tx: &Sender<VisMsg>,
) -> std::io::Result<()> {
    let stream = UnixStream::connect(socket_path())?;
    let mut cur = *rect.lock().unwrap();
    let hello = Hello { hello: name, rect: cur };
    let line = serde_json::to_string(&hello).unwrap_or_default();
    (&stream).write_all(format!("{line}\n").as_bytes())?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        // バーの表示/非表示で描画領域が変わっていたら申告し直す。
        let now = *rect.lock().unwrap();
        if now != cur {
            cur = now;
            let upd = serde_json::to_string(&RectUpdate { rect: now }).unwrap_or_default();
            (&stream).write_all(format!("{upd}\n").as_bytes())?;
        }
        match (&stream).read(&mut chunk) {
            Ok(0) => return Ok(()), // サーバー切断
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let l: Vec<u8> = buf.drain(..=pos).collect();
                    if let Ok(v) = serde_json::from_slice::<RawVisible>(&l) {
                        let msg = VisMsg { visible: v.visible, scene: v.scene };
                        if tx.send(msg).is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(e) => return Err(e),
        }
    }
}
