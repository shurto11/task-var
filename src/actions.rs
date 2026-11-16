//! アイコンの定義(icons.toml)と、タッチされたときの動作。
//!
//! 定義はリポジトリ直下の `icons.toml`(`TASKVAR_ICONS` で差し替え可)を実行時に
//! 読む。デーモンは 1 秒ごとにファイル(と画像)の更新時刻を見て、変わっていれば
//! 読み直してバーを組み直す(main.rs)。書き間違いなら読み直さず前の定義で続ける。
//!
//! - `window = true` のアイコン(tmux)は、常に表示中のセッションに新しいウィンドウを
//!   開いてコマンドを実行する。
//! - それ以外は「セッションがあれば switch のみ / なければ作成してコマンド実行 → switch」。
//! - Spotify は再生情報パネル(bar.rs)が受け持つので、アイコン列には並べず
//!   `[spotify]` として別に持つ。パネルのタップとアート未取得時の絵がこれを引く。

use crate::tmux;
use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// ビルド時点の icons.toml。実行時のファイルが読めないときの控え。
const BUILTIN: &str = include_str!("../icons.toml");

/// icons.toml の既定の場所(このリポジトリの直下)。
const DEFAULT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/icons.toml");

/// アイコン 1 個ぶん。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    /// tmux のセッション名(`window` なら名前として使うだけ)。
    pub name: String,
    /// 画像のパス(`~` と相対パスは解決済み)。
    pub image: PathBuf,
    /// セッション(`window` ならウィンドウ)を作ったときに実行するコマンド。
    pub command: String,
    /// セッションを作らず、表示中のセッションに新しいウィンドウを開く。
    pub window: bool,
}

impl Icon {
    /// 対応する tmux セッション名。`window` のアイコンは None。
    pub fn session(&self) -> Option<&str> {
        (!self.window).then_some(self.name.as_str())
    }
}

/// icons.toml 全体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icons {
    /// アイコン列に左から並べる順。空でもよい。
    pub list: Vec<Icon>,
    /// 再生情報パネルの遷移先とアート未取得時の絵。
    pub spotify: Icon,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    icon: Vec<Entry>,
    spotify: Entry,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    image: String,
    command: String,
    #[serde(default)]
    window: bool,
}

/// `~/` を $HOME に、相対パスを base からのパスにする。
fn resolve(path: &str, base: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => Path::new(&home()).join(rest),
        None => base.join(path),
    }
}

impl Icons {
    /// icons.toml の場所。`TASKVAR_ICONS` があればそれ。
    pub fn path() -> PathBuf {
        std::env::var_os("TASKVAR_ICONS").map(PathBuf::from).unwrap_or_else(|| DEFAULT_PATH.into())
    }

    /// TOML の中身を読む。画像の相対パスは base から解決する。
    pub fn parse(text: &str, base: &Path) -> Result<Self> {
        let file: File = toml::from_str(text)?;
        let to_icon = |e: Entry| -> Result<Icon> {
            let name = e.name.trim().to_string();
            ensure!(!name.is_empty(), "name が空のアイコンがある");
            // tmux はセッション名の : と . を _ に置き換えるので、名前で照合できなくなる
            ensure!(!name.contains([':', '.']), "name に「:」「.」は使えない: {name}");
            ensure!(!e.command.trim().is_empty(), "{name} の command が空");
            Ok(Icon { name, image: resolve(&e.image, base), command: e.command, window: e.window })
        };
        let list = file.icon.into_iter().map(to_icon).collect::<Result<Vec<_>>>()?;
        let spotify = to_icon(file.spotify).context("[spotify]")?;
        let mut seen = std::collections::HashSet::new();
        for icon in list.iter().chain([&spotify]) {
            if !seen.insert(icon.name.as_str()) {
                bail!("name が重なっている: {}", icon.name);
            }
        }
        Ok(Self { list, spotify })
    }

    /// ファイルから読む。
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("{} を開けない", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        Self::parse(&text, base).with_context(|| format!("{} の書き方が違う", path.display()))
    }

    /// ビルド時点の icons.toml。画像はリポジトリの assets/ を引く。
    pub fn builtin() -> Result<Self> {
        Self::parse(BUILTIN, Path::new(env!("CARGO_MANIFEST_DIR")))
    }

    /// 起動時の読み込み。ファイル → ビルド時点の控え → アイコン無し の順に諦める
    /// (書き間違いでバーごと出なくなるのを避ける)。
    pub fn load_at_start(path: &Path) -> Self {
        Self::load(path)
            .or_else(|e| {
                eprintln!("task-var: アイコンの定義を読めません(ビルド時点の定義で起動): {e:#}");
                Self::builtin()
            })
            .unwrap_or_else(|e| {
                eprintln!("task-var: ビルド時点のアイコン定義も読めません(アイコン無しで起動): {e:#}");
                Self {
                    list: Vec::new(),
                    spotify: Icon {
                        name: "spotify".into(),
                        image: PathBuf::new(),
                        command: "true".into(),
                        window: false,
                    },
                }
            })
    }

