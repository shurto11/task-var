//! Melon TOP100 チャートでの、追っている数曲の順位取得。
//!
//! 取得元は 가이섬 (guyso.me) の曲 trend ページ
//! `/chart/melon/top100/trend/ranking/{songId}`。Next.js 製なので HTML の
//! `__NEXT_DATA__` から JSON を抜き出して読む(HTML のパースは要らない)。
//! 1 回の取得で曲名・アーティスト・アルバム画像と、1 時間ごとの順位の
//! 直近 31 日ぶんが揃う。最後の点が最新の時台で、圏外の時台は
//! ranking / previous が null になっている。
//!
//! Melon のチャートは毎正時に更新されるが、guyso.me に出るまで数分かかる
//! ことがある。正時を過ぎたら取りに行き、まだ今の時台が出ていない曲だけを
//! 出るまで短い間隔で取り直す。
//!
//! 枠には 1 曲ずつ、`ROTATE_EVERY` ごとに切り替えて出す(`rotation`)。
//!
//! 取得はアルバムアートと同じく curl のサブプロセスで行う(art.rs 参照)。

use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 既定で追う曲(Melon の songId)。並びが枠に出す順番。
/// NMIXX「긴 소원 (Birthday Wish)」「Blue Valentine」「Heavy Serenade」。
/// `TASKVAR_MELON_SONGS`(カンマ区切り)で変えられる。
const SONG_IDS: [&str; 3] = ["603442244", "600243411", "601879407"];
/// 枠に出す曲を切り替える間隔。
pub const ROTATE_EVERY: Duration = Duration::from_secs(10);
const BASE: &str = "https://guyso.me/chart/melon/top100/trend/ranking";
/// UA が無いと弾かれる。
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko)";
/// チャートの時刻は韓国時間(UTC+9)。
const KST_OFFSET: i64 = 9 * 3600;
/// 正時からこれだけ待ってから最初に取りに行く。
const FIRST_TRY_AFTER: i64 = 60;
/// 今の時台がまだ出ていないときの取り直し間隔。正時から `RETRY_FAST_FOR` までは
/// 短く、それを過ぎたら(サイトが止まっているなど)間隔を開ける。
/// 取得そのものに失敗した(ネットが無い)ときは、時刻によらず短い方で取り直す。
const RETRY_FAST: i64 = 60;
const RETRY_SLOW: i64 = 300;
const RETRY_FAST_FOR: i64 = 20 * 60;
/// 待つときの 1 回の眠りの上限。`thread::sleep` は CLOCK_MONOTONIC で測るので
/// サスペンド中は進まない。長く 1 回で寝ると、復帰しても寝ていたぶんだけ
/// 取りに行くのが遅れる(16:01 に 1 時間寝て 16:14〜17:35 にサスペンドすると
/// 18:22 まで起きない)。これごとに起きて壁時計で期限を確かめる。
const NAP: i64 = 15;

/// パネルに出す 1 時台ぶんの内容。
#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub title: String,
    pub artist: String,
    pub art_url: Option<String>,
    /// 順位。圏外なら None。
    pub rank: Option<u32>,
    /// 前の時台の順位。新規ランクインは 0 か None で届く。
    pub previous: Option<u32>,
    /// 集計の時台(KST)。`2026-10-21T14:00:00` の形。
    pub time: String,
}

/// 前の時台からの動き。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up(u32),
    Down(u32),
    Same,
    New,
    /// 圏外
    Out,
}

impl Chart {
    pub fn movement(&self) -> Move {
        let Some(rank) = self.rank else { return Move::Out };
        match self.previous {
            None | Some(0) => Move::New,
            Some(p) if p > rank => Move::Up(p - rank),
            Some(p) if p < rank => Move::Down(rank - p),
            Some(_) => Move::Same,
        }
    }

    /// 時台の表示。今日(KST)なら `14:00`、それより前なら `10/20 14:00`。
    pub fn time_label(&self, now_unix: i64) -> String {
        let Some(hour) = hour_index(&self.time) else { return String::new() };
        let h = hour.rem_euclid(24);
        if hour.div_euclid(24) == (now_unix + KST_OFFSET).div_euclid(86400) {
            return format!("{h:02}:00");
        }
        let (m, d) = (self.time.get(5..7).unwrap_or("?"), self.time.get(8..10).unwrap_or("?"));
        format!("{}/{} {h:02}:00", m.trim_start_matches('0'), d.trim_start_matches('0'))
    }
}

/// 追う曲の一覧。`TASKVAR_MELON_SONGS`(カンマ区切り)が無ければ `SONG_IDS`。
fn songs() -> Vec<String> {
    let from_env = std::env::var("TASKVAR_MELON_SONGS").ok().map(|v| {
        v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>()
    });
    match from_env {
        Some(v) if !v.is_empty() => v,
        _ => SONG_IDS.iter().map(|s| s.to_string()).collect(),
    }
}

