//! spotatui の MPRIS(`org.mpris.MediaPlayer2.spotatui`)を `busctl --user`
//! 経由で読み書きする。
//!
//! D-Bus クレート(zbus)は依存が重いわりに用途が「3 プロパティの読み取りと
//! 5 種類の操作」しかないので、tmux/stty/pgrep と同じくサブプロセスで済ませる。
//!
//! 取れるものは 1 回の `get-property` でまとめて取る:
//! PlaybackStatus / Shuffle / LoopStatus / Position / Metadata。
//!
//! 曲情報(曲名・アーティスト・アート・進捗)の第一候補は np.rs が読む
//! `/tmp/spotatui_np.json` だが、あれは spotatui の **Web API ポーリング経路**
//! でしか書かれない。ネイティブ再生中は current_playback_context が埋まらず
//! ファイルが更新されないことを実機で確認したため、MPRIS の Metadata / Position
//! から同じ形を組み立てて代替できるようにしてある。
//! 併せて「MPRIS が応答する = spotatui が生きている」をパネルの表示条件に使う。

use crate::np::NowPlaying;
use serde_json::Value;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEST: &str = "org.mpris.MediaPlayer2.spotatui";
const OBJ: &str = "/org/mpris/MediaPlayer2";
const IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// MPRIS の LoopStatus。文字列は "None" / "Track" / "Playlist"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Loop {
    #[default]
    Off,
    Track,
    Playlist,
}

impl Loop {
    fn as_str(self) -> &'static str {
        match self {
            Loop::Off => "None",
            Loop::Track => "Track",
            Loop::Playlist => "Playlist",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "Track" => Loop::Track,
            "Playlist" => Loop::Playlist,
            _ => Loop::Off,
        }
    }

    /// リピートボタンを押したときの巡回: なし → 全曲 → 1 曲 → なし。
    pub fn cycle(self) -> Self {
        match self {
            Loop::Off => Loop::Playlist,
            Loop::Playlist => Loop::Track,
            Loop::Track => Loop::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerState {
    pub playing: bool,
    pub shuffle: bool,
    pub repeat: Loop,
}

/// 1 回のポーリングで取れたもの。`np` は Metadata から組み立てた曲情報
/// (トラックが無いときは None)。
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub player: PlayerState,
    pub np: Option<NowPlaying>,
}

/// パネル上の操作ボタン。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctrl {
    Shuffle,
    Prev,
    PlayPause,
    Next,
    Repeat,
}

/// busctl を起動する。task-var は fbterm から起動されるので通常 environ に
/// DBUS_SESSION_BUS_ADDRESS があるが、無い場合に備えて補っておく。
fn busctl() -> Command {
    let mut c = Command::new("busctl");
    c.arg("--user");
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            c.env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={dir}/bus"));
        }
    }
    c
}

/// busctl --json=short の 1 行(`{"type":..,"data":..}`)から data を取り出す。
fn data(line: &str) -> Option<Value> {
    let mut v: Value = serde_json::from_str(line).ok()?;
    Some(v.get_mut("data")?.take())
}

/// Metadata(a{sv})の値も `{"type":..,"data":..}` で包まれている。
fn entry<'a>(meta: &'a Value, key: &str) -> Option<&'a Value> {
    meta.get(key)?.get("data")
}

