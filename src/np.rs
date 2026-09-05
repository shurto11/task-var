//! `/tmp/spotatui_np.json` の読み取り。
//!
//! このファイルは spotatui 本体が再生ポーリングごとに書いている
//! (spotatui/src/infra/network/playback.rs の `write_now_playing_info`)。
//! 更新間隔は外部デバイス再生で 1 秒、ネイティブストリーミングでは 5 秒まで
//! 開くので、進捗は `written_at_ms` からの経過時間で補間する必要がある。
//!
//! 書き込みは `File::create` による truncate で、アトミックな rename でも
//! ロックでもない。読み取り中の部分書き込みに当たることがあるため、
//! パース失敗は `None` を返し、呼び出し側が前回値を保持する。

use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

const NP_FILE: &str = "/tmp/spotatui_np.json";

/// この時間より古い JSON は「もう更新されていない」とみなす。
/// spotatui は最長 5 秒間隔(ネイティブ再生時)で書くので、その 3 倍を取る。
const FRESH_FOR_MS: u64 = 15_000;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct NowPlaying {
    pub track: String,
    pub artist: String,
    /// アルバムアートの URL。Spotify の画像配列の先頭 = 最大サイズ(640x640)。
    #[serde(default)]
    pub art_url: Option<String>,
    #[serde(default)]
    pub progress_ms: u64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub written_at_ms: u64,
    #[serde(default = "default_true")]
    pub is_playing: bool,
}

impl NowPlaying {
    /// まだ更新され続けているか。spotatui が Web API 経路を通らない設定だと
    /// このファイルは更新されないため、古ければ MPRIS 由来の情報へ切り替える。
    pub fn is_fresh(&self) -> bool {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.written_at_ms > 0 && now_ms.saturating_sub(self.written_at_ms) <= FRESH_FOR_MS
    }

    /// 0.0..=1.0 の再生位置。ファイル書き込みからの経過時間を足して補間する
    /// (停止中は補間しない)。
    pub fn progress_ratio(&self) -> f32 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(self.written_at_ms);
        let elapsed = if self.is_playing && self.written_at_ms > 0 {
            now_ms.saturating_sub(self.written_at_ms)
        } else {
            0
        };
        let current = (self.progress_ms + elapsed).min(self.duration_ms);
        current as f32 / self.duration_ms as f32
    }
}

/// 現在の再生情報を読む。ファイルが無い/壊れている場合は None。
pub fn read() -> Option<NowPlaying> {
    let text = std::fs::read_to_string(NP_FILE).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn np(progress_ms: u64, written_at_ms: u64, is_playing: bool) -> NowPlaying {
        NowPlaying {
            track: "t".into(),
            artist: "a".into(),
            art_url: None,
            progress_ms,
            duration_ms: 200_000,
            written_at_ms,
            is_playing,
        }
    }

    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
    }

    #[test]
    fn progress_is_interpolated_only_while_playing() {
        // 10 秒前に「50 秒地点」と書かれた → 再生中なら約 60 秒地点
        let playing = np(50_000, now_ms() - 10_000, true);
        let r = playing.progress_ratio() * 200_000.0;
        assert!((59_000.0..=61_000.0).contains(&r), "補間されるはず: {r}");

        // 停止中は書かれた値のまま
        let paused = np(50_000, now_ms() - 10_000, false);
        assert!((paused.progress_ratio() * 200_000.0 - 50_000.0).abs() < 1.0);
    }

    #[test]
    fn progress_is_clamped_and_safe_without_duration() {
        // 曲を跨いで長く放置しても 1.0 を超えない
        let stale = np(190_000, now_ms() - 600_000, true);
        assert_eq!(stale.progress_ratio(), 1.0);

        let mut zero = np(0, now_ms(), true);
        zero.duration_ms = 0;
        assert_eq!(zero.progress_ratio(), 0.0);
    }

    #[test]
    fn freshness_follows_written_at() {
        assert!(np(0, now_ms() - 2_000, true).is_fresh(), "2 秒前は新鮮");
        assert!(!np(0, now_ms() - 60_000, true).is_fresh(), "1 分前は古い");
        assert!(!np(0, 0, true).is_fresh(), "written_at_ms が無いものは古い扱い");
    }

    #[test]
    fn parses_the_writer_side_json() {
        // spotatui の write_now_playing_info が出す形そのもの
        let json = r#"{"track":"I mean, It's about time","artist":"NCT 127",
            "art_url":"https://i.scdn.co/image/abc","progress_ms":42620,
            "duration_ms":128000,"written_at_ms":1712345678000,"is_playing":true}"#;
        let np: NowPlaying = serde_json::from_str(json).unwrap();
        assert_eq!(np.track, "I mean, It's about time");
        assert_eq!(np.art_url.as_deref(), Some("https://i.scdn.co/image/abc"));
        assert!(np.is_playing);

        // art_url は null になりうる / 部分書き込みは弾かれる
        let no_art = r#"{"track":"t","artist":"a","art_url":null}"#;
        assert!(serde_json::from_str::<NowPlaying>(no_art).is_ok());
        assert!(serde_json::from_str::<NowPlaying>(r#"{"track":"t","art"#).is_err());
    }
}
