//! open-bar Webサーバー: ランディング/Web版プレーヤーの静的配信 + 「どの端末でopen-barが起動中か」を返すプレゼンスAPI。
//!
//! - `POST /api/presence` : アプリ(PC/スマホ/タブレット)やWeb版が数秒ごとに送る心拍。`{token, device, name, app_version, state}`。
//! - `GET  /api/presence?token=…` : そのトークンで直近30秒以内に心拍のあった端末の一覧(Webページが「起動中」表示に使う)。
//!
//! プライバシー: 個人情報は持たない。トークンはブラウザが作るランダム文字列(アカウント無し)で、同じトークンを共有した端末だけが
//! お互いを見られる。保存はメモリのみ(再起動で消える)、曲名などの再生内容は受け取らない(`state`は`idle`/`playing`のみ)。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const ALIVE: Duration = Duration::from_secs(30);
const MAX_TOKENS: usize = 20_000;
const MAX_DEVICES_PER_TOKEN: usize = 16;
const MAX_BODY: usize = 2048;

#[derive(Debug, Clone, Deserialize)]
struct Beat {
    token: String,
    device: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    app_version: String,
    #[serde(default)]
    state: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct DeviceOut {
    device: String,
    name: String,
    app_version: String,
    state: String,
    age_secs: u64,
}

struct Entry {
    device: String,
    name: String,
    app_version: String,
    state: String,
    seen: Instant,
}

/// トークン → (端末キー → 最終心拍)。
#[derive(Default)]
struct Store {
    map: HashMap<String, HashMap<String, Entry>>,
}

fn valid_token(t: &str) -> bool {
    (16..=64).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string()
}

impl Store {
    fn gc(&mut self, now: Instant) {
        for devices in self.map.values_mut() {
            devices.retain(|_, e| now.duration_since(e.seen) <= ALIVE * 2);
        }
        self.map.retain(|_, d| !d.is_empty());
    }

    fn beat(&mut self, b: &Beat, now: Instant) -> Result<(), &'static str> {
        if !valid_token(&b.token) {
            return Err("token");
        }
        if !matches!(b.device.as_str(), "pc" | "phone" | "tablet" | "web") {
            return Err("device");
        }
        self.gc(now);
        if !self.map.contains_key(&b.token) && self.map.len() >= MAX_TOKENS {
            return Err("busy");
        }
        let devices = self.map.entry(b.token.clone()).or_default();
        let name = clean(&b.name, 40);
        let key = format!("{}|{}", b.device, name);
        if !devices.contains_key(&key) && devices.len() >= MAX_DEVICES_PER_TOKEN {
            return Err("too many devices");
        }
        let state = if b.state == "playing" { "playing" } else { "idle" };
        devices.insert(key, Entry { device: b.device.clone(), name, app_version: clean(&b.app_version, 20), state: state.to_string(), seen: now });
        Ok(())
    }

    fn list(&mut self, token: &str, now: Instant) -> Vec<DeviceOut> {
        self.gc(now);
        let mut v: Vec<DeviceOut> = self
            .map
            .get(token)
            .map(|d| d.values().filter(|e| now.duration_since(e.seen) <= ALIVE).map(|e| DeviceOut { device: e.device.clone(), name: e.name.clone(), app_version: e.app_version.clone(), state: e.state.clone(), age_secs: now.duration_since(e.seen).as_secs() }).collect())
            .unwrap_or_default();
        v.sort_by(|a, b| a.device.cmp(&b.device).then(a.name.cmp(&b.name)));
        v
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

/// `/`区切りのURLパスを、静的ディレクトリ配下の安全なファイルパスへ(`..`や絶対パスは拒否)。
fn safe_path(root: &std::path::Path, url_path: &str) -> Option<std::path::PathBuf> {
    let p = url_path.split('?').next().unwrap_or("/");
    let rel = p.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let mut out = root.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') || part.contains(':') || part.starts_with('.') {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

fn json_response(status: u16, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string(body).with_status_code(StatusCode(status));
    r.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    r.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    r.add_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap());
    r
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v.to_string()))
}