/// Metadata + Position + PlaybackStatus から np.rs と同じ形の曲情報を組み立てる。
fn now_playing(meta: &Value, position_us: u64, playing: bool) -> Option<NowPlaying> {
    let track = entry(meta, "xesam:title")?.as_str()?.to_owned();
    let artist = match entry(meta, "xesam:artist") {
        Some(Value::Array(a)) => {
            a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
        }
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some(NowPlaying {
        track,
        artist,
        art_url: entry(meta, "mpris:artUrl").and_then(Value::as_str).map(str::to_owned),
        progress_ms: position_us / 1000,
        // mpris:length はマイクロ秒。負値はありえないが念のため飽和させる
        duration_ms: entry(meta, "mpris:length").and_then(Value::as_i64).unwrap_or(0).max(0)
            as u64
            / 1000,
        written_at_ms: now_ms,
        is_playing: playing,
    })
}

/// 再生状態と曲情報を 1 回の呼び出しで取る。
/// spotatui が居ない(= MPRIS 名が無い)ときは None。
pub fn poll() -> Option<Snapshot> {
    let out = busctl()
        .args([
            "--json=short",
            "get-property",
            DEST,
            OBJ,
            IFACE,
            "PlaybackStatus",
            "Shuffle",
            "LoopStatus",
            "Position",
            "Metadata",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    // 引数の順にプロパティが 1 行ずつ返る
    let status = data(lines.next()?)?;
    let shuffle = data(lines.next()?)?;
    let repeat = data(lines.next()?)?;
    let position = data(lines.next()?)?;
    let meta = data(lines.next()?)?;

    let playing = status.as_str() == Some("Playing");
    let player = PlayerState {
        playing,
        shuffle: shuffle.as_bool().unwrap_or(false),
        repeat: Loop::parse(repeat.as_str().unwrap_or("None")),
    };
    let position_us = position.as_i64().unwrap_or(0).max(0) as u64;
    Some(Snapshot { player, np: now_playing(&meta, position_us, playing) })
}

/// ボタン操作を MPRIS へ送る。タッチ処理をブロックしないよう別スレッドで投げる
/// (呼び出し側は手元の PlayerState を先に更新して即座に描き直す)。
pub fn activate(ctrl: Ctrl, cur: PlayerState) {
    std::thread::spawn(move || {
        let mut cmd = busctl();
        match ctrl {
            Ctrl::Prev => cmd.args(["call", DEST, OBJ, IFACE, "Previous"]),
            Ctrl::Next => cmd.args(["call", DEST, OBJ, IFACE, "Next"]),
            Ctrl::PlayPause => cmd.args(["call", DEST, OBJ, IFACE, "PlayPause"]),
            Ctrl::Shuffle => cmd.args([
                "set-property",
                DEST,
                OBJ,
                IFACE,
                "Shuffle",
                "b",
                if cur.shuffle { "false" } else { "true" },
            ]),
            Ctrl::Repeat => {
                cmd.args(["set-property", DEST, OBJ, IFACE, "LoopStatus", "s", cur.repeat.cycle().as_str()])
            }
        };
        match cmd.stdout(Stdio::null()).stderr(Stdio::piped()).output() {
            Ok(o) if !o.status.success() => {
                eprintln!("task-var: MPRIS {ctrl:?} に失敗: {}", String::from_utf8_lossy(&o.stderr).trim())
            }
            Err(e) => eprintln!("task-var: busctl を実行できません: {e}"),
            _ => {}
        }
    });
}

/// 1 秒間隔で状態をポーリングするスレッドを起動する(detached)。
pub fn spawn(shared: Arc<Mutex<Option<Snapshot>>>) {
    std::thread::spawn(move || loop {
        let s = poll();
        *shared.lock().unwrap() = s;
        std::thread::sleep(Duration::from_secs(1));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_now_playing_from_mpris_metadata() {
        // 実機の busctl --json=short が返す Metadata そのもの
        let meta: Value = serde_json::from_str(
            r#"{"mpris:artUrl":{"type":"s","data":"https://i.scdn.co/image/abc"},
                "mpris:length":{"type":"x","data":141285000},
                "xesam:album":{"type":"s","data":"WhyKiiiKiii"},
                "xesam:artist":{"type":"as","data":["KiiiKiii","NCT 127"]},
                "xesam:title":{"type":"s","data":"Pop Off Pop Off"}}"#,
        )
        .unwrap();
        let np = now_playing(&meta, 57_876_000, true).unwrap();
        assert_eq!(np.track, "Pop Off Pop Off");
        assert_eq!(np.artist, "KiiiKiii, NCT 127", "複数アーティストは , で連結");
        assert_eq!(np.art_url.as_deref(), Some("https://i.scdn.co/image/abc"));
        // マイクロ秒 → ミリ秒
        assert_eq!(np.progress_ms, 57_876);
        assert_eq!(np.duration_ms, 141_285);
        assert!(np.is_playing);

        // 曲が無いとき(Metadata が空)は None
        assert!(now_playing(&Value::Object(Default::default()), 0, false).is_none());
    }

    #[test]
    fn unwraps_busctl_json_lines() {
        assert_eq!(data(r#"{"type":"s","data":"Playing"}"#), Some(Value::from("Playing")));
        assert_eq!(data(r#"{"type":"b","data":true}"#), Some(Value::from(true)));
        assert_eq!(data("not json"), None);
    }

    #[test]
    fn loop_cycles_off_playlist_track() {
        assert_eq!(Loop::Off.cycle(), Loop::Playlist);
        assert_eq!(Loop::Playlist.cycle(), Loop::Track);
        assert_eq!(Loop::Track.cycle(), Loop::Off);
        // MPRIS 上の綴りと往復する
        for l in [Loop::Off, Loop::Track, Loop::Playlist] {
            assert_eq!(Loop::parse(l.as_str()), l);
        }
    }
}
