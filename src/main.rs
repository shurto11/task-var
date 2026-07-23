//! task-var: フレームバッファ画面下部にセッション切替タスクバーを表示する。
//!
//! - 下中央に白円アイコン(tmux / Spotify / YouTube Shorts / Bluetooth / ssbrowse)を表示
//! - タッチ入力は touch-server から受け取る(バー矩形を region として申告)
//! - アイコンタップで対応 tmux セッションへ遷移(なければ作成してプログラム実行)
//! - 通常は端末行数を縮めてバー領域を専有し、終了時に復元する(touch-key と同方式)
//! - fbhalf シーンの間は「スワイプ表示モード」: 端末縮小を解除して全高を明け渡し、
//!   バーは既定で隠す。下部ストリップの上スワイプで 3 秒だけバーを出す。表示中は
//!   fb-server へバー矩形を申告し、fbhalf にその領域を clip で避けさせる(重ねても
//!   チカチカしない)。3 秒無操作、または fbhalf 終了で隠す。
//! - セッション状態は 1 秒間隔でポーリングし、リング色(青=表示中 / 灰=存在)を更新。
//!   ポーリングごとに再ブリットするため、fbterm の再描画で消されても 1 秒以内に復活する
//!   (tmux-session スイッチャー実行中は SIGSTOP で止められる想定)

mod actions;
mod bar;
mod fb;
mod fb_client;
mod icons;
mod term;
mod tmux;
mod touch_client;

use anyhow::Result;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use touch_client::FracRect;

/// タップとみなす最大移動量(画面に対する割合)。tmux-session / touch-claude と同じ値。
const TAP_FRAC: f64 = 0.05;

/// スワイプ表示モードで「上スワイプ」とみなす最小の上向き移動量(画面高に対する割合)。
const SWIPE_UP_FRAC: f64 = 0.05;

/// スワイプ表示モードでバーを出す対象シーン名(fb-server の scene と一致比較)。
const SWIPE_SCENE: &str = "fbhalf";

/// スワイプ表示したバーを、無操作で自動的に隠すまでの時間。
const HIDE_AFTER: Duration = Duration::from_secs(3);

/// バー非表示中に「上スワイプ」を待ち受ける画面下端の帯の高さ(画面高に対する割合)。
/// ここだけを task-var が占有し、残りは fbhalf 等が受け取れるようにする。
const SWIPE_ZONE_FRAC: f64 = 0.06;

