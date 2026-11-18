//! アイコンの定義(icons.toml)と、タッチされたときの動作。
//!
//! 定義はリポジトリ直下の `icons.toml`(`TASKVAR_ICONS` で差し替え可)を実行時に
//! 読む。デーモンは 1 秒ごとにファイル(と画像)の更新時刻を見て、変わっていれば
//! 読み直してバーを組み直す(main.rs)。書き間違いなら読み直さず前の定義で続ける。
//!
//! - `window = true` のアイコン(tmux)は、常に表示中のセッションに新しいウィンドウを
//!   開いてコマンドを実行する。
//! - それ以外は「セッションがあれば switch のみ / なければ作成してコマンド実行 → switch」。
//! - `hint_session` を書いたアイコンは、キーボード(Ctrl+b f、hint.rs)で選んだときだけ
//!   コマンドを実行せず、そのセッションの最近開いていたウィンドウへ移る(`activate_by_key`)。
//!   tmux アイコンを「タップなら tmux-session、キーならセッション 0 へ戻る」にするため。
//! - Spotify は再生情報パネル(bar.rs)が受け持つので、アイコン列には並べず
//!   `[spotify]` として別に持つ。パネルのタップとアート未取得時の絵がこれを引く。
//! - Melon 枠(追っている曲の TOP100 順位)は `[melon]` に書く。節ごと無ければ
//!   (コメントアウトしたら)枠を出さない。

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
    /// キーボードで選んだときは command を実行せず、このセッションへ移る。
    pub hint_session: Option<String>,
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
    /// Melon 枠。None なら枠を出さない。
    pub melon: Option<MelonDef>,
}

/// Melon 枠の定義(`[melon]`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MelonDef {
    /// 札に置くロゴ(`~` と相対パスは解決済み)。読めなければ札は白地だけ。
    pub image: PathBuf,
    /// 追う曲の Melon の songId。並びが枠に左から並べる順番。
    pub songs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    icon: Vec<Entry>,
    spotify: Entry,
    #[serde(default)]
    melon: Option<MelonEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MelonEntry {
    image: String,
    songs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    image: String,
    command: String,
    #[serde(default)]
    window: bool,
    #[serde(default)]
    hint_session: Option<String>,
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
            let hint_session = e.hint_session.map(|s| s.trim().to_string());
            if let Some(s) = &hint_session {
                ensure!(!s.is_empty(), "{name} の hint_session が空");
                ensure!(!s.contains([':', '.']), "{name} の hint_session に「:」「.」は使えない: {s}");
            }
            Ok(Icon {
                name,
                image: resolve(&e.image, base),
                command: e.command,
                window: e.window,
                hint_session,
            })
        };
        let list = file.icon.into_iter().map(to_icon).collect::<Result<Vec<_>>>()?;
        let spotify = to_icon(file.spotify).context("[spotify]")?;
        let mut seen = std::collections::HashSet::new();
        for icon in list.iter().chain([&spotify]) {
            if !seen.insert(icon.name.as_str()) {
                bail!("name が重なっている: {}", icon.name);
            }
        }
        let melon = file.melon.map(|m| to_melon(m, base)).transpose().context("[melon]")?;
        Ok(Self { list, spotify, melon })
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
                        hint_session: None,
                    },
                    melon: None,
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
            .chain(self.melon.as_ref().map(|m| m.image.as_path()))
            .map(mtime)
            .collect()
    }
}

/// `[melon]` を確かめる。曲が 1 つも無い・songId が数字でないものは弾く
/// (枠ごと消したいならコメントアウトする)。
fn to_melon(m: MelonEntry, base: &Path) -> Result<MelonDef> {
    let songs: Vec<String> = m.songs.iter().map(|s| s.trim().to_string()).collect();
    ensure!(!songs.is_empty(), "songs が空(枠ごと消すなら [melon] をコメントアウトする)");
    for s in &songs {
        ensure!(
            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
            "songId は数字だけ: {s:?}"
        );
    }
    Ok(MelonDef { image: resolve(&m.image, base), songs })
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

/// キーボードで選ばれたときに何をするか(`hint_session` の解釈)。
#[derive(Debug, PartialEq, Eq)]
enum KeyPick<'a> {
    /// タップと同じ(`activate`)。
    Activate,
    /// ほかのセッションを見ているので、そのセッションへ移る。tmux はセッションごとに
    /// 最後に開いていたウィンドウを覚えているので、そこへ戻る。
    Switch(&'a str),
    /// もうそのセッションに居るので、直前に開いていたウィンドウへ移る(Ctrl+b l と同じ)。
    LastWindow(&'a str),
}

fn key_pick<'a>(icon: &'a Icon, state: &tmux::State) -> KeyPick<'a> {
    let Some(sess) = icon.hint_session.as_deref() else { return KeyPick::Activate };
    if !state.existing.iter().any(|s| s == sess) {
        eprintln!("task-var: hint_session のセッション {sess} が無いので、タップと同じ動作にします");
        return KeyPick::Activate;
    }
    if state.current.as_deref() == Some(sess) {
        KeyPick::LastWindow(sess)
    } else {
        KeyPick::Switch(sess)
    }
}

