//! tmux の各ペインで動く Claude Code の状態管理(旧 touch-claude)。
//!
//! Claude Code の hooks が `task-var hook-*` を呼び、`$TMUX_PANE` を添えて
//! Unix ソケットへ 1 行送る。デーモン側はそれをペインごとの状態として保ち、
//! バー中央の枠にキャラクターとセッション名で並べる。
//!
//! touch-claude から変えたところ:
//! - デーモンは task-var 本体。hook からの自動起動はしない(tmux-autostart が
//!   立ち上げる前提。届かなければ黙って諦める = hook を絶対に遅らせない)
//! - 表示はバー内の固定枠なので、経過時間による横幅の拡大は引き継がない
//! - キャラの右に「そのペインで何をしているか」を出す。Claude Code は端末
//!   タイトルへ作業の要約を書き込んでくるので、それを第一候補にし、
//!   無ければカレントディレクトリ名へ落とす
//! - 枠に収まらない(既定 4 行を超える)ときは、要対応(質問待ち → 終了)を
//!   優先して選ぶ。並べる順は開始順のままなので、行が飛び回らない

use crate::tmux;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// hook コマンドとデーモンが話すソケット。
pub fn socket_path() -> PathBuf {
    let dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from("/tmp"),
    };
    dir.join("task-var.sock")
}

/// キャラクターの状態。色はこれで決まる。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum St {
    /// 処理中(オレンジ・走るアニメーション)
    Run,
    /// 質問・許可待ち(青)
    Ask,
    /// 処理終了(黄)
    Done,
    /// 終了をタッチで確認した後(灰)。次の処理開始で Run へ戻る
    Seen,
}

impl St {
    /// 枠に入りきらないときに残す優先度(小さいほど先に残る)。
    /// ユーザーの操作を待っているものから順に見せる。
    fn prio(self) -> u8 {
        match self {
            St::Ask => 0,
            St::Done => 1,
            St::Run => 2,
            St::Seen => 3,
        }
    }
}

/// ペイン 1 つぶんの状態。`Vec` の並び = 開始順 = 表示の上からの並び。
struct Entry {
    pane: String,
    /// キャラの右に出す見出し(`label` 参照)。不明なら空。
    label: String,
    st: St,
}

/// Claude Code が端末タイトルの頭に付けるマーカー。この後ろが作業の要約になる。
/// 状態によって字が変わるので、見かけるものをまとめて剥がす。
const TITLE_MARKS: [char; 6] = ['✳', '✻', '✽', '✶', '✢', '*'];

/// ペインの見出しを決める。
///
/// tmux のセッション名は、複数の claude が同じセッションで動くと全部同じに
/// なってしまい見分けがつかない(実機では軒並み "0")。代わりに
/// 1. Claude Code が端末タイトルへ書く作業の要約(`✳ タスクの説明`)
/// 2. それが無ければカレントディレクトリ名
///
/// の順で選ぶ。どちらも取れなければ空(キャラだけ出る)。
fn label(title: &str, cwd: &str) -> String {
    let title = title.trim();
    if let Some(rest) = title.strip_prefix(TITLE_MARKS) {
        let rest = rest.trim();
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    cwd.trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string()
}

#[derive(Default)]
pub struct Model {
    entries: Vec<Entry>,
}

/// バーへ渡す 1 行ぶんの表示内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub pane: String,
    /// キャラの右に出す見出し(作業の要約、またはディレクトリ名)。
    pub label: String,
    pub st: St,
}

