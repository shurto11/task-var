//! キーボードでアイコンを選ぶヒントモード(fbservo / fbrowse のリンクヒントと同じ考え方)。
//!
//! Ctrl+b f でアイコンの左上にラベルが出て、そのキーを押すとアイコンをタップしたのと
//! 同じ動作をする。Esc かラベル以外のキーで取消。
//!
//! task-var は tmux の外のデーモンでキーボードを読めないので、キーは tmux の
//! キーテーブル `taskvar`(`TABLE`)で受ける。
//!
//! - 入口は `~/.tmux.conf` に書く(キーを選ぶのは利用者なので):
//!   `bind f run-shell -b '<task-var> hint on' \; switch-client -T taskvar`
//!   `hint on` でラベルを出し、次の 1 キーを taskvar テーブルで引かせる
//! - taskvar テーブルの中身は、task-var が icons.toml を読むたびに張り直す(`bind_keys`)。
//!   ラベルのキー → `hint pick <ラベル>`、Esc とそれ以外のキー(Any)→ `hint off`。
//!   Any まで張るのは、ラベル以外のキーをペインへ漏らさないため
//!   (tmux はテーブルに無いキーを root テーブルで引き直し、ペインへ送ってしまう)
//!
//! コマンドは `task-var hint …` が Unix ソケット(clawd の hook と同じもの)へ 1 行送り、
//! デーモンの main ループが受ける。ラベルを出したまま取り残されないよう、1 秒ごとの
//! ポーリングで fbterm クライアントが taskvar テーブルを抜けていたら消す(main.rs)。

use crate::{clawd, tmux};
use anyhow::{bail, Result};
use std::io::Write;
use std::os::unix::net::UnixStream;

/// ヒント用の tmux キーテーブルの名前。`~/.tmux.conf` の入口と揃える。
pub const TABLE: &str = "taskvar";

/// ラベルに使うキー。アイコン列は横一列なので、ホーム列をキーボードの並び順のまま
/// 左から振ると、キーの位置とアイコンの位置が揃う(慣れればラベルを見ずに打てる)。
/// 11 個目からは上の列、下の列の順。
pub const LABELS: &str = "asdfghjkl;qwertyuiopzxcvbnm";

/// i 番目(左から)のアイコンのラベル。ラベルが足りなければ None。
pub fn label(i: usize) -> Option<char> {
    LABELS.chars().nth(i)
}

/// ラベルから、何番目のアイコンかを引く。
pub fn index(label: char) -> Option<usize> {
    LABELS.chars().position(|c| c == label)
}

/// `task-var hint …` からデーモンへ届く指示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// ラベルを出す(Ctrl+b f)。
    On,
    /// ラベルを消す(Esc / ラベル以外のキー)。
    Off,
    /// そのラベルのアイコンを起動して、ラベルを消す。
    Pick(char),
}

impl Cmd {
    /// `on` / `off` / `pick <ラベル>`。
    pub fn parse(arg: &str) -> Option<Self> {
        let words: Vec<&str> = arg.split_whitespace().collect();
        match words.as_slice() {
            ["on"] => Some(Cmd::On),
            ["off"] => Some(Cmd::Off),
            ["pick", l] => {
                let mut cs = l.chars();
                let c = cs.next()?;
                (cs.next().is_none() && index(c).is_some()).then_some(Cmd::Pick(c))
            }
            _ => None,
        }
    }
}

/// `task-var hint <on|off|pick ラベル>`: デーモンへ 1 行送るだけで終わる。
///
/// tmux のキー割り当てから呼ばれるので、デーモンが居なくても黙って成功で返す
/// (失敗を返すと tmux が「returned 1」をペインに出してしまう)。
pub fn notify(args: &[String]) -> Result<()> {
    let arg = args.join(" ");
    if Cmd::parse(&arg).is_none() {
        bail!("usage: task-var hint <on|off|pick ラベル>(ラベルは {LABELS} の 1 文字): {arg:?}");
    }
    let line = serde_json::json!({"cmd": "hint", "arg": arg}).to_string() + "\n";
    if let Ok(mut s) = UnixStream::connect(clawd::socket_path()) {
        let _ = s.write_all(line.as_bytes());
    }
    Ok(())
}

/// sh の 1 語として渡せるよう単引用符で囲む。
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// tmux の引数としてのキー名。tmux は `;` で終わる引数をコマンドの区切りとみなすので、
/// `;` キーは `\;` と書く。
fn tmux_key(c: char) -> String {
    match c {
        ';' => r"\;".to_string(),
        c => c.to_string(),
    }
}

