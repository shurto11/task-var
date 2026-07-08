//! fbterm 端末の行数調整。touch-key/src/term.rs を踏襲。
//!
//! タスクバーが画面下部を占有するため、起動時に端末の pty へ stty で行数を
//! 縮めて表示がバーと重ならないようにし、終了時に元の行数へ復元する。
//!
//! 対象の優先順位:
//! 1. `TASKVAR_TTY`(明示指定)
//! 2. uim-fep の端末 — fbterm → uim-fep(mozc)→ シェルの構成では、uim-fep が
//!    自端末の最下行に IM ステータス行を描き、子 pty を 1 行少なくリサイズして
//!    伝播する。uim-fep 側を縮めれば mozc 表示ごとバーの上に移動する。
//! 3. tmux クライアントのうち termname が fbterm/linux のもの(SSH は触らない)。
//!
//! 見つからなければ何もしない。

use std::process::Command;

/// 縮小前の状態。exit / シグナル時に `restore` で戻す。
#[derive(Clone)]
pub struct TermGuard {
    tty: String,
    orig_rows: u32,
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn set_rows(tty: &str, rows: u32) -> Option<()> {
    run("stty", &["-F", tty, "rows", &rows.to_string()]).map(|_| ())
}

/// stty size で (cols, rows) を得る(出力は "rows cols")。
fn tty_size(tty: &str) -> Option<(u32, u32)> {
    let size = run("stty", &["-F", tty, "size"])?;
    let mut it = size.split_whitespace();
    let rows: u32 = it.next()?.parse().ok()?;
    let cols: u32 = it.next()?.parse().ok()?;
    Some((cols, rows))
}

/// uim-fep が動いていればその端末を返す。
fn uim_fep_tty() -> Option<String> {
    let out = run("ps", &["-C", "uim-fep", "-o", "tty="])?;
    let t = out.lines().next()?.trim();
    if t.is_empty() || t == "?" {
        return None;
    }
    Some(format!("/dev/{t}"))
}

/// 対象端末を探して (tty, cols, rows) を返す(優先順位はモジュールコメント参照)。
fn find_client() -> Option<(String, u32, u32)> {
    if let Ok(tty) = std::env::var("TASKVAR_TTY") {
        if !tty.is_empty() {
            let (cols, rows) = tty_size(&tty)?;
            return Some((tty, cols, rows));
        }
    }
    if let Some(tty) = uim_fep_tty() {
        if let Some((cols, rows)) = tty_size(&tty) {
            return Some((tty, cols, rows));
        }
    }
    let out = run(
        "tmux",
        &["list-clients", "-F", "#{client_name} #{client_termname} #{client_width} #{client_height}"],
    )?;
    let clients: Vec<Vec<&str>> =
        out.lines().map(|l| l.split_whitespace().collect()).filter(|f: &Vec<&str>| f.len() == 4).collect();
    let pick = clients
        .iter()
        .find(|f| f[1].starts_with("fbterm") || f[1] == "linux")
        .or(if clients.len() == 1 { clients.first() } else { None })?;
    Some((pick[0].to_string(), pick[2].parse().ok()?, pick[3].parse().ok()?))
}

fn disabled() -> bool {
    std::env::var("TASKVAR_SHRINK").is_ok_and(|v| v == "0")
}

/// 対象端末のセル高(px)。fbterm はセルを敷き詰めるのでセル幅 = 画面幅 / 桁数、
/// 高さは等幅フォントの慣例(8x16 等)どおり幅の 2 倍とみなす。
/// バー上端をセル境界へスナップして端末との隙間を無くすのに使う。
pub fn cell_height(screen_w: u32) -> Option<u32> {
    if disabled() {
        return None;
    }
    let (_, cols, _) = find_client()?;
    let cell_h = screen_w / cols.max(1) * 2;
    (cell_h > 0).then_some(cell_h)
}

/// 端末行数をバー上端まで縮める。縮めた場合のみ TermGuard を返す。
/// `TASKVAR_SHRINK=0` で無効化。
pub fn shrink(screen_w: u32, region_y: u32) -> Option<TermGuard> {
    if disabled() {
        return None;
    }
    let (tty, cols, rows) = find_client()?;
    let cell_h = screen_w / cols.max(1) * 2;
    if cell_h == 0 {
        return None;
    }
    let target = (region_y / cell_h).max(5);
    if target >= rows {
        return None; // 既に収まっている(前回の縮小が残っている場合も含む)
    }
    set_rows(&tty, target)?;
    eprintln!("task-var: 端末行数を縮小 {rows} → {target} ({tty})");
    Some(TermGuard { tty, orig_rows: rows })
}

impl TermGuard {
    /// 行数を起動時の値へ戻す。
    pub fn restore(&self) {
        if set_rows(&self.tty, self.orig_rows).is_some() {
            eprintln!("task-var: 端末行数を復元 {} ({})", self.orig_rows, self.tty);
        }
    }
}