/// touch-server へ申告する region の優先度。後から起動する全画面クライアント
/// (fbhalf 等)より上に描くので、重なった領域のタッチはこちらが受け取る。
const TOUCH_PRIORITY: i32 = 10;

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
    // スワイプ表示モード(fbhalf シーン)では解除して全高を明け渡すため、
    // 現在の縮小状態を共有し、動的にトグルできるようにする。
    // pkill / Ctrl-C で終了しても復元されるようシグナルでも戻す。
    let term_guard: Arc<Mutex<Option<term::TermGuard>>> =
        Arc::new(Mutex::new(term::shrink(screen_w, bar_y)));
    {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        let mut signals = signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM])?;
        let g = term_guard.clone();
        std::thread::spawn(move || {
            if signals.forever().next().is_some() {
                if let Some(guard) = g.lock().unwrap().as_ref() {
                    guard.restore();
                }
                std::process::exit(0);
            }
        });
    }

    let bar = bar::Bar::new(screen_w, bar_h)?;
    let mut buf = vec![0u8; (screen_w * bar_h * 4) as usize];
    let mut state = tmux::State::poll();
    bar.draw(&mut buf, &state);

    // touch-server クライアント起動。バー表示中は帯全体、非表示中は下端だけを
    // region として申告する(残りは fbhalf 等が受け取れる)。優先度を付けて、
    // 後から起動する全画面クライアントにバー領域のタッチを奪われないようにする。
    let bar_region = FracRect {
        left: 0.0,
        top: bar_y as f64 / screen_h as f64,
        right: 1.0,
        bottom: 1.0,
    };
    let swipe_region = FracRect { left: 0.0, top: 1.0 - SWIPE_ZONE_FRAC, right: 1.0, bottom: 1.0 };
    let touch_region = Arc::new(Mutex::new(bar_region));
    let (tx, rx) = mpsc::channel::<touch_client::Up>();
    touch_client::spawn(touch_region.clone(), TOUCH_PRIORITY, tx);

    // バーの表示状態に合わせてタッチ領域を切り替える。
    let set_touch_region = |shown: bool| {
        *touch_region.lock().unwrap() = if shown { bar_region } else { swipe_region };
    };

    // fb-server クライアント起動。バーの描画領域(物理 fb 座標)を、バーを実際に
    // 表示している間だけ申告する(下位レイヤー = fbhalf にその矩形を避けさせる)。
    let (bx, by, bw, bh) = fb.phys_region(0, bar_y, screen_w, bar_h);
    let bar_rect = fb_client::Rect { x: bx, y: by, w: bw, h: bh };
    let fb_rect: Arc<Mutex<Option<fb_client::Rect>>> = Arc::new(Mutex::new(Some(bar_rect)));
    let (vtx, vrx) = mpsc::channel::<fb_client::VisMsg>();
    fb_client::spawn("task-var", fb_rect.clone(), vtx);

    // バーの描画/非表示を rect 申告とセットで切り替えるヘルパ。
    let set_rect = |shown: bool| {
        *fb_rect.lock().unwrap() = shown.then_some(bar_rect);
    };

    eprintln!(
        "task-var: 起動 (論理画面 {screen_w}x{screen_h}, 回転 {}, バー領域 y={bar_y} h={bar_h})",
        fb.rotate
    );

    // 表示状態。
    let mut visible = true; // fb-server の可視許可(スワイプ以外のシーン用)
    let mut swipe_mode = false; // 現在 SWIPE_SCENE か
    let mut bar_shown = true; // バーを実際に描画しているか
    let mut hide_at: Option<Instant> = None; // スワイプ表示の自動非表示期限
    let mut last_poll = Instant::now();
    // 初期状態を描画(通常モードで可視)
    fb.blit(0, bar_y, screen_w, bar_h, &buf)?;

    // バーを表示する(rect 申告 + タッチ領域を帯全体へ + 描画)。
    let show_bar = |fb: &fb::Framebuffer, buf: &[u8]| -> Result<()> {
        set_rect(true);
        set_touch_region(true);
        fb.blit(0, bar_y, screen_w, bar_h, buf)?;
        Ok(())
    };
    // バーを隠す。通常モードでは黒クリア。スワイプモードでは rect を取り消して
    // fbhalf に再描画で埋めさせる(黒フラッシュを避ける)。
    // タッチ領域はスワイプ検知用の下端だけに縮める。
    let hide_bar = |fb: &fb::Framebuffer, in_swipe: bool| -> Result<()> {
        set_rect(false);
        set_touch_region(false);
        if !in_swipe {
            let _ = fb.clear(0, bar_y, screen_w, bar_h);
        }
        Ok(())
    };

    // 端末縮小のトグル(スワイプモードでは解除して全高を明け渡す)。
    let set_shrunk = |want: bool| {
        let mut g = term_guard.lock().unwrap();
        match (want, g.is_some()) {
            (true, false) => *g = term::shrink(screen_w, bar_y),
            (false, true) => {
                if let Some(guard) = g.take() {
                    guard.restore();
                }
            }
            _ => {}
        }
    };

    loop {
        // fb-server からの通知を反映。scene によりモードを切り替える。
        while let Ok(msg) = vrx.try_recv() {
            let now_swipe = msg.scene.as_deref() == Some(SWIPE_SCENE);
            if now_swipe != swipe_mode {
                swipe_mode = now_swipe;
                if swipe_mode {
                    // スワイプモードへ: 端末縮小を解除して全高を明け渡し、バーを隠す。
                    set_shrunk(false);
                    if bar_shown {
                        hide_bar(&fb, true)?;
                        bar_shown = false;
                    }
                    hide_at = None;
                } else {
                    // 通常モードへ戻る: 端末を縮小し直し、可視ならバーを出す。
                    set_shrunk(true);
                    hide_at = None;
                    if visible && !bar_shown {
                        show_bar(&fb, &buf)?;
                        bar_shown = true;
                    }
                }
            }
            // 通常モードのみ fb-server の可視許可に従う(スワイプモードは自前管理)。
            if !swipe_mode {
                if visible && !msg.visible && bar_shown {
                    hide_bar(&fb, false)?;
                    bar_shown = false;
                } else if !visible && msg.visible && !bar_shown {
                    show_bar(&fb, &buf)?;
                    bar_shown = true;
                }
                visible = msg.visible;
            } else {
                visible = msg.visible;
            }
        }

        // 表示中は自動非表示のチェックと再ブリットのため短周期で回す。
        let timeout = if bar_shown && swipe_mode {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(1)
        };

        match rx.recv_timeout(timeout) {
            Ok(up) => {
                let dx = up.fx1 - up.fx0;
                let dy = up.fy1 - up.fy0;
                let moved = dx.abs().max(dy.abs());

                // スワイプモードでバー非表示中: 上スワイプでのみ表示する。
                if swipe_mode && !bar_shown {
                    let swiped_up = -dy > SWIPE_UP_FRAC && (-dy) > dx.abs();
                    if swiped_up {
                        state = tmux::State::poll();
                        bar.draw(&mut buf, &state);
                        show_bar(&fb, &buf)?;
                        bar_shown = true;
                        hide_at = Some(Instant::now() + HIDE_AFTER);
                    }
                    continue;
                }

                // ここに来るのはバー表示中。タッチがあったので自動非表示を延長。
                if swipe_mode {
                    hide_at = Some(Instant::now() + HIDE_AFTER);
                }

                // タップ判定: 始点→終点の移動が小さいこと
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
                if bar_shown {
                    fb.blit(0, bar_y, screen_w, bar_h, &buf)?;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                // スワイプ表示の期限切れで自動的に隠す。
                if swipe_mode && bar_shown && hide_at.is_some_and(|t| Instant::now() >= t) {
                    hide_bar(&fb, true)?;
                    bar_shown = false;
                    hide_at = None;
                    continue;
                }

                // tmux 状態のポーリングは約1秒間隔に抑える(表示中は短周期で回るため)。
                if last_poll.elapsed() >= Duration::from_secs(1) {
                    last_poll = Instant::now();
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
                            if let (Some(client), Some(first)) =
                                (s.client.clone(), s.first_session.clone())
                            {
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
                }
                // 状態が同じでも再ブリット(fbterm/fbhalf 再描画で消された場合の復活)。
                // スワイプ表示中は clip が効くまでの取りこぼしをここで埋める。
                if bar_shown {
                    fb.blit(0, bar_y, screen_w, bar_h, &buf)?;
                }
            }
        }
    }
}