/// キーボード(Ctrl+b f)で選ばれたときの動作。`hint_session` が無ければタップと同じ。
pub fn activate_by_key(icon: &Icon, state: &tmux::State) -> Result<()> {
    let client = state.client.as_deref().context("fbterm の tmux クライアントが見つかりません")?;
    match key_pick(icon, state) {
        KeyPick::Activate => activate(icon, state),
        KeyPick::Switch(sess) => {
            eprintln!("task-var: セッション {sess} の最近のウィンドウへ切替");
            tmux::switch_exact(client, sess)
        }
        KeyPick::LastWindow(sess) => {
            eprintln!("task-var: セッション {sess} の直前のウィンドウへ切替");
            // 直前のウィンドウが無い(ウィンドウが 1 つだけ等)なら、そのまま居ればよい
            if let Err(e) = tmux::last_window(sess) {
                eprintln!("task-var: 直前のウィンドウが無いので動きません: {e:#}");
            }
            Ok(())
        }
    }
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
        assert_eq!(icons.melon, None, "[melon] が無ければ Melon 枠は出さない");
    }

    #[test]
    fn hint_session_only_changes_what_the_keyboard_does() {
        let icons = Icons::parse(
            r#"
            [[icon]]
            name = "tmux"
            image = "a.svg"
            command = "tmux-session"
            window = true
            hint_session = " 0 "

            [[icon]]
            name = "wanna"
            image = "a.svg"
            command = "wanna"

            [spotify]
            name = "spotify"
            image = "a.svg"
            command = "spotatui"
            "#,
            Path::new("/"),
        )
        .unwrap();
        let (tmux, wanna) = (&icons.list[0], &icons.list[1]);
        assert_eq!(tmux.hint_session.as_deref(), Some("0"), "前後の空白は落とす");
        assert_eq!(wanna.hint_session, None, "既定は無し");

        let state = |current: &str| tmux::State {
            client: Some("/dev/pts/1".into()),
            current: Some(current.into()),
            existing: vec!["0".into(), "wanna".into()],
            ..Default::default()
        };
        // ほかのセッションからはセッション 0 へ(最後に開いていたウィンドウに戻る)
        assert_eq!(key_pick(tmux, &state("wanna")), KeyPick::Switch("0"));
        // セッション 0 に居るなら直前のウィンドウへ
        assert_eq!(key_pick(tmux, &state("0")), KeyPick::LastWindow("0"));
        // hint_session が無ければタップと同じ
        assert_eq!(key_pick(wanna, &state("0")), KeyPick::Activate);
        // セッション 0 が無ければタップと同じ(tmux-session を開く)
        let gone = tmux::State { existing: vec!["wanna".into()], ..state("wanna") };
        assert_eq!(key_pick(tmux, &gone), KeyPick::Activate);

        let err = |v: &str| {
            let text = format!("[[icon]]\nname = \"a\"\nimage = \"a.svg\"\ncommand = \"x\"\nhint_session = {v}\n[spotify]\nname = \"spotify\"\nimage = \"a.svg\"\ncommand = \"x\"\n");
            format!("{:#}", Icons::parse(&text, Path::new("/")).unwrap_err())
        };
        assert!(err("\" \"").contains("空"));
        assert!(err("\"0:1\"").contains("0:1"));
        assert!(err("0").contains("hint_session"), "数値ではなく文字列で書く");
    }

    #[test]
    fn parse_reads_the_melon_section() {
        let spotify = "[spotify]\nname = \"spotify\"\nimage = \"a.svg\"\ncommand = \"x\"\n";
        let parse = |melon: &str| Icons::parse(&format!("{spotify}{melon}"), Path::new("/repo"));
        let icons = parse(
            r#"
            [melon]
            image = "assets/melon.png"
            songs = [
              "603442244",  # コメントも書ける
              " 601879407 ",
            ]
            "#,
        )
        .unwrap();
        let m = icons.melon.unwrap();
        assert_eq!(m.image, Path::new("/repo/assets/melon.png"), "相対はファイルの場所から");
        assert_eq!(m.songs, ["603442244", "601879407"], "書いた順・前後の空白は落とす");

        // コメントアウトすれば None(枠ごと消える)
        assert_eq!(parse("# [melon]\n# image = \"m.png\"\n# songs = [\"1\"]\n").unwrap().melon, None);

        let err = |melon: &str| format!("{:#}", parse(melon).unwrap_err());
        assert!(err("[melon]\nimage = \"m.png\"\nsongs = []").contains("空"));
        assert!(err("[melon]\nimage = \"m.png\"\nsongs = [\"60344x\"]").contains("60344x"));
        assert!(err("[melon]\nimage = \"m.png\"\nsongs = [\"\"]").contains("数字"));
        assert!(err("[melon]\nimage = \"m.png\"").contains("songs"), "書き忘れは弾く");
        assert!(err("[melon]\nimage = \"m.png\"\nsong = [\"1\"]").contains("song"), "綴り違いも");
        assert!(parse("[melon]\nimage = \"m.png\"\nsongs = [603442244]").is_err(), "数値は弾く");
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