/// 取得スレッドを起動する(detached)。どれかの曲が新しく取れるたびに、
/// 取れている曲を追う順に並べて `shared` へ置く(1 度も取れていない曲は抜ける)。
pub fn spawn(shared: Arc<Mutex<Option<Vec<Chart>>>>) {
    let songs = songs();
    std::thread::spawn(move || {
        let mut charts: Vec<Option<Chart>> = vec![None; songs.len()];
        loop {
            let hour = (now_unix() + KST_OFFSET).div_euclid(3600);
            let (mut changed, mut failed) = (false, false);
            for (song, slot) in songs.iter().zip(charts.iter_mut()) {
                // 今の時台まで取れている曲は取り直さない
                if slot.as_ref().is_some_and(|c| is_current(c, hour)) {
                    continue;
                }
                match fetch(song) {
                    Some(c) => {
                        if slot.as_ref().map(|old| &old.time) != Some(&c.time) {
                            eprintln!(
                                "task-var: Melon {} {}: {:?} (前 {:?})",
                                c.time, c.title, c.rank, c.previous
                            );
                        }
                        changed |= slot.as_ref() != Some(&c);
                        *slot = Some(c);
                    }
                    None => {
                        eprintln!("task-var: Melon チャートの取得に失敗 (song {song})");
                        failed = true;
                    }
                }
            }
            if changed {
                *shared.lock().unwrap() = Some(charts.iter().flatten().cloned().collect());
            }
            // いちばん遅れている曲に合わせて次を決める(取れていない曲があれば None)
            let latest = charts.iter().map(|c| c.as_ref().and_then(|c| hour_index(&c.time))).min();
            let now = now_unix();
            sleep_until(now + next_wait(latest.flatten(), failed, now).as_secs() as i64);
        }
    });
}

/// 今の時台(KST の通し時数 `hour`)まで出ているか。
fn is_current(c: &Chart, hour: i64) -> bool {
    hour_index(&c.time).is_some_and(|h| h >= hour)
}

/// `len` 曲を `ROTATE_EVERY` ごとに切り替えるとき、`elapsed` の時点で出す曲。
/// 1 曲も無ければ None。
pub fn rotation(elapsed: Duration, len: usize) -> Option<usize> {
    (len > 0).then(|| (elapsed.as_secs() / ROTATE_EVERY.as_secs()) as usize % len)
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 壁時計(unix 秒)で `deadline` になるまで待つ。サスペンドから復帰したら
/// `NAP` 以内に気付いて戻る。
fn sleep_until(deadline: i64) {
    loop {
        let left = deadline - now_unix();
        if left <= 0 {
            return;
        }
        std::thread::sleep(Duration::from_secs(left.min(NAP) as u64));
    }
}

/// 次に取りに行くまでの待ち時間。`latest` は取れた最新の時台(`hour_index`)で、
/// まだ取れていない曲があれば None。`failed` はこの回に取得そのものが
/// 失敗した曲があったか(ネットが無い・サイトが落ちている)。
fn next_wait(latest: Option<i64>, failed: bool, now_unix: i64) -> Duration {
    let now = now_unix + KST_OFFSET;
    let hour = now.div_euclid(3600);
    // 次の正時を少し過ぎたところ。どの場合もここより先へは寝過ごさない
    let next_hour = (hour + 1) * 3600 + FIRST_TRY_AFTER - now;
    let secs = match latest {
        // 取得に失敗した。つながり直したらすぐ追いつけるよう短い間隔で取り直す
        _ if failed => RETRY_FAST.min(next_hour),
        // 今の時台まで出ている。次の更新まで待つ
        Some(h) if h >= hour => next_hour,
        // まだ前の時台のまま(更新待ち)
        _ if now - hour * 3600 < RETRY_FAST_FOR => RETRY_FAST.min(next_hour),
        _ => RETRY_SLOW.min(next_hour),
    };
    Duration::from_secs(secs.max(1) as u64)
}

/// `2026-10-21T14:00:00` を 1970-01-01T00 からの通し時数にする。
fn hour_index(time: &str) -> Option<i64> {
    let num = |r: std::ops::Range<usize>| time.get(r)?.parse::<i64>().ok();
    let (y, m, d, h) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(0..24).contains(&h) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 24 + h)
}

/// 1970-01-01 からの日数(proleptic グレゴリオ暦。H. Hinnant の days_from_civil)。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn fetch(song: &str) -> Option<Chart> {
    let out = std::process::Command::new("curl")
        .args(["-sL", "--compressed", "--max-time", "30", "-A", UA])
        .arg(format!("{BASE}/{song}"))
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse(&String::from_utf8_lossy(&out.stdout))
}

