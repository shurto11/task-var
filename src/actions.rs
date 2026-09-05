//! アイコンの定義と、タッチされたときの動作。
//!
//! - tmux アイコン(session=None)は例外で、常に現セッション内の新規ウィンドウで
//!   tmux-session スイッチャーを起動する。
//! - それ以外は「セッションがあれば switch のみ / なければ作成してコマンド実行 → switch」。
//! - Spotify の再生情報はバー右側のパネル(bar.rs)が担当するので、ここから
//!   spotatui-pip デーモンを起動することはしない。

use crate::tmux;
use anyhow::{bail, Context, Result};

pub struct IconDef {
    pub name: &'static str,
    /// 対応する tmux セッション名。None は tmux スイッチャー(特殊動作)。
    pub session: Option<&'static str>,
    pub svg: &'static [u8],
}

pub const ICONS: [IconDef; 7] = [
    IconDef { name: "tmux", session: None, svg: include_bytes!("../assets/tmux.svg") },
    IconDef { name: "spotify", session: Some("spotify"), svg: include_bytes!("../assets/spotify.svg") },
    IconDef { name: "shorts", session: Some("shorts"), svg: include_bytes!("../assets/shorts.svg") },
    IconDef { name: "bluetooth", session: Some("bluetooth"), svg: include_bytes!("../assets/bluetooth.svg") },
    IconDef { name: "ssbrowse", session: Some("ssbrowse"), svg: include_bytes!("../assets/ssbrowse.svg") },
    IconDef { name: "eduroam", session: Some("eduroam"), svg: include_bytes!("../assets/eduroam.svg") },
    IconDef { name: "calendar", session: Some("calendar"), svg: include_bytes!("../assets/calendar.svg") },
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
        // eduroam は ~/.bashrc の関数(sudo wpa_supplicant ...)なので対話bash経由で呼ぶ
        "eduroam" => "bash -ic eduroam".to_string(),
        // calendar-tui は credentials.json をカレントディレクトリから探すため、
        // 自身のディレクトリへ cd してから起動する(tmux new-session の既定cwdは
        // task-var 自身のディレクトリを継承してしまうため)。
        "calendar" => format!(
            "cd {home}/ssd/tui/calendar-tui && {home}/ssd/tui/calendar-tui/target/release/calendar-tui"
        ),
        _ => unreachable!("未知のセッション {session}"),
    }
}

/// アイコンがタップされたときの動作。
pub fn activate(def: &IconDef, state: &tmux::State) -> Result<()> {
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

    if !state.existing.iter().any(|s| s == session) {
        eprintln!("task-var: セッション {session} を新規作成");
        tmux::new_session(session, &session_command(session))?;
    }
    eprintln!("task-var: セッション {session} へ切替");
    tmux::switch(client, session)
}
