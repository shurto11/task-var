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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

impl PlayerState {
    /// ボタンを押した直後に期待される状態。
    pub fn after(self, ctrl: Ctrl) -> Self {
        match ctrl {
            Ctrl::Shuffle => Self { shuffle: !self.shuffle, ..self },
            Ctrl::Repeat => Self { repeat: self.repeat.cycle(), ..self },
            Ctrl::PlayPause => Self { playing: !self.playing, ..self },
            Ctrl::Prev | Ctrl::Next => self,
        }
    }

    /// `ctrl` が対象とするフィールドだけを `want` の値で上書きする。
    fn overlay(&mut self, ctrl: Ctrl, want: &Self) {
        match ctrl {
            Ctrl::Shuffle => self.shuffle = want.shuffle,
            Ctrl::Repeat => self.repeat = want.repeat,
            Ctrl::PlayPause => self.playing = want.playing,
            Ctrl::Prev | Ctrl::Next => {}
        }
    }

    /// `ctrl` の操作が反映されたか(対象フィールドだけを見る)。
    fn reflects(&self, ctrl: Ctrl, want: &Self) -> bool {
        match ctrl {
            Ctrl::Shuffle => self.shuffle == want.shuffle,
            Ctrl::Repeat => self.repeat == want.repeat,
            Ctrl::PlayPause => self.playing == want.playing,
            // 曲送りは状態で確認できないので即座に確定扱い
            Ctrl::Prev | Ctrl::Next => true,
        }
    }
}

/// ボタンを押してからポーリングが追いつくまでの間、押した結果を表示に反映して
/// おくための保留値。spotatui 側の反映が遅いときに一瞬元へ戻って見えるのを防ぐ。
/// 期限切れまでに反映されなければ諦めて実際の値へ戻す(= 効いていないことが分かる)。
pub struct Pending {
    ctrl: Ctrl,
    want: PlayerState,
    until: Instant,
}

/// 保留を持てる時間。
const HOLD: Duration = Duration::from_secs(5);

impl Pending {
    pub fn new(ctrl: Ctrl, want: PlayerState) -> Self {
        Self { ctrl, want, until: Instant::now() + HOLD }
    }