#[derive(Deserialize)]
struct Page {
    props: Props,
}

#[derive(Deserialize)]
struct Props {
    #[serde(rename = "pageProps")]
    page_props: PageProps,
}

#[derive(Deserialize)]
struct PageProps {
    data: Trend,
    song: Song,
}

#[derive(Deserialize)]
struct Trend {
    data: Vec<Point>,
}

#[derive(Deserialize)]
struct Point {
    #[serde(default)]
    ranking: Option<u32>,
    #[serde(default)]
    previous: Option<u32>,
    time: String,
}

#[derive(Deserialize)]
struct Song {
    name: String,
    #[serde(default)]
    artists: Vec<Artist>,
    #[serde(default)]
    album: Option<Album>,
}

#[derive(Deserialize)]
struct Artist {
    name: String,
}

#[derive(Deserialize)]
struct Album {
    id: String,
    #[serde(default)]
    image: Option<String>,
}

/// trend ページの HTML から最新の時台を取り出す。
fn parse(html: &str) -> Option<Chart> {
    const OPEN: &str = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let rest = &html[html.find(OPEN)? + OPEN.len()..];
    let json = &rest[..rest.find("</script>")?];
    let p = serde_json::from_str::<Page>(json).ok()?.props.page_props;
    let latest = p.data.data.into_iter().max_by(|a, b| a.time.cmp(&b.time))?;
    // アルバム画像は album.image が本命。無いときは guyso の CDN から引く。
    let art_url = p.song.album.map(|a| {
        a.image.unwrap_or_else(|| format!("https://cdn.guyso.me/image/album/melon/{}.jpg", a.id))
    });
    Some(Chart {
        title: p.song.name,
        artist: p.song.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
        art_url,
        rank: latest.ranking.filter(|r| *r > 0),
        previous: latest.previous,
        time: latest.time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実ページの `__NEXT_DATA__` を必要なところだけ残したもの。
    fn page(points: &str, album: &str) -> String {
        format!(
            r#"<html><script id="__NEXT_DATA__" type="application/json">{{"props":{{"pageProps":
            {{"data":{{"chart":"MELON_TOP100_CHART","data":[{points}]}},
              "song":{{"id":"603442244","name":"긴 소원 (Birthday Wish)",
                       "artists":[{{"name":"NMIXX"}}],"album":{album}}}}}}},"page":"/x"}}
            </script></html>"#
        )
    }

    #[test]
    fn takes_the_latest_hour_and_the_song() {
        let html = page(
            r#"{"ranking":null,"previous":null,"time":"2026-10-19T18:00:00"},
               {"ranking":43,"previous":41,"time":"2026-10-21T14:00:00","count1":null},
               {"ranking":41,"previous":38,"time":"2026-10-21T13:00:00"}"#,
            r#"{"id":"14801628","image":"https://cdnimg.melon.co.kr/a.jpg"}"#,
        );
        let c = parse(&html).unwrap();
        assert_eq!(c.title, "긴 소원 (Birthday Wish)");
        assert_eq!(c.artist, "NMIXX");
        assert_eq!(c.art_url.as_deref(), Some("https://cdnimg.melon.co.kr/a.jpg"));
        assert_eq!((c.rank, c.previous), (Some(43), Some(41)), "並び順によらず最新の時台");
        assert_eq!(c.time, "2026-10-21T14:00:00");
        assert_eq!(c.movement(), Move::Down(2));
    }

    #[test]
    fn out_of_chart_and_missing_album_image() {
        let html = page(
            r#"{"ranking":null,"previous":null,"time":"2026-10-21T15:00:00"}"#,
            r#"{"id":"14801628","image":null}"#,
        );
        let c = parse(&html).unwrap();
        assert_eq!(c.movement(), Move::Out, "最新の時台が null なら圏外");
        assert_eq!(
            c.art_url.as_deref(),
            Some("https://cdn.guyso.me/image/album/melon/14801628.jpg"),
            "album.image が無ければ CDN へ落とす"
        );
        // __NEXT_DATA__ が無い/壊れているページは None
        assert!(parse("<html>404</html>").is_none());
        assert!(parse(&page("{\"ranking\":", "null")).is_none());
    }

    fn chart(rank: Option<u32>, previous: Option<u32>, time: &str) -> Chart {
        Chart {
            title: "t".into(),
            artist: "a".into(),
            art_url: None,
            rank,
            previous,
            time: time.into(),
        }
    }

    #[test]
    fn movement_from_the_previous_hour() {
        let t = "2026-10-21T14:00:00";
        assert_eq!(chart(Some(26), Some(30), t).movement(), Move::Up(4));
        assert_eq!(chart(Some(62), Some(62), t).movement(), Move::Same);
        assert_eq!(chart(Some(34), Some(0), t).movement(), Move::New, "previous=0 は新規");
        assert_eq!(chart(Some(34), None, t).movement(), Move::New);
        assert_eq!(chart(None, None, t).movement(), Move::Out);
    }

    /// KST の y-m-d h:mm を unix 秒に。
    fn kst(y: i64, m: i64, d: i64, h: i64, min: i64) -> i64 {
        (days_from_civil(y, m, d) * 24 + h) * 3600 + min * 60 - KST_OFFSET
    }

    #[test]
    fn time_label_drops_the_date_only_for_today() {
        let c = chart(Some(1), Some(1), "2026-10-21T14:00:00");
        assert_eq!(c.time_label(kst(2026, 10, 21, 14, 47)), "14:00");
        // 日付をまたいだら日付も付ける(KST の 0 時 = JST の 0 時)
        assert_eq!(c.time_label(kst(2026, 10, 22, 0, 30)), "10/21 14:00");
        assert_eq!(chart(None, None, "2026-01-05T09:00:00").time_label(0), "1/5 09:00");
    }

    #[test]
    fn hour_index_round_trips_with_the_clock() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(hour_index("2026-10-21T14:00:00"), Some(kst(2026, 10, 21, 14, 0) / 3600 + 9));
        assert_eq!(hour_index("bad"), None);
        assert_eq!(hour_index("2026-13-01T00:00:00"), None);
    }

    #[test]
    fn only_songs_behind_the_hour_are_fetched_again() {
        let hour = hour_index("2026-10-21T15:00:00").unwrap();
        assert!(is_current(&chart(Some(1), Some(1), "2026-10-21T15:00:00"), hour));
        assert!(!is_current(&chart(Some(1), Some(1), "2026-10-21T14:00:00"), hour));
        assert!(!is_current(&chart(None, None, "bad"), hour), "時刻が読めなければ取り直す");
    }

    #[test]
    fn rotation_switches_every_ten_seconds() {
        let at = |s| rotation(Duration::from_secs(s), 3);
        let got = [at(0), at(9), at(10), at(25), at(30)];
        assert_eq!(got, [Some(0), Some(0), Some(1), Some(2), Some(0)]);
        assert_eq!(rotation(Duration::from_secs(15), 1), Some(0), "1 曲なら出しっぱなし");
        assert_eq!(rotation(Duration::from_secs(15), 0), None);
    }

    #[test]
    fn waits_for_the_next_hour_and_retries_until_it_shows_up() {
        let at = |h, min| kst(2026, 10, 21, h, min);
        let cur = hour_index("2026-10-21T14:00:00");
        let prev = hour_index("2026-10-21T13:00:00");
        // 今の時台が出ていれば次の正時 + 1 分まで寝る
        assert_eq!(next_wait(cur, false, at(14, 47)), Duration::from_secs(14 * 60));
        // 正時を過ぎてもまだ前の時台なら 1 分おきに取り直す
        assert_eq!(next_wait(prev, false, at(14, 1)), Duration::from_secs(60));
        assert_eq!(next_wait(None, false, at(14, 5)), Duration::from_secs(60), "未取得も同じ");
        // 20 分過ぎても出ないなら 5 分おきに開ける
        assert_eq!(next_wait(prev, false, at(14, 30)), Duration::from_secs(300));
        // ただし次の正時は寝過ごさない
        assert_eq!(next_wait(prev, false, at(14, 58)), Duration::from_secs(3 * 60));
    }

    #[test]
    fn a_failed_fetch_retries_soon_whatever_the_time() {
        let at = |h, min| kst(2026, 10, 21, h, min);
        let prev = hour_index("2026-10-21T16:00:00");
        // サスペンド明けでネットがまだ無い(17:36)。20 分を過ぎていても 1 分後に取り直す
        assert_eq!(next_wait(prev, true, at(17, 36)), Duration::from_secs(60));
        assert_eq!(next_wait(None, true, at(17, 50)), Duration::from_secs(60));
        // 今の時台まで取れている曲があっても、失敗した曲があれば待たない
        let cur = hour_index("2026-10-21T17:00:00");
        assert_eq!(next_wait(cur, true, at(17, 40)), Duration::from_secs(60));
    }

    #[test]
    fn sleep_until_returns_at_once_for_a_past_deadline() {
        let t = std::time::Instant::now();
        sleep_until(now_unix() - 3600);
        assert!(t.elapsed() < Duration::from_millis(100));
    }
}
