//! task-var: フレームバッファ画面下部にセッション切替タスクバーを表示する。
//!
//! - 左側に白円アイコン(tmux / Spotify / YouTube Shorts / Bluetooth / ssbrowse /
//!   eduroam / カレンダー)を横一列に表示
//! - 右側に Spotify の再生情報パネル(アルバムアート・曲名・アーティスト・
//!   操作ボタン 5 個・進捗バー)を表示する。表示データは spotatui が書く
//!   `/tmp/spotatui_np.json`、操作とシャッフル/リピート状態は MPRIS。
//! - タッチ入力は touch-server から受け取る(バー矩形を region として申告)
//! - アイコンタップで対応 tmux セッションへ遷移(なければ作成してプログラム実行)
//! - 通常は端末行数を縮めてバー領域を専有し、終了時に復元する(touch-key と同方式)
//! - 全画面クライアントのシーン(`SWIPE_SCENES`: fbhalf / ssbrowse)の間は
//!   「スワイプ表示モード」: 端末縮小を解除して全高を明け渡し、バーは既定で隠す。
//!   画面下端の上スワイプで 3 秒だけバーを出す。表示中は fb-server へバー矩形を
//!   申告し、clip 対応クライアント(fbhalf)にはその領域を避けさせる(重ねても
//!   チカチカしない)。3 秒無操作、またはシーン終了で隠す。
//! - セッション状態は 1 秒間隔でポーリングし、リング色(青=表示中 / 灰=存在)を更新。
//!   ポーリングごとに再ブリットするため、fbterm の再描画で消されても 1 秒以内に復活する
//!   (tmux-session スイッチャー実行中は SIGSTOP で止められる想定)

mod actions;
mod art;
mod bar;
mod fb;
mod fb_client;
mod icons;
mod mpris;
mod np;
mod term;
mod text;
mod tmux;
mod touch_client;

use anyhow::Result;
use bar::{Bar, Hit, NpView};
use mpris::{Pending, PlayerState, Snapshot};
use np::NowPlaying;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use touch_client::FracRect;

/// タップとみなす最大移動量(画面に対する割合)。tmux-session / touch-claude と同じ値。
const TAP_FRAC: f64 = 0.05;

/// スワイプ表示モードで「上スワイプ」とみなす最小の上向き移動量(画面高に対する割合)。
const SWIPE_UP_FRAC: f64 = 0.05;

/// スワイプ表示モードにする対象シーン名(fb-server の scene と一致比較)。
/// これらのシーンでは全画面クライアントに画面を明け渡し、バーは下端からの
/// 上スワイプでだけ一時表示する。
const SWIPE_SCENES: &[&str] = &["fbhalf", "ssbrowse"];

/// スワイプ表示したバーを、無操作で自動的に隠すまでの時間。
const HIDE_AFTER: Duration = Duration::from_secs(3);

/// バー非表示中に「上スワイプ」を待ち受ける画面下端の帯の高さ(画面高に対する割合)。
/// ここだけを task-var が占有し、残りは fbhalf 等が受け取れるようにする。
const SWIPE_ZONE_FRAC: f64 = 0.06;

/// touch-server へ申告する region の優先度。後から起動する全画面クライアント
/// (fbhalf 等)より上に描くので、重なった領域のタッチはこちらが受け取る。
const TOUCH_PRIORITY: i32 = 10;

/// 全面ブリットの最短間隔。この間は進捗バーのためにパネル部分だけを部分ブリットする。
const FULL_BLIT_EVERY: Duration = Duration::from_secs(1);

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// 再生情報まわりの現在値。main のループが保持する。
#[derive(Default)]
struct Np {
    now: Option<NowPlaying>,
    player: Option<PlayerState>,
    /// 取得済みのアルバムアート(URL・BGRA・そこから採った背景色)。
    art: Option<(String, Vec<u8>, art::Accent)>,
    /// ボタンを押した直後の期待値。ポーリングが追いつくまで表示に反映する。
    pending: Option<Pending>,
}

impl Np {
    /// JSON・MPRIS・アート取得スレッドの結果を取り込む。
    ///
    /// 曲情報は spotatui が書く JSON を第一候補にし、それが無い/古いときは
    /// MPRIS の Metadata から組み立てたものを使う(spotatui はネイティブ再生中
    /// だと JSON を更新しないため、実機ではこちらが本命になる)。
    fn refresh(&mut self, art: &art::Art, shared: &Arc<Mutex<Option<Snapshot>>>) {
        let snap = shared.lock().unwrap().clone();
        let mut player = snap.as_ref().map(|s| s.player);
        // 押した直後のボタンは、MPRIS 側が追いつくまで手元の期待値を見せる。
        if let (Some(p), Some(got)) = (&self.pending, player.as_mut()) {
            if !p.overlay(got, Instant::now()) {
                self.pending = None;
            }
        }
        self.player = player;
        self.now = np::read()
            .filter(NowPlaying::is_fresh)
            .or_else(|| snap.and_then(|s| s.np));
        if let Some(url) = self.now.as_ref().and_then(|n| n.art_url.as_deref()) {
            art.request(url);
        }
        if let Some((url, bgra)) = art.take() {
            // 代表色の抽出は取得時の 1 回だけ。毎フレームやる必要はない。
            let accent = art::accent(&bgra);
            self.art = Some((url, bgra, accent));
        }
    }

