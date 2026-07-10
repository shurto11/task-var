//! tmux コマンドの薄いラッパ。task-var は tmux 外のデーモンなので、
//! 遷移先の表示は fbterm 上の tmux クライアント(termname が fbterm/linux)に対して行う。

use anyhow::{bail, Context, Result};
use std::process::Command;

fn run(args: &[&str]) -> Option<String> {
    let out = Command::new("tmux").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_checked(args: &[&str]) -> Result<()> {
    let out = Command::new("tmux").args(args).output().context("tmux 実行失敗")?;
    if !out.status.success() {
        bail!("tmux {} 失敗: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// バーの描画判断に使うセッション状態。1 秒間隔でポーリングする。
#[derive(PartialEq, Clone, Default)]
pub struct State {
    /// fbterm クライアントの名前(= tty)。switch-client の -c に渡す。
    pub client: Option<String>,
    /// fbterm クライアントが表示中のセッション名。
    pub current: Option<String>,
    /// 存在する全セッション名。
    pub existing: Vec<String>,
}

impl State {
    /// tmux から現在の状態を取得する。tmux サーバー不在時は全て空。
    pub fn poll() -> Self {
        let mut st = State::default();
        if let Some(out) = run(&["list-sessions", "-F", "#S"]) {
            st.existing = out.lines().map(str::to_string).collect();
        }
        if let Some((name, session)) = fbterm_client() {
            st.client = Some(name);
            st.current = Some(session);
        }
        st
    }
}

/// fbterm 上の tmux クライアントを探して (client_name, client_session) を返す。
/// SSH クライアントは対象にしない(term.rs の探索と同じ方針)。
fn fbterm_client() -> Option<(String, String)> {
    let out = run(&["list-clients", "-F", "#{client_name}\t#{client_termname}\t#{client_session}"])?;
    let clients: Vec<Vec<&str>> =
        out.lines().map(|l| l.split('\t').collect()).filter(|f: &Vec<&str>| f.len() == 3).collect();
    let pick = clients
        .iter()
        .find(|f| f[1].starts_with("fbterm") || f[1] == "linux")
        .or(if clients.len() == 1 { clients.first() } else { None })?;
    Some((pick[0].to_string(), pick[2].to_string()))
}

/// fbterm クライアントの表示をセッションへ切り替える。
pub fn switch(client: &str, session: &str) -> Result<()> {
    run_checked(&["switch-client", "-c", client, "-t", session])
}

/// デタッチ状態で新規セッションを作り、コマンドを実行する(cmd はシェル経由)。
pub fn new_session(name: &str, cmd: &str) -> Result<()> {
    run_checked(&["new-session", "-d", "-s", name, cmd])
}

/// セッション内に新規ウィンドウを作ってコマンドを実行する(作成後そのウィンドウが選択される)。
///
/// -t は末尾コロン付き(`name:`)でセッション指定を明示する。tmux の自動命名
/// セッション("4" や "66" など数値名)を裸で渡すと target-window の
/// 「現セッションのウィンドウ index」と解釈され、その index が使用中だと
/// "create window failed: index N in use" で失敗する(空いていれば偶然成功する)。
pub fn new_window(session: &str, cmd: &str) -> Result<()> {
    run_checked(&["new-window", "-t", &format!("{session}:"), cmd])
}
