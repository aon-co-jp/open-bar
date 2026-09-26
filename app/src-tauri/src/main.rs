// リリースビルドでWindowsのコンソール窓を出さない
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use open_bar::media::{self, MediaInfo};
use open_bar::output::ResampleFilter;
use open_bar::player::{mode_text, ModeText, PlayMode, Player, Status};
mod presence;

use tauri::{Emitter, Manager, State};

#[tauri::command]
fn probe_file(path: String) -> MediaInfo {
    media::probe(&path)
}

/// フォルダ内の再生できそうなファイル(音声・DSD・動画コンテナ)を名前順に返す(サブフォルダも再帰)。
#[tauri::command]
fn scan_folder(path: String) -> Vec<String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>, depth: u32) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() && depth < 6 {
                walk(&p, out, depth + 1);
            } else if p.is_file() {
                let s = p.to_string_lossy().to_string();
                if media::probe_kind_only(&s) != media::MediaKind::Unknown {
                    out.push(s);
                }
            }
        }
    }
    let mut v = Vec::new();
    walk(std::path::Path::new(&path), &mut v, 0);
    v
}

/// コマンドライン引数で渡されたファイル(「プログラムから開く」・exeへのドラッグ&ドロップ・ファイル関連付け)。
#[tauri::command]
fn startup_files() -> Vec<String> {
    std::env::args().skip(1).filter(|a| std::path::Path::new(a).is_file()).collect()
}

#[tauri::command]
fn player_set_playlist(player: State<Player>, files: Vec<String>) {
    player.set_playlist(files);
}
#[tauri::command]
fn player_play(player: State<Player>, index: usize) {
    player.play(index);
}
#[tauri::command]
fn player_pause(player: State<Player>) {
    player.pause();
}
#[tauri::command]
fn player_resume(player: State<Player>) {
    player.resume();
}
#[tauri::command]
fn player_stop(player: State<Player>) {
    player.stop();
}
#[tauri::command]
fn player_next(player: State<Player>) {
    player.next();
}
#[tauri::command]
fn player_prev(player: State<Player>) {
    player.prev();
}
#[tauri::command]
fn player_seek(player: State<Player>, secs: f64) {
    player.seek(secs);
}
/// 再生モード(A/B/E/D)の一覧と、日英の説明文。
#[tauri::command]
fn player_modes() -> Vec<ModeText> {
    [PlayMode::Exclusive, PlayMode::Shared, PlayMode::ExclusiveUpsample, PlayMode::Dop].into_iter().map(mode_text).collect()
}
#[tauri::command]
fn player_set_mode(player: State<Player>, mode: PlayMode) {
    player.set_mode(mode);
}
#[tauri::command]
fn player_set_filter(player: State<Player>, filter: ResampleFilter) {
    player.set_filter(filter);
}
#[tauri::command]
fn player_set_auto_version(player: State<Player>, on: bool) {
    player.set_auto_version(on);
}
/// 高音補正(MP3向け) / Treble restoration (for MP3 sources)。
#[tauri::command]
fn player_set_treble_restore(player: State<Player>, on: bool) {
    player.set_treble_restore(on);
}
#[tauri::command]
fn player_set_volume(player: State<Player>, volume: f32) {
    player.set_volume(volume);
}
#[tauri::command]
fn player_status(player: State<Player>) -> Status {
    player.status()
}

/// 起動引数(ファイル/`openbar://`のURL)を処理する: URLのトークンは保存、ファイルはフロントへ渡して追加・再生する。
fn handle_args(app: &tauri::AppHandle, args: &[String], cfg_path: &std::path::Path, shared: &presence::Shared) {
    let mut files = Vec::new();
    for a in args {
        if let Some(t) = presence::token_from_url(a) {
            let mut c = shared.lock().unwrap();
            c.token = Some(t);
            presence::save(cfg_path, &c);
        } else if !a.starts_with("openbar://") && std::path::Path::new(a).is_file() {
            files.push(a.clone());
        }
    }
    if !files.is_empty() {
        let _ = app.emit("open-files", files);
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn main() {
    let shared: presence::Shared = std::sync::Arc::new(std::sync::Mutex::new(presence::Config { token: None, enabled: true }));
    let shared_for_second = shared.clone();
    tauri::Builder::default()
        // 2つ目の起動(Webページからの`openbar://`起動やファイルを開く操作)は、既に動いているウィンドウへ渡す
        .plugin(tauri_plugin_single_instance::init(move |app, args, _cwd| {
            let dir = app.path().app_config_dir().unwrap_or_default();
            handle_args(app, &args, &presence::config_path(dir), &shared_for_second);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let dir = app.path().app_config_dir().unwrap_or_default();
            let cfg_path = presence::config_path(dir);
            *shared.lock().unwrap() = presence::load(&cfg_path);
            // 起動時の引数(`openbar://launch?token=…`やファイル)
            let args: Vec<String> = std::env::args().skip(1).collect();
            handle_args(app.handle(), &args, &cfg_path, &shared);
            // 開発時・一部環境でスキームが未登録なら登録する(インストーラー経由なら登録済み)
            #[cfg(any(windows, target_os = "linux"))]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let _ = app.deep_link().register("openbar");
            }
            // 心拍: トークンがあり、有効な間だけ、約10秒ごとに送る
            let handle = app.handle().clone();
            let shared_beat = shared.clone();
            std::thread::spawn(move || {
                let url = std::env::var("OPEN_BAR_PRESENCE_URL").unwrap_or_else(|_| presence::DEFAULT_URL.to_string());
                loop {
                    let cfg = shared_beat.lock().unwrap().clone();
                    if let (Some(t), true) = (cfg.token, cfg.enabled) {
                        let playing = handle.state::<Player>().status().state == open_bar::player::PlayState::Playing;
                        let _ = presence::send_beat(&url, &t, if playing { "playing" } else { "idle" }, env!("CARGO_PKG_VERSION"));
                    }
                    std::thread::sleep(std::time::Duration::from_secs(10));
                }
            });
            Ok(())
        })
        .manage(Player::new())
        .invoke_handler(tauri::generate_handler![probe_file, scan_folder, startup_files, player_set_playlist, player_play, player_pause, player_resume, player_stop, player_next, player_prev, player_seek, player_set_volume, player_status, player_modes, player_set_mode, player_set_filter, player_set_auto_version, player_set_treble_restore])
        .run(tauri::generate_context!())
        .expect("open-barの起動に失敗しました");
}