/// taskvar テーブルを張り直す引数列(tmux へ 1 回で渡す)。`exe` は task-var 本体のパス。
fn bind_args(exe: &str, n: usize) -> Vec<String> {
    let run = |arg: &str| format!("{} hint {arg}", sh_quote(exe));
    let bind = |key: &str, arg: &str| -> Vec<String> {
        ["bind-key", "-T", TABLE, key, "run-shell", "-b", &run(arg)].map(String::from).to_vec()
    };
    // tmux 3.4 の `unbind-key -q -a -T` は、テーブルがまだ無いと黙って後続のコマンドまで
    // 止めてしまう。先に 1 つ張ってテーブルを必ず作ってから、前の割り当てごと空にする
    // (アイコンを減らしたときに、もう無いラベルが残らないように)
    let mut cmds = vec![
        bind("Any", "off"),
        ["unbind-key", "-a", "-T", TABLE].map(String::from).to_vec(),
        bind("Any", "off"),
        bind("Escape", "off"),
    ];
    for c in LABELS.chars().take(n) {
        cmds.push(bind(&tmux_key(c), &format!("pick {}", sh_quote(&c.to_string()))));
    }
    cmds.join(&[";".to_string()][..])
}

/// taskvar テーブルを、いまのアイコンの数(`n`)に合わせて張り直す。
/// 起動時と icons.toml を読み直したときに呼ぶ。
pub fn bind_keys(n: usize) {
    let exe = match std::env::current_exe() {
        // 走っている最中に作り直された(cargo build)本体は ` (deleted)` が付いて返るので剥がす
        Ok(p) => p.to_string_lossy().trim_end_matches(" (deleted)").to_string(),
        Err(e) => {
            eprintln!("task-var: 自分のパスが分からないのでヒントのキーを張れません: {e}");
            return;
        }
    };
    let args = bind_args(&exe, n);
    match tmux::run_checked(&args.iter().map(String::as_str).collect::<Vec<_>>()) {
        Ok(()) => eprintln!(
            "task-var: ヒントのキー({TABLE} テーブル)を張りました: {}",
            LABELS.chars().take(n).collect::<String>()
        ),
        Err(e) => eprintln!("task-var: ヒントのキーを張れません: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_home_row_left_to_right() {
        let first: String = (0..10).filter_map(label).collect();
        assert_eq!(first, "asdfghjkl;", "10 個目まではホーム列の並び順");
        assert_eq!(label(10), Some('q'), "11 個目からは上の列");
        assert_eq!(label(LABELS.len()), None, "ラベルが尽きたら付けない");
        for (i, c) in LABELS.chars().enumerate() {
            assert_eq!(index(c), Some(i));
        }
        assert_eq!(index('A'), None, "大文字はラベルではない");
        let mut uniq: Vec<char> = LABELS.chars().collect();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), LABELS.chars().count(), "ラベルが重なっている");
    }

    #[test]
    fn parse_accepts_only_known_commands() {
        assert_eq!(Cmd::parse("on"), Some(Cmd::On));
        assert_eq!(Cmd::parse(" off "), Some(Cmd::Off));
        assert_eq!(Cmd::parse("pick a"), Some(Cmd::Pick('a')));
        assert_eq!(Cmd::parse("pick ;"), Some(Cmd::Pick(';')));
        assert_eq!(Cmd::parse("pick"), None);
        assert_eq!(Cmd::parse("pick ab"), None, "ラベルは 1 文字");
        assert_eq!(Cmd::parse("pick A"), None);
        assert_eq!(Cmd::parse("pick 1"), None);
        assert_eq!(Cmd::parse("on off"), None);
        assert_eq!(Cmd::parse(""), None);
    }

    #[test]
    fn bind_args_rebuild_the_whole_table_in_one_go() {
        let args = bind_args("/opt/task var/task-var", 10);
        let cmds: Vec<Vec<&str>> =
            args.split(|a| a == ";").map(|c| c.iter().map(String::as_str).collect()).collect();
        let exe = "'/opt/task var/task-var'";

        // テーブルを作ってから空にし、Any と Esc は取消
        assert_eq!(cmds[0][..4], ["bind-key", "-T", TABLE, "Any"]);
        assert_eq!(cmds[1], ["unbind-key", "-a", "-T", TABLE]);
        assert_eq!(cmds[2], ["bind-key", "-T", TABLE, "Any", "run-shell", "-b", &format!("{exe} hint off")]);
        assert_eq!(cmds[3][3], "Escape");

        // ラベルはアイコンの数だけ。`;` はキー名を `\;` にし、sh には引用符で渡す
        let labels = &cmds[4..];
        assert_eq!(labels.len(), 10);
        assert_eq!(labels[0], ["bind-key", "-T", TABLE, "a", "run-shell", "-b", &format!("{exe} hint pick 'a'")]);
        assert_eq!(labels[9][3], r"\;");
        assert_eq!(labels[9][6], format!("{exe} hint pick ';'"));
        // 区切りの `;` 以外に、`;` で終わる引数は無い(tmux に区切りと誤読させない)
        assert!(args.iter().all(|a| a == ";" || !a.ends_with(';') || a.ends_with(r"\;")));
    }
}