    /// アイコン列に居るか spotify かを問わず、セッションに対応するものの名前。
    /// tmux(window)アイコンの下線を決めるのに使う。
    pub fn sessions(&self) -> impl Iterator<Item = &str> {
        self.list.iter().chain([&self.spotify]).filter_map(Icon::session)
    }

    /// 定義ファイルと画像の更新時刻。変わっていれば読み直す合図。
    /// 画像だけ差し替えても(定義を書き換えなくても)反映されるよう画像も見る。
    pub fn stamp(&self, path: &Path) -> Vec<Option<SystemTime>> {
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        std::iter::once(path)
            .chain(self.list.iter().chain([&self.spotify]).map(|i| i.image.as_path()))
            .map(mtime)
            .collect()
    }
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())
}

/// アイコンがタップされたときの動作。
pub fn activate(icon: &Icon, state: &tmux::State) -> Result<()> {
    let client = state.client.as_deref().context("fbterm の tmux クライアントが見つかりません")?;

    let Some(session) = icon.session() else {
        let current = state.current.as_deref().context("表示中セッションが不明です")?;
        eprintln!("task-var: {} を新しいウィンドウで起動 (session={current})", icon.name);
        return tmux::new_window(current, &icon.command);
    };

    if !state.existing.iter().any(|s| s == session) {
        eprintln!("task-var: セッション {session} を新規作成");
        tmux::new_session(session, &icon.command)?;
    }
    eprintln!("task-var: セッション {session} へ切替");
    tmux::switch(client, session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_icons_toml_is_valid_and_every_image_loads() {
        // リポジトリの icons.toml が読め、画像もすべて描ける(手で書き換えたときの確認用)
        let icons = Icons::builtin().unwrap();
        for icon in icons.list.iter().chain([&icons.spotify]) {
            crate::icons::load(&icon.image, 32)
                .unwrap_or_else(|e| panic!("{} の画像を描けない: {e:#}", icon.name));
        }
    }

    #[test]
    fn parse_resolves_paths_and_defaults() {
        let base = Path::new("/repo");
        let icons = Icons::parse(
            r#"
            [[icon]]
            name = "tmux"
            image = "assets/tmux.svg"
            command = "tmux-session"
            window = true

            [[icon]]
            name = " shorts "
            image = "~/pics/shorts.png"
            command = "dopagaki"

            [spotify]
            name = "spotify"
            image = "/abs/spotify.svg"
            command = "spotatui"
            "#,
            base,
        )
        .unwrap();
        assert_eq!(icons.list.len(), 2);
        assert_eq!(icons.list[0].image, Path::new("/repo/assets/tmux.svg"), "相対はファイルの場所から");
        assert_eq!(icons.list[0].session(), None, "window はセッションを持たない");
        assert_eq!(icons.list[1].name, "shorts", "前後の空白は落とす");
        assert!(!icons.list[1].window, "window の既定は false");
        assert_eq!(icons.list[1].image, Path::new(&home()).join("pics/shorts.png"));
        assert_eq!(icons.spotify.image, Path::new("/abs/spotify.svg"));
        assert_eq!(icons.sessions().collect::<Vec<_>>(), ["shorts", "spotify"]);
    }

    #[test]
    fn parse_rejects_mistakes() {
        let spotify = "[spotify]\nname = \"spotify\"\nimage = \"a.svg\"\ncommand = \"x\"\n";
        let icon = |body: &str| format!("[[icon]]\n{body}\n{spotify}");
        let err = |text: &str| format!("{:#}", Icons::parse(text, Path::new("/")).unwrap_err());

        // アイコンが 1 個も無いのは許す(列が空になるだけ)
        assert!(Icons::parse(spotify, Path::new("/")).unwrap().list.is_empty());

        assert!(err(&icon("name = \"a\"\nimage = \"a.svg\"\ncomand = \"x\"")).contains("comand"), "綴り違いは弾く");
        assert!(err(&icon("name = \"a\"\nimage = \"a.svg\"")).contains("command"), "書き忘れは弾く");
        assert!(err(&icon("name = \"a.b\"\nimage = \"a.svg\"\ncommand = \"x\"")).contains("a.b"));
        assert!(err(&icon("name = \"\"\nimage = \"a.svg\"\ncommand = \"x\"")).contains("空"));
        assert!(err(&icon("name = \"a\"\nimage = \"a.svg\"\ncommand = \" \"")).contains("command"));
        assert!(err(&icon("name = \"spotify\"\nimage = \"a.svg\"\ncommand = \"x\"")).contains("重なって"));
        assert!(err("[[icon]]\nname = \"a\"\nimage = \"a.svg\"\ncommand = \"x\"\n").contains("spotify"));
    }
}
