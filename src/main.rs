//! task-var: フレームバッファ画面下部にセッション切替タスクバーを表示する。
//!
//! - 下中央に白円アイコン(tmux / Spotify / YouTube Shorts / Bluetooth / ssbrowse)を表示
//! - タッチ入力は touch-server から受け取る(バー矩形を region として申告)
//! - アイコンタップで対応 tmux セッションへ遷移(なければ作成してプログラム実行)
//! - 起動時に端末行数を縮めてバー領域を専有し、終了時に復元する(touch-key と同方式)
//! - セッション状態は 1 秒間隔でポーリングし、リング色(青=表示中 / 灰=存在)を更新。
//!   ポーリングごとに再ブリットするため、fbterm の再描画で消されても 1 秒以内に復活する
//!   (tmux-session スイッチャー実行中は SIGSTOP で止められる想定)

mod actions;
mod bar;
mod fb;
mod icons;
mod term;
mod tmux;
mod touch_client;

use anyhow::Result;
use std::sync::mpsc;
use std::time::Duration;
use touch_client::FracRect;

/// タップとみなす最大移動量(画面に対する割合)。tmux-session / touch-claude と同じ値。
const TAP_FRAC: f64 = 0.05;

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() -> Result<()> {
    let fb = fb::Framebuffer::open()?;
    let (screen_w, screen_h) = (fb.width, fb.height);

    // バー占有領域(下部)。上端を端末セル境界へスナップして隙間を無くす
    let mut bar_h = env_u32("TASKVAR_BAR_H", 88).clamp(24, screen_h / 2);
    let mut bar_y = screen_h - bar_h;
    if let Some(cell_h) = term::cell_height(screen_w) {
        bar_y = bar_y / cell_h * cell_h;
        bar_h = screen_h - bar_y;
    }

    // 端末(fbterm の tmux クライアント)の行数をバー上端まで縮める。
    // pkill / Ctrl-C で終了しても復元されるようシグナルでも戻す。
    let term_guard = term::shrink(screen_w, bar_y);
    if let Some(g) = term_guard.clone() {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        let mut signals = signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM])?;
        std::thread::spawn(move || {
            if signals.forever().next().is_some() {
                g.restore();
                std::process::exit(0);
            }
        });
    }

    let bar = bar::Bar::new(screen_w, bar_h)?;
    let mut buf = vec![0u8; (screen_w * bar_h * 4) as usize];
    let mut state = tmux::State::poll();
    bar.draw(&mut buf, &state);
    fb.blit(0, bar_y, screen_w, bar_h, &buf)?;

    // touch-server クライアント起動(バー矩形を region として申告)
    let region = FracRect {
        left: 0.0,
        top: bar_y as f64 / screen_h as f64,
        right: 1.0,
        bottom: 1.0,
    };
    let (tx, rx) = mpsc::channel::<touch_client::Up>();
    touch_client::spawn(region, tx);

    eprintln!(
        "task-var: 起動 (論理画面 {screen_w}x{screen_h}, 回転 {}, バー領域 y={bar_y} h={bar_h})",
        fb.rotate
    );

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(up) => {
                // タップ判定: 始点→終点の移動が小さいこと
                let moved = (up.fx1 - up.fx0).abs().max((up.fy1 - up.fy0).abs());
                if moved > TAP_FRAC {
                    continue;
                }
                let lx = up.fx1 * screen_w as f64;
                let ly = up.fy1 * screen_h as f64 - bar_y as f64;
                if let Some(i) = bar.hit(lx, ly) {
                    if let Err(e) = actions::activate(&actions::ICONS[i], &state, bar_h) {
                        eprintln!("task-var: {} の起動に失敗: {e:#}", actions::ICONS[i].name);
                    }
                }
                // タップ直後は状態が変わっているはずなので即時更新
                state = tmux::State::poll();
                bar.draw(&mut buf, &state);
                fb.blit(0, bar_y, screen_w, bar_h, &buf)?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let mut s = tmux::State::poll();
                if s != state {
                    // 表示中だったセッションが消えていたら(=起動プログラムの終了で破棄)、
                    // tmux の自動切替先(直近アクティブなセッション)ではなく
                    // 明示的に1番目のセッションへ復帰させる。
                    let destroyed = state.current.as_deref().is_some_and(|cur| {
                        state.existing.iter().any(|e| e == cur)
                            && !s.existing.iter().any(|e| e == cur)
                    });
                    if destroyed {
                        if let (Some(client), Some(first)) = (s.client.clone(), s.first_session.clone()) {
                            if s.current.as_deref() != Some(first.as_str()) {
                                eprintln!("task-var: セッション破棄を検知、1番目のセッション {first} へ復帰");
                                match tmux::switch(&client, &first) {
                                    Ok(()) => s.current = Some(first),
                                    Err(e) => eprintln!("task-var: 復帰switchに失敗: {e:#}"),
                                }
                            }
                        }
                    }
                    state = s;
                    bar.draw(&mut buf, &state);
                }
                // 状態が同じでも再ブリット(fbterm 再描画で消された場合の復活)
                fb.blit(0, bar_y, screen_w, bar_h, &buf)?;
            }
        }
    }
}
