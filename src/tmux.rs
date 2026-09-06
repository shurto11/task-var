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
    /// 存在するセッションのうち最も古い(作成が最初の)もの。
    /// アイコン起動セッションがプログラム終了で破棄されたときの復帰先に使う。
    pub first_session: Option<String>,
}

impl State {
    /// tmux から現在の状態を取得する。tmux サーバー不在時は全て空。
    pub fn poll() -> Self {
        let mut st = State::default();
        if let Some(out) = run(&["list-sessions", "-F", "#{session_created} #{session_name}"]) {
            let mut sessions: Vec<(i64, String)> = out
                .lines()
                .filter_map(|l| {
                    let (created, name) = l.split_once(' ')?;
                    Some((created.parse().ok()?, name.to_string()))
                })
                .collect();
            sessions.sort_by_key(|(created, _)| *created);
            st.first_session = sessions.first().map(|(_, name)| name.clone());
            st.existing = sessions.into_iter().map(|(_, name)| name).collect();
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
///
/// 作成したセッション単体に detach-on-destroy off を設定する。既定(on)だと
/// このセッションを表示中にコマンドが終了した際、fbterm クライアントごと
/// デタッチ(tmux 終了)してしまうため。実際にどのセッションへ表示を戻すかは
/// main.rs のポーリングループが明示的に switch-client で決める。
pub fn new_session(name: &str, cmd: &str) -> Result<()> {
    run_checked(&["new-session", "-d", "-s", name, cmd])?;
    // 起動直後にコマンドが即終了してセッションが既に無い場合はここで失敗しうる
    // (即クラッシュするプログラムなど)。detach-on-destroy はもう手遅れなだけなので
    // 後続の switch に処理を進め、そちらの失敗で実際の原因を伝える。
    if let Err(e) = run_checked(&["set-option", "-t", name, "detach-on-destroy", "off"]) {
        eprintln!("task-var: {name} の detach-on-destroy 設定に失敗(起動直後に終了した可能性): {e:#}");
    }
    Ok(())
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

/// ペインの見出しに使う材料。`title` は端末タイトル(Claude Code が
/// 「いま何をしているか」を書き込んでくる)、`cwd` はカレントディレクトリ。
pub struct PaneInfo {
    pub id: String,
    pub title: String,
    pub cwd: String,
}

/// 出力 1 行を PaneInfo にする。タイトルに空白が入るのでタブ区切りで受ける。
fn parse_pane(line: &str) -> Option<PaneInfo> {
    let mut f = line.splitn(3, '\t');
    let id = f.next()?.trim();
    if id.is_empty() {
        return None;
    }
    Some(PaneInfo {
        id: id.to_string(),
        title: f.next().unwrap_or_default().trim().to_string(),
        cwd: f.next().unwrap_or_default().trim().to_string(),
    })
}

const PANE_FORMAT: &str = "#{pane_id}\t#{pane_title}\t#{pane_current_path}";

/// 1 つのペインの情報。ペインが無ければ None。
pub fn pane_info(pane: &str) -> Option<PaneInfo> {
    parse_pane(run(&["display", "-p", "-t", pane, PANE_FORMAT])?.lines().next()?)
}

/// 全ペインの情報。tmux を呼べなければ None。
pub fn panes() -> Option<Vec<PaneInfo>> {
    Some(run(&["list-panes", "-a", "-F", PANE_FORMAT])?.lines().filter_map(parse_pane).collect())
}

/// tmux サーバーが動いているか。ペイン一覧の取得に失敗したとき、
/// 「サーバーが居ない」のか「一時的な失敗」なのかを分けるために使う。
pub fn server_running() -> bool {
    Command::new("tmux")
        .args(["has-session"])
        .output()
        .map(|o| {
            let err = String::from_utf8_lossy(&o.stderr);
            // セッションが 1 つも無いだけならサーバーは生きている
            !err.contains("no server running") && !err.contains("error connecting")
        })
        .unwrap_or(false)
}

/// 表示中の tmux クライアントを、指定ペインのセッション・ウィンドウ・ペインへ
/// 移す。デーモンは tmux クライアントの外に居るので、switch-client には
/// 対象クライアントを明示する(`-t` にはペイン ID をそのまま渡せる)。
pub fn goto_pane(client: &str, pane: &str) -> Result<()> {
    run_checked(&["switch-client", "-c", client, "-t", pane])?;
    run_checked(&["select-window", "-t", pane])?;
    run_checked(&["select-pane", "-t", pane])
}
