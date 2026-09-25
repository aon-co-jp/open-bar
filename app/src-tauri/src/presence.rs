//! 「この端末でopen-barが起動中」を、Webページ(easy-web.tokyo/open-bar)へ知らせる心拍(プレゼンス)。
//!
//! Webページが`openbar://launch?token=…`でアプリを起動すると、そのトークンを保存し、以後はアプリが起動している間、
//! 約10秒ごとに`{token, device, name, app_version, state}`だけをサーバーへ送る(曲名などの再生内容は送らない)。
//! 同じトークンを持つWebページ(他のPC・スマホ・タブレットのブラウザを含む)が、この端末を「起動中」と表示できる。
//! トークンが無い(Webページ経由で一度も起動していない)間は、何も送らない。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const DEFAULT_URL: &str = "https://easy-web.tokyo/open-bar/api/presence";

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub token: Option<String>,
    /// falseなら心拍を送らない(既定はtrue)。
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

pub fn valid_token(t: &str) -> bool {
    (16..=64).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `openbar://launch?token=XYZ`からトークンを取り出す。他の形式・不正なトークンはNone。
pub fn token_from_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("openbar://")?;
    let q = rest.split_once('?')?.1;
    q.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == "token").map(|(_, v)| v.to_string())).filter(|t| valid_token(t))
}

pub fn load(path: &Path) -> Config {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Config { token: None, enabled: true })
}

pub fn save(path: &Path, cfg: &Config) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, serde_json::to_string_pretty(cfg).unwrap_or_default());
}

pub fn config_path(dir: PathBuf) -> PathBuf {
    dir.join("presence.json")
}

/// 表示名(例: `Windows PC (DESKTOP-ABC)`)。ホスト名は環境変数から(個人名が入りうるので、送るのは先頭40文字まで)。
pub fn device_name() -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let os = if cfg!(target_os = "windows") { "Windows PC" } else if cfg!(target_os = "macos") { "Mac" } else { "Linux PC" };
    if host.is_empty() { os.to_string() } else { format!("{os} ({host})") }
}

/// 心拍を1回送る。失敗(オフライン等)は無視してよい。
pub fn send_beat(url: &str, token: &str, state: &str, version: &str) -> Result<(), String> {
    let body = serde_json::json!({ "token": token, "device": "pc", "name": device_name(), "app_version": version, "state": state });
    ureq::post(url).timeout(std::time::Duration::from_secs(5)).send_json(body).map(|_| ()).map_err(|e| e.to_string())
}

/// 共有のトークン入れ物(設定ファイルと同期)。
pub type Shared = Arc<Mutex<Config>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_extracted_only_from_valid_openbar_urls() {
        assert_eq!(token_from_url("openbar://launch?token=abcdefghijklmnop1234"), Some("abcdefghijklmnop1234".into()));
        assert_eq!(token_from_url("openbar://launch?x=1&token=abcdefghijklmnop1234&y=2"), Some("abcdefghijklmnop1234".into()));
        assert_eq!(token_from_url("openbar://launch"), None);
        assert_eq!(token_from_url("openbar://launch?token=short"), None, "短すぎるトークンは拒否");
        assert_eq!(token_from_url("openbar://launch?token=bad%20token%20chars!!"), None);
        assert_eq!(token_from_url("https://evil.example/?token=abcdefghijklmnop1234"), None, "openbar://以外は無視");
    }

    #[test]
    fn config_round_trips_and_defaults_to_enabled() {
        let dir = std::env::temp_dir().join(format!("obar_pres_{}", std::process::id()));
        let path = config_path(dir.clone());
        assert_eq!(load(&path), Config { token: None, enabled: true });
        let cfg = Config { token: Some("abcdefghijklmnop1234".into()), enabled: false };
        save(&path, &cfg);
        assert_eq!(load(&path), cfg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_beat_reaches_a_real_local_server_with_the_expected_fields() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 4096];
            let mut n = 0;
            // ヘッダと本文が別々のパケットで届くことがあるので、本文(JSONの閉じ括弧)が揃うまで読む
            while !String::from_utf8_lossy(&buf[..n]).contains("\"state\"") || !String::from_utf8_lossy(&buf[..n]).trim_end().ends_with('}') {
                let k = s.read(&mut buf[n..]).unwrap();
                if k == 0 {
                    break;
                }
                n += k;
            }
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}").unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        send_beat(&format!("http://127.0.0.1:{port}/api/presence"), "abcdefghijklmnop1234", "playing", "0.1.1").unwrap();
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /api/presence"), "{req}");
        for needle in ["abcdefghijklmnop1234", "\"device\":\"pc\"", "\"state\":\"playing\"", "0.1.1"] {
            assert!(req.contains(needle), "{needle} が含まれるはず: {req}");
        }
        assert!(!req.contains("wav") && !req.contains("dsf"), "曲名などは送らない");
    }
}