    /// パネルに描く内容。MPRIS が応答しない(= spotatui が居ない)なら None を
    /// 返してパネルごと消す。JSON は古いまま残りうるので、生存確認は MPRIS 側で行う。
    fn view(&self) -> Option<NpView<'_>> {
        let now = self.now.as_ref()?;
        let player = self.player?;
        // アートは今の曲のものだけ使う(曲送り直後の取り違えを防ぐ)。
        // 背景色も同じアート由来なので、外れたときは既定色へ一緒に戻す。
        let album = self
            .art
            .as_ref()
            .filter(|(url, _, _)| Some(url.as_str()) == now.art_url.as_deref());
        let art = album.map(|(_, bgra, _)| bgra.as_slice());
        let accent = album.map(|(_, _, a)| *a).unwrap_or_default();
        Some(NpView { np: now, player, art, accent })
    }

    /// 再生中かどうか(短周期で回して進捗バーを動かすかの判断に使う)。
    fn is_playing(&self) -> bool {
        self.player.is_some_and(|p| p.playing)
    }
}

/// バー全体を描き直し、パネルを描いたかどうかを返す。
fn redraw(bar: &Bar, buf: &mut [u8], state: &tmux::State, np: &Np) -> bool {
    let view = np.view();
    bar.draw(buf, state, view.as_ref());
    view.is_some()
}

/// バーバッファから矩形 `r` を切り出して連続バイト列にする(部分ブリット用)。
fn sub_rect(buf: &[u8], buf_w: u32, r: bar::Rect) -> Vec<u8> {
    let row = (r.w * 4) as usize;
    let mut out = Vec::with_capacity(row * r.h as usize);
    for j in 0..r.h {
        let off = (((r.y + j) * buf_w + r.x) * 4) as usize;
        out.extend_from_slice(&buf[off..off + row]);
    }
    out
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

    let bar = Bar::new(screen_w, bar_h)?;
    let mut buf = vec![0u8; (screen_w * bar_h * 4) as usize];
    let mut state = tmux::State::poll();

    // 再生情報: MPRIS のポーリングスレッドとアルバムアートの取得スレッド。
    let player_shared: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
    mpris::spawn(player_shared.clone());
    let art = art::Art::spawn(bar.art_side());
    let mut np = Np::default();
    np.refresh(&art, &player_shared);
    let mut np_shown = redraw(&bar, &mut buf, &state, &np);

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
    let mut last_full = Instant::now();
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
            let now_swipe = msg.scene.as_deref().is_some_and(|s| SWIPE_SCENES.contains(&s));
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
                        last_full = Instant::now();
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
                    last_full = Instant::now();
                }
                visible = msg.visible;
            } else {
                visible = msg.visible;
            }
        }

        // 表示中は自動非表示のチェックと再ブリットのため短周期で回す。
        // 再生中はさらに短くして進捗バーを滑らかに進める。
        let timeout = if bar_shown && swipe_mode {
            Duration::from_millis(200)
        } else if bar_shown && np_shown && np.is_playing() {
            Duration::from_millis(500)
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
                        np.refresh(&art, &player_shared);
                        np_shown = redraw(&bar, &mut buf, &state, &np);
                        show_bar(&fb, &buf)?;
                        bar_shown = true;
                        last_full = Instant::now();
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
                match bar.hit(lx, ly, np_shown) {
                    Some(Hit::Icon(i)) => {
                        if let Err(e) = actions::activate(&actions::ICONS[i], &state) {
                            eprintln!("task-var: {} の起動に失敗: {e:#}", actions::ICONS[i].name);
                        }
                    }
                    Some(Hit::Panel) => {
                        let def = actions::spotify();
                        if let Err(e) = actions::activate(def, &state) {
                            eprintln!("task-var: {} の起動に失敗: {e:#}", def.name);
                        }
                    }
                    Some(Hit::Ctrl(c)) => {
                        if let Some(p) = np.player {
                            eprintln!("task-var: {c:?} を MPRIS へ送信");
                            mpris::activate(c, p);
                            // 手元の状態を先に進めて即座に描き直す。実際の値は
                            // ポーリングで確認し、反映されるまで(最長 5 秒)
                            // この期待値を見せ続ける。
                            let next = p.after(c);
                            np.player = Some(next);
                            np.pending = Some(Pending::new(c, next));
                        }
                    }
                    None => {}
                }
                // タップ直後は状態が変わっているはずなので即時更新
                state = tmux::State::poll();
                np_shown = redraw(&bar, &mut buf, &state, &np);
                if bar_shown {
                    fb.blit(0, bar_y, screen_w, bar_h, &buf)?;
                    last_full = Instant::now();
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
                    }
                }
                // 再生情報は毎ティック取り込んで描き直す(進捗バーを進めるため)。
                np.refresh(&art, &player_shared);
                np_shown = redraw(&bar, &mut buf, &state, &np);

                if bar_shown {
                    // 全面ブリットは 1 秒に 1 回(fbterm/fbhalf の再描画で消された
                    // ときの復活用)。その合間はパネル部分だけを部分ブリットして
                    // /dev/fb0 への書き込み量を抑える。
                    if last_full.elapsed() >= FULL_BLIT_EVERY {
                        fb.blit(0, bar_y, screen_w, bar_h, &buf)?;
                        last_full = Instant::now();
                    } else if np_shown {
                        let r = bar.np_rect();
                        fb.blit(r.x, bar_y + r.y, r.w, r.h, &sub_rect(&buf, screen_w, r))?;
                    }
                }
            }
        }
    }
}