fn handle(mut req: Request, root: &std::path::Path, store: &Mutex<Store>) {
    let url = req.url().to_string();
    let method = req.method().clone();
    let path = url.split('?').next().unwrap_or("/").to_string();
    let resp = if path == "/api/presence" && method == Method::Post {
        let mut body = String::new();
        let _ = req.as_reader().take(MAX_BODY as u64 + 1).read_to_string(&mut body);
        if body.len() > MAX_BODY {
            json_response(413, r#"{"ok":false,"error":"too large"}"#.into())
        } else {
            match serde_json::from_str::<Beat>(&body) {
                Ok(b) => match store.lock().unwrap().beat(&b, Instant::now()) {
                    Ok(()) => json_response(200, r#"{"ok":true}"#.into()),
                    Err(e) => json_response(400, format!(r#"{{"ok":false,"error":"{e}"}}"#)),
                },
                Err(_) => json_response(400, r#"{"ok":false,"error":"json"}"#.into()),
            }
        }
    } else if path == "/api/presence" && method == Method::Get {
        match query_param(&url, "token").filter(|t| valid_token(t)) {
            Some(t) => {
                let devices = store.lock().unwrap().list(&t, Instant::now());
                json_response(200, serde_json::json!({ "ok": true, "devices": devices }).to_string())
            }
            None => json_response(400, r#"{"ok":false,"error":"token"}"#.into()),
        }
    } else if path == "/api/presence" && method == Method::Options {
        let mut r = Response::from_string("").with_status_code(StatusCode(204));
        r.add_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap());
        r.add_header(Header::from_bytes("Access-Control-Allow-Methods", "GET, POST, OPTIONS").unwrap());
        r.add_header(Header::from_bytes("Access-Control-Allow-Headers", "Content-Type").unwrap());
        let _ = req.respond(r);
        return;
    } else if method == Method::Get || method == Method::Head {
        match safe_path(root, &url).and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
            Some((p, bytes)) => {
                let mut r = Response::from_data(bytes);
                r.add_header(Header::from_bytes("Content-Type", content_type(&p.to_string_lossy())).unwrap());
                let _ = req.respond(r);
                return;
            }
            None => json_response(404, r#"{"ok":false,"error":"not found"}"#.into()),
        }
    } else {
        json_response(405, r#"{"ok":false,"error":"method"}"#.into())
    };
    let _ = req.respond(resp);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = args.first().cloned().unwrap_or_else(|| "127.0.0.1:8114".to_string());
    let root = std::path::PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "webpage".to_string()));
    let server = Server::http(&addr).unwrap_or_else(|e| panic!("{addr}で待ち受けできません: {e}"));
    eprintln!("open-bar-web: http://{addr}/ (static: {})", root.display());
    let store = std::sync::Arc::new(Mutex::new(Store::default()));
    for req in server.incoming_requests() {
        let (root, store) = (root.clone(), store.clone());
        std::thread::spawn(move || handle(req, &root, &store));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beat(token: &str, device: &str, name: &str, state: &str) -> Beat {
        Beat { token: token.into(), device: device.into(), name: name.into(), app_version: "0.1.1".into(), state: state.into() }
    }
    const T: &str = "abcdefghijklmnop1234";

    #[test]
    fn presence_lists_only_live_devices_of_the_same_token() {
        let mut s = Store::default();
        let t0 = Instant::now();
        s.beat(&beat(T, "pc", "Windows PC", "playing"), t0).unwrap();
        s.beat(&beat(T, "phone", "Pixel", "idle"), t0).unwrap();
        s.beat(&beat("zzzzzzzzzzzzzzzzzzzz", "tablet", "iPad", "idle"), t0).unwrap();
        let l = s.list(T, t0 + Duration::from_secs(5));
        assert_eq!(l.iter().map(|d| d.device.as_str()).collect::<Vec<_>>(), vec!["pc", "phone"], "他のトークンの端末は見えない");
        assert_eq!(l[0].state, "playing");
        // 30秒を超えて心拍が無い端末は消える
        assert!(s.list(T, t0 + Duration::from_secs(31)).is_empty());
    }

    #[test]
    fn a_new_beat_refreshes_and_updates_the_same_device() {
        let mut s = Store::default();
        let t0 = Instant::now();
        s.beat(&beat(T, "pc", "PC", "idle"), t0).unwrap();
        s.beat(&beat(T, "pc", "PC", "playing"), t0 + Duration::from_secs(20)).unwrap();
        let l = s.list(T, t0 + Duration::from_secs(40));
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].state.as_str(), l[0].age_secs), ("playing", 20));
    }

    #[test]
    fn invalid_input_is_rejected() {
        let mut s = Store::default();
        let now = Instant::now();
        assert_eq!(s.beat(&beat("short", "pc", "", ""), now), Err("token"));
        assert_eq!(s.beat(&beat("bad token with spaces!!", "pc", "", ""), now), Err("token"));
        assert_eq!(s.beat(&beat(T, "toaster", "", ""), now), Err("device"));
        for i in 0..MAX_DEVICES_PER_TOKEN {
            s.beat(&beat(T, "web", &format!("tab{i}"), ""), now).unwrap();
        }
        assert_eq!(s.beat(&beat(T, "web", "one-too-many", ""), now), Err("too many devices"));
    }

    #[test]
    fn names_are_sanitized_and_state_is_limited() {
        let mut s = Store::default();
        let now = Instant::now();
        s.beat(&beat(T, "pc", &format!("A\u{0}B\n{}", "x".repeat(100)), "listening to secret song"), now).unwrap();
        let l = s.list(T, now);
        assert!(l[0].name.len() <= 40 && !l[0].name.contains('\u{0}') && !l[0].name.contains('\n'));
        assert_eq!(l[0].state, "idle", "曲名などの自由文は保存しない");
    }

    #[test]
    fn static_paths_cannot_escape_the_root() {
        let root = std::path::Path::new("/srv/web");
        assert_eq!(safe_path(root, "/").unwrap(), root.join("index.html"));
        assert_eq!(safe_path(root, "/app.js?x=1").unwrap(), root.join("app.js"));
        for bad in ["/../etc/passwd", "/a/../../b", "/.git/config", "/a\\b", "/C:/x"] {
            assert!(safe_path(root, bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn end_to_end_over_http() {
        use std::io::{Read as _, Write as _};
        let dir = std::env::temp_dir().join(format!("obar_web_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<h1>hi</h1>").unwrap();
        let server = Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let store = std::sync::Arc::new(Mutex::new(Store::default()));
        let (d2, s2) = (dir.clone(), store.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                handle(req, &d2, &s2);
            }
        });
        let http = |req: String| -> String {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        let body = format!(r#"{{"token":"{T}","device":"pc","name":"PC","app_version":"0.1.1","state":"idle"}}"#);
        let post = http(format!("POST /api/presence HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        assert!(post.starts_with("HTTP/1.1 200"), "{post}");
        let get = http(format!("GET /api/presence?token={T} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"));
        assert!(get.contains(r#""device":"pc""#) && get.contains(r#""ok":true"#), "{get}");
        let page = http("GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".to_string());
        assert!(page.contains("<h1>hi</h1>") && page.contains("text/html"), "{page}");
        let evil = http("GET /../Cargo.toml HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".to_string());
        assert!(evil.starts_with("HTTP/1.1 404"), "{evil}");
        let bad = http("POST /api/presence HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}".to_string());
        assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
