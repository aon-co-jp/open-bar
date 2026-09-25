// リリースビルドでWindowsのコンソール窓を出さない
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use open_bar::media::{self, MediaInfo};
use open_bar::output::ResampleFilter;
use open_bar::player::{mode_text, ModeText, PlayMode, Player, Status};
use tauri::State;

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
fn player_set_volume(player: State<Player>, volume: f32) {
    player.set_volume(volume);
}
#[tauri::command]
fn player_status(player: State<Player>) -> Status {
    player.status()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Player::new())
        .invoke_handler(tauri::generate_handler![probe_file, scan_folder, startup_files, player_set_playlist, player_play, player_pause, player_resume, player_stop, player_next, player_prev, player_seek, player_set_volume, player_status, player_modes, player_set_mode, player_set_filter])
        .run(tauri::generate_context!())
        .expect("open-barの起動に失敗しました");
}