    /// ポーリング結果 `got` に保留値をかぶせる。反映済み/期限切れなら false を
    /// 返す(呼び出し側が保留を捨てる)。
    pub fn overlay(&self, got: &mut PlayerState, now: Instant) -> bool {
        if got.reflects(self.ctrl, &self.want) || now >= self.until {
            return false;
        }
        got.overlay(self.ctrl, &self.want);
        true
    }
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
            // spotatui の PlayPause は自前の再生フラグで分岐する。そのフラグが
            // 実際の再生とずれていると逆の操作(=停止のつもりが曲送り)になるため、
            // こちらの判定で Pause / Play を明示して送る。
            Ctrl::PlayPause => {
                cmd.args(["call", DEST, OBJ, IFACE, if cur.playing { "Pause" } else { "Play" }])
            }
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

/// ポーリング間隔。
const POLL_EVERY: Duration = Duration::from_secs(1);
/// Position がこれ以上進んでいれば「動いた」とみなす。
const POS_EPSILON_MS: u64 = 200;
/// 動かない状態がこれだけ続いて初めて停止とみなす。
///
/// spotatui の Position は **1 秒刻みでしか動かず、しかも ±250ms ほど前後する**
/// (実機で 0.4 秒間隔に採って確認した)。ポーリングも 1 秒間隔なので、位相が
/// 重なると再生中でも差が 0 になる回が現れる。1 回ぶんで停止と判定すると、
/// そこだけ再生ボタンの表示が入れ替わってすぐ戻る。刻み 2 回ぶんは待つ。
const STALL_FOR: Duration = Duration::from_millis(2500);
/// これ以上戻っていたらシークか 1 曲リピートの頭出し。止まってはいないので
/// 基準を取り直して再生中のままにする。
const REWIND_MS: u64 = 1000;

/// Position の進み具合から再生中かを判定するための基準点。
///
/// spotatui の PlaybackStatus は、librespot が非アクティブなデバイスを掴んで
/// いると実際の再生と食い違う(実機で Position は進むのに Stopped のままになる
/// のを確認した)ため、Position の方を正とする。
#[derive(Debug)]
struct PosWatch {
    track: String,
    /// 最後に「動いた」と認めたときの Position。
    pos: u64,
    /// そのときの時刻。
    moved: Instant,
}

impl PosWatch {
    /// 新しいサンプルを与えて、再生中かどうかを返す。
    /// 曲が変わった直後など比較できないときは None(呼び出し側は
    /// PlaybackStatus をそのまま使う)。
    fn update(cur: &mut Option<Self>, np: &NowPlaying, now: Instant) -> Option<bool> {
        match cur {
            Some(w) if w.track == np.track => {
                let pos = np.progress_ms;
                if pos > w.pos + POS_EPSILON_MS || pos + REWIND_MS < w.pos {
                    // 進んだ / 巻き戻った。どちらも動いている証拠
                    w.pos = pos;
                    w.moved = now;
                    Some(true)
                } else {
                    // 1 秒刻みの谷間か、実際に止まっているか。
                    // 動かないまま STALL_FOR を過ぎたら停止と決める
                    Some(now.duration_since(w.moved) < STALL_FOR)
                }
            }
            _ => {
                *cur = Some(Self { track: np.track.clone(), pos: np.progress_ms, moved: now });
                None
            }
        }
    }
}

/// 1 秒間隔で状態をポーリングするスレッドを起動する(detached)。
pub fn spawn(shared: Arc<Mutex<Option<Snapshot>>>) {
    std::thread::spawn(move || {
        let mut watch: Option<PosWatch> = None;
        loop {
            let mut snap = poll();
            match snap.as_mut().and_then(|s| s.np.as_mut().map(|np| (&mut s.player, np))) {
                Some((player, np)) => {
                    if let Some(playing) = PosWatch::update(&mut watch, np, Instant::now()) {
                        player.playing = playing;
                        np.is_playing = playing; // 進捗の補間もこれに従う
                    }
                }
                // 曲が取れないなら基準も捨てる(次に来た曲で取り直す)
                None => watch = None,
            }
            *shared.lock().unwrap() = snap;
            std::thread::sleep(POLL_EVERY);
        }
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

    fn st(playing: bool, shuffle: bool, repeat: Loop) -> PlayerState {
        PlayerState { playing, shuffle, repeat }
    }

    #[test]
    fn playing_follows_the_position_not_the_status() {
        let np_at = |track: &str, ms: u64| NowPlaying {
            track: track.into(),
            artist: "a".into(),
            art_url: None,
            progress_ms: ms,
            duration_ms: 100_000,
            written_at_ms: 0,
            is_playing: false,
        };
        let t0 = Instant::now();
        let at = |sec: u64| t0 + Duration::from_secs(sec);
        let mut w = None;

        // 初回は基準が無いので判定できない
        assert_eq!(PosWatch::update(&mut w, &np_at("HOP", 5_000), t0), None);
        // 進んでいれば再生中(PlaybackStatus が Stopped でもこちらを採る)
        assert_eq!(PosWatch::update(&mut w, &np_at("HOP", 6_000), at(1)), Some(true));
        // Position は 1 秒刻みなので、同じ値が返る回がある。ここで止めない
        assert_eq!(PosWatch::update(&mut w, &np_at("HOP", 6_000), at(2)), Some(true));
        // 250ms ほど後戻りするのも実機では普通(これも止めない)
        assert_eq!(PosWatch::update(&mut w, &np_at("HOP", 5_750), at(3)), Some(true));
        // 動かないまま STALL_FOR を過ぎたら停止
        assert_eq!(PosWatch::update(&mut w, &np_at("HOP", 6_000), at(4)), Some(false));
        // また進めば再生中へ戻る
        assert_eq!(PosWatch::update(&mut w, &np_at("HOP", 7_000), at(5)), Some(true));
        // 頭出し(1 曲リピート)は「動いた」扱い。止まってはいない
        assert_eq!(PosWatch::update(&mut w, &np_at("HOP", 300), at(6)), Some(true));

        // 曲が変わった直後は基準を取り直すので判定できない
        assert_eq!(PosWatch::update(&mut w, &np_at("LOCO", 1_000), at(7)), None);
        assert_eq!(PosWatch::update(&mut w, &np_at("LOCO", 2_000), at(8)), Some(true));
    }

    #[test]
    fn pending_holds_until_reflected_or_expired() {
        let now = std::time::Instant::now();
        let cur = st(true, false, Loop::Off);
        let want = cur.after(Ctrl::Shuffle);
        assert!(want.shuffle, "シャッフルはトグルされる");
        let p = Pending::new(Ctrl::Shuffle, want);

        // まだ反映されていない間は保留値をかぶせる
        let mut got = cur;
        assert!(p.overlay(&mut got, now));
        assert!(got.shuffle, "押した直後は ON に見える");

        // 反映されたら保留を捨てる
        let mut got = st(true, true, Loop::Off);
        assert!(!p.overlay(&mut got, now));

        // 期限切れでも捨てる(効いていないことが表示に出る)
        let mut got = cur;
        assert!(!p.overlay(&mut got, now + HOLD + Duration::from_secs(1)));

        // 対象外のフィールドは触らない
        let p = Pending::new(Ctrl::Repeat, cur.after(Ctrl::Repeat));
        let mut got = st(false, true, Loop::Off);
        assert!(p.overlay(&mut got, now));
        assert_eq!(got.repeat, Loop::Playlist);
        assert!(!got.playing && got.shuffle, "repeat 以外は実際の値のまま");
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