impl Model {
    fn find(&mut self, pane: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.pane == pane)
    }

    /// hook からの 1 コマンドを適用する。
    pub fn apply(&mut self, cmd: &str, pane: &str) {
        // 取りこぼしの追跡ができるよう、状態が動くものはログに残す
        // (answer は PostToolUse ごとに飛んでくるので除く)
        if cmd != "answer" {
            eprintln!("task-var: clawd cmd={cmd} pane={pane}");
        }
        match cmd {
            "start" => {
                if let Some(e) = self.find(pane) {
                    e.st = St::Run;
                } else {
                    let label = tmux::pane_info(pane)
                        .map(|p| label(&p.title, &p.cwd))
                        .unwrap_or_default();
                    self.entries.push(Entry { pane: pane.to_string(), label, st: St::Run });
                }
            }
            // Notification は処理終了後のアイドル通知(60 秒放置)でも飛んでくるため、
            // 処理中のものだけを質問待ちにする。終了(黄)は黄のまま維持する
            "question" => {
                if let Some(e) = self.find(pane) {
                    if e.st == St::Run {
                        e.st = St::Ask;
                    }
                }
            }
            "answer" => {
                if let Some(e) = self.find(pane) {
                    if e.st == St::Ask {
                        e.st = St::Run;
                    }
                }
            }
            "stop" => {
                if let Some(e) = self.find(pane) {
                    e.st = St::Done;
                }
            }
            // タッチで終了(黄)を確認した
            "seen" => {
                if let Some(e) = self.find(pane) {
                    if e.st == St::Done {
                        e.st = St::Seen;
                    }
                }
            }
            "end" => self.entries.retain(|e| e.pane != pane),
            _ => {}
        }
    }

    /// 表示する行を最大 `max` 件返す。
    ///
    /// 選ぶのは優先度順(質問待ち → 終了 → 処理中 → 確認済み、同じなら開始順)、
    /// 並べるのは開始順。こうすると「見逃したくないものが必ず出る」一方で、
    /// 状態が変わるたびに行が入れ替わってタップ先がずれることもない。
    pub fn rows(&self, max: usize) -> Vec<Row> {
        let mut idx: Vec<usize> = (0..self.entries.len()).collect();
        idx.sort_by_key(|&i| (self.entries[i].st.prio(), i));
        idx.truncate(max);
        idx.sort_unstable();
        idx.into_iter()
            .map(|i| {
                let e = &self.entries[i];
                Row { pane: e.pane.clone(), label: e.label.clone(), st: e.st }
            })
            .collect()
    }
}

/// tmux の実状態に合わせて、消えたペインを掃除し見出しを更新する。
/// 作業の要約は処理中に書き換わるので、ここで追いかける。
/// task-var のポーリング(1 秒間隔)から呼ぶ。
pub fn refresh(model: &Arc<Mutex<Model>>) {
    // エントリが無いときに tmux を叩いても意味がない
    if model.lock().unwrap().entries.is_empty() {
        return;
    }
    match tmux::panes() {
        Some(alive) => {
            let mut m = model.lock().unwrap();
            m.entries.retain(|e| {
                let keep = alive.iter().any(|p| p.id == e.pane);
                if !keep {
                    eprintln!("task-var: clawd ペイン消滅につき削除 {}", e.pane);
                }
                keep
            });
            for e in &mut m.entries {
                if let Some(p) = alive.iter().find(|p| p.id == e.pane) {
                    let now = label(&p.title, &p.cwd);
                    if e.label != now {
                        e.label = now;
                    }
                }
            }
        }
        // tmux サーバーが居ないなら、そこで動く claude も居ない
        None if !tmux::server_running() => {
            let mut m = model.lock().unwrap();
            if !m.entries.is_empty() {
                eprintln!("task-var: clawd tmux サーバー不在のため全 {} 件を削除", m.entries.len());
                m.entries.clear();
            }
        }
        // 一時的な失敗。次のポーリングに持ち越す
        None => {}
    }
}

#[derive(Deserialize)]
struct Msg {
    cmd: String,
    #[serde(default)]
    pane: Option<String>,
}

/// hook からの通知を受けるソケットを開き、受信スレッドを起こす。
/// ソケットを開けない場合はバーの他の機能を巻き添えにせず、警告だけ出す。
pub fn spawn(model: Arc<Mutex<Model>>) {
    let sock = socket_path();
    let _ = std::fs::remove_file(&sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("task-var: clawd ソケットを開けません({}): {e}", sock.display());
            return;
        }
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Msg>(line.trim()) else { continue };
            match msg.cmd.as_str() {
                "status" => {
                    let m = model.lock().unwrap();
                    let mut s = String::new();
                    for e in &m.entries {
                        let st = match e.st {
                            St::Run => "run",
                            St::Ask => "ask",
                            St::Done => "done",
                            St::Seen => "seen",
                        };
                        let label = if e.label.is_empty() { "?" } else { &e.label };
                        s.push_str(&format!("{} {} {}\n", e.pane, st, label));
                    }
                    if s.is_empty() {
                        s = "(表示中の claude なし)\n".to_string();
                    }
                    let _ = (&stream).write_all(s.as_bytes());
                }
                cmd => {
                    if let Some(pane) = msg.pane.as_deref() {
                        model.lock().unwrap().apply(cmd, pane);
                    }
                }
            }
        }
    });
}

