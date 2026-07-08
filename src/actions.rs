//! アイコンの定義と、タッチされたときの動作。
//!
//! - tmux アイコン(session=None)は例外で、常に現セッション内の新規ウィンドウで
//!   tmux-session スイッチャーを起動する。
//! - それ以外は「セッションがあれば switch のみ / なければ作成してコマンド実行 → switch」。

use crate::tmux;
use anyhow::{bail, Context, Result};
use std::process::{Command, Stdio};

pub struct IconDef {
    pub name: &'static str,
    /// 対応する tmux セッション名。None は tmux スイッチャー(特殊動作)。
    pub session: Option<&'static str>,
    pub svg: &'static [u8],
}

pub const ICONS: [IconDef; 5] = [
    IconDef { name: "tmux", session: None, svg: include_bytes!("../assets/tmux.svg") },
    IconDef { name: "spotify", session: Some("spotify"), svg: include_bytes!("../assets/spotify.svg") },
    IconDef { name: "shorts", session: Some("shorts"), svg: include_bytes!("../assets/shorts.svg") },
    IconDef { name: "bluetooth", session: Some("bluetooth"), svg: include_bytes!("../assets/bluetooth.svg") },
    IconDef { name: "ssbrowse", session: Some("ssbrowse"), svg: include_bytes!("../assets/ssbrowse.svg") },
];

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())
}

/// セッション新規作成時に実行するコマンド(tmux がシェル経由で実行する)。
fn session_command(session: &str) -> String {
    let home = home();
    match session {
        "spotify" => format!("{home}/ssd/tui/spotatui/target/release/spotatui"),
        "shorts" => format!("{home}/ssd/tools/dopagaki/target/release/dopagaki standalone"),
        "bluetooth" => "bluetoothctl".to_string(),
        "ssbrowse" => format!("cd {home}/ssd/ssbrowse && npm run browser:auto"),
        _ => unreachable!("未知のセッション {session}"),
    }
}

/// spotatui-pip デーモンが動いていなければ起動する(fb 右下の再生情報ウィジェット)。
/// 既定のまま起動すると右下でタスクバーと上書き合戦になるため、
/// --margin をバー高さぶん取ってバーの上に配置する。
fn ensure_spotatui_pip(bar_h: u32) {
    let running = Command::new("pgrep")
        .args(["-x", "spotatui-pip"])
        .output()
        .is_ok_and(|o| o.status.success());
    if running {
        return;
    }
    let margin = (bar_h + 6).to_string();
    match Command::new("spotatui-pip")
        .args(["--margin", &margin])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => eprintln!("task-var: spotatui-pip を起動 (--margin {margin})"),
        Err(e) => eprintln!("task-var: spotatui-pip 起動失敗: {e}"),
    }
}

/// アイコンがタップされたときの動作。bar_h は spotatui-pip の配置マージンに使う。
pub fn activate(def: &IconDef, state: &tmux::State, bar_h: u32) -> Result<()> {
    let client = state.client.as_deref().context("fbterm の tmux クライアントが見つかりません")?;

    let Some(session) = def.session else {
        // tmux アイコン: 常に現セッションの新規ウィンドウでスイッチャーを実行
        let current = state.current.as_deref().context("表示中セッションが不明です")?;
        let bin = format!("{}/ssd/tools/tmux-session/target/release/tmux-session", home());
        if !std::path::Path::new(&bin).exists() {
            bail!("tmux-session バイナリがありません: {bin}");
        }
        eprintln!("task-var: tmux-session スイッチャーを起動 (session={current})");
        return tmux::new_window(current, &bin);
    };

    if session == "spotify" {
        ensure_spotatui_pip(bar_h);
    }
    if !state.existing.iter().any(|s| s == session) {
        eprintln!("task-var: セッション {session} を新規作成");
        tmux::new_session(session, &session_command(session))?;
    }
    eprintln!("task-var: セッション {session} へ切替");
    tmux::switch(client, session)
}