/// hooks から呼ばれる側。ソケットへ 1 行送るだけで終わる。
///
/// hook は即 return が必須なので、デーモンが居なければ何もせず成功で返す
/// (touch-claude はここでデーモンを起こしていたが、task-var は端末行数を
/// 縮めるデーモンなので hook から起こすのは行儀が悪い)。
pub fn notify(cmd: &str) -> Result<()> {
    // tmux の外で動く claude は対象外
    let Ok(pane) = std::env::var("TMUX_PANE") else { return Ok(()) };
    let line = serde_json::json!({"cmd": cmd, "pane": pane}).to_string() + "\n";
    if let Ok(mut s) = UnixStream::connect(socket_path()) {
        let _ = s.write_all(line.as_bytes());
    }
    Ok(())
}

/// `task-var status`: デーモンが把握している claude の一覧を表示する。
pub fn status() -> Result<()> {
    let mut s = match UnixStream::connect(socket_path()) {
        Ok(s) => s,
        Err(_) => {
            println!("task-var: 停止中");
            return Ok(());
        }
    };
    s.write_all((serde_json::json!({"cmd": "status"}).to_string() + "\n").as_bytes())
        .context("status の送信に失敗")?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    print!("task-var: 起動中\n{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(states: &[St]) -> Model {
        Model {
            entries: states
                .iter()
                .enumerate()
                .map(|(i, &st)| Entry { pane: format!("%{i}"), label: format!("s{i}"), st })
                .collect(),
        }
    }

    #[test]
    fn label_prefers_what_claude_is_doing() {
        // Claude Code が端末タイトルへ書く作業の要約が第一候補
        assert_eq!(label("✳ touch-claude廃止とセッション表示枠", "/home/me/ssd/tools/touch/task-var"), "touch-claude廃止とセッション表示枠");
        // マーカーが別の字でも剥がす
        assert_eq!(label("✻ フレームバッファの周辺知識", "/home/me"), "フレームバッファの周辺知識");
        // claude のタイトルでなければ(ホスト名など)ディレクトリ名へ落とす
        assert_eq!(label("ubuntubook", "/home/me/ssd/tools/touch/task-var"), "task-var");
        assert_eq!(label("", "/home/me/ssd/ssbrowse/"), "ssbrowse");
        // マーカーだけで中身が無いときもディレクトリ名
        assert_eq!(label("✳", "/home/me/work"), "work");
        // どちらも取れなければ空(キャラだけ出る)
        assert_eq!(label("", ""), "");
    }

    #[test]
    fn state_transitions_follow_the_hooks() {
        let mut m = Model::default();
        // 見出しは tmux 頼みなので、テスト環境では空のことがある
        m.apply("start", "%1");
        assert_eq!(m.rows(4).len(), 1);
        assert_eq!(m.rows(4)[0].st, St::Run);

        m.apply("question", "%1");
        assert_eq!(m.rows(4)[0].st, St::Ask, "処理中への質問通知は青");
        m.apply("answer", "%1");
        assert_eq!(m.rows(4)[0].st, St::Run, "回答後は処理中へ戻る");

        m.apply("stop", "%1");
        assert_eq!(m.rows(4)[0].st, St::Done);
        m.apply("question", "%1");
        assert_eq!(m.rows(4)[0].st, St::Done, "終了後のアイドル通知では青にしない");
        m.apply("seen", "%1");
        assert_eq!(m.rows(4)[0].st, St::Seen, "タッチで確認済み");
        m.apply("start", "%1");
        assert_eq!(m.rows(4)[0].st, St::Run, "次のプロンプトで処理中へ戻る");

        m.apply("end", "%1");
        assert!(m.rows(4).is_empty(), "SessionEnd で消える");
    }

    #[test]
    fn overflow_keeps_the_ones_that_need_attention() {
        // 開始順: Run, Seen, Done, Run, Ask, Run
        let m = model(&[St::Run, St::Seen, St::Done, St::Run, St::Ask, St::Run]);
        let rows = m.rows(4);

        // 選ばれるのは Ask(%4) → Done(%2) → Run(%0, %3)。Seen(%1) と Run(%5) は落ちる
        let panes: Vec<&str> = rows.iter().map(|r| r.pane.as_str()).collect();
        assert_eq!(panes, ["%0", "%2", "%3", "%4"], "並びは開始順のまま");

        // 4 件以下なら全部そのまま出る
        let m = model(&[St::Run, St::Done]);
        assert_eq!(m.rows(4).len(), 2);
    }
}
