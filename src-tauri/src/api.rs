use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Method, Response, Server};

/// Where a Stream Deck on this PC finds Afterclip, with no address typed in.
pub const LOCAL_PORT: u16 = 4467;
/// Where a Stream Deck on another PC finds it. A port of its own, so the two never get in each other's way.
pub const REMOTE_PORT: u16 = 4468;
// How long a stopped server is given to let go of its port.
const RELEASE_TRIES: u32 = 40;
const RELEASE_RETRY: Duration = Duration::from_millis(50);
// A wrong key waits this long for its answer, which makes guessing the key slow.
const WRONG_KEY_DELAY: Duration = Duration::from_millis(400);
// After the API stops, its port can take a moment to come free.
const BIND_TRIES: u32 = 15;
const BIND_RETRY: Duration = Duration::from_millis(200);

/// True for a copy started by the tests, which must not touch a real running copy.
/// Never while elevated: these variables redirect files and open a debug port, and anything can set them.
pub fn is_test_instance() -> bool {
    std::env::var_os("AFTERCLIP_HOME").is_some() && !crate::win32::is_self_elevated()
}

pub trait ApiHandlers: Send + Sync + 'static {
    /// Opens a window on request. Only reachable in a test instance.
    fn test_open(&self, what: &str);
    /// Stops the panel closing on focus loss. Only reachable in a test instance.
    fn test_hold_panel(&self, hold: bool);
    /// Pretends this program is the running game. Empty means none. Only reachable in a test instance.
    fn test_game(&self, exe: &str);
    fn test_controller(&self, kind: &str, report: &str);
    /// Pretends the drive has this much free. Only reachable in a test instance.
    fn test_free_space(&self, bytes: Option<u64>);
    /// Crashes the engine, or stops its buffer as an encoder error would. Only reachable in a test instance.
    fn test_engine(&self, what: &str);
    /// Pretends the push-to-talk key is held or let go. Only reachable in a test instance.
    fn test_talk(&self, held: bool);
    fn clip(&self, seconds: i64) -> Result<Value, String>;
    /// Saves a clip and stars it.
    fn clip_and_star(&self, seconds: i64) -> Result<Value, String>;
    fn toggle_record(&self) -> Result<Value, String>;
    fn toggle_mic(&self) -> Result<Value, String>;
    fn toggle_clean_mic(&self) -> Result<Value, String>;
    fn add_marker(&self) -> Result<Value, String>;
    fn screenshot(&self) -> Result<Value, String>;
    fn open_panel(&self) -> Result<Value, String>;
    fn open_clips_folder(&self) -> Result<Value, String>;
    fn edit_last_clip(&self) -> Result<Value, String>;
    fn copy_last_clip(&self) -> Result<Value, String>;
    fn compress_last_clip(&self) -> Result<Value, String>;
    fn send_last_clip(&self) -> Result<Value, String>;
    fn vertical_last_clip(&self) -> Result<Value, String>;
    fn star_last_clip(&self) -> Result<Value, String>;
    fn recipe_last_clip(&self, name: &str) -> Result<Value, String>;
    fn toggle_camera(&self) -> Result<Value, String>;
    /// The Stream Deck's Hold to Talk key going down or up.
    fn deck_talk(&self, held: bool) -> Result<Value, String>;
    /// Turns every mute-key source up or down by this many percent.
    fn change_mic_volume(&self, percent: i64) -> Result<Value, String>;
    fn status(&self) -> Value;
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

pub fn local_port() -> u16 {
    std::env::var("AFTERCLIP_API_PORT").ok().filter(|_| is_test_instance()).and_then(|p| p.parse().ok()).unwrap_or(LOCAL_PORT)
}

/// This PC's address on the local network, which a Stream Deck on another PC connects to. Choosing a route
/// to an outside address finds it without sending anything.
pub fn lan_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip()).filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
}

fn bind(address: &str) -> Result<Server, String> {
    let mut last = String::new();
    for _ in 0..BIND_TRIES {
        match Server::http(address) {
            Ok(server) => return Ok(server),
            Err(e) => last = e.to_string(),
        }
        thread::sleep(BIND_RETRY);
    }
    Err(format!("Couldn't listen on port {}: {last}", address.rsplit(':').next().unwrap_or(address)))
}

/// Control API for the Stream Deck plugin. Every request needs the key as a bearer token, and it sends no
/// CORS headers, so a web page can't trigger a clip. On 127.0.0.1 it's for this PC only. On 0.0.0.0 it's
/// for a Stream Deck on another PC on the local network. The key is read on every request, so a new one
/// takes effect at once.
pub fn start_api(address: &str, key: Arc<RwLock<String>>, handlers: Arc<dyn ApiHandlers>) -> Result<Arc<Server>, String> {
    let server = Arc::new(bind(address)?);
    serve(Arc::clone(&server), key, handlers);
    Ok(server)
}

/// Stops a server and waits until its port is free. The library wakes its listener by connecting to the
/// address it listens on, which can't work for 0.0.0.0, so this connects to it on this PC instead.
pub fn stop_api(server: Arc<Server>) {
    let port = server.server_addr().to_ip().map(|a| a.port());
    server.unblock();
    drop(server);
    let Some(port) = port else { return };
    let here = SocketAddr::from(([127, 0, 0, 1], port));
    for _ in 0..RELEASE_TRIES {
        thread::sleep(RELEASE_RETRY);
        let _ = TcpStream::connect_timeout(&here, RELEASE_RETRY);
        if TcpListener::bind(("0.0.0.0", port)).is_ok() {
            return;
        }
    }
}

fn serve(server: Arc<Server>, key: Arc<RwLock<String>>, handlers: Arc<dyn ApiHandlers>) {
    let listener = server;
    thread::spawn(move || {
        // incoming_requests ends when the server is told to unblock, which is how the API stops.
        for request in listener.incoming_requests() {
            let expected = format!("Bearer {}", key.read().unwrap());
            let authorized = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .is_some_and(|h| constant_time_eq(h.value.as_str().as_bytes(), expected.as_bytes()));

            let handlers = Arc::clone(&handlers);
            // Clips take a moment, and must not hold up other requests.
            thread::spawn(move || {
                let (code, body) = if authorized {
                    route(request.method(), request.url(), handlers.as_ref())
                } else {
                    thread::sleep(WRONG_KEY_DELAY);
                    (401, json!({ "error": "unauthorized" }))
                };
                let header = Header::from_bytes("Content-Type", "application/json").expect("valid header");
                let response = Response::from_string(body.to_string()).with_status_code(code).with_header(header);
                let _ = request.respond(response);
            });
        }
    });
}


fn route(method: &Method, url: &str, handlers: &dyn ApiHandlers) -> (u16, Value) {
    let result = match (method, url) {
        (Method::Get, "/status") => Ok(handlers.status()),
        (Method::Post, "/record/toggle") => handlers.toggle_record(),
        (Method::Post, "/mic/toggle") => handlers.toggle_mic(),
        (Method::Post, "/mic/clean") => handlers.toggle_clean_mic(),
        (Method::Post, "/marker") => handlers.add_marker(),
        (Method::Post, "/screenshot") => handlers.screenshot(),
        (Method::Post, "/open/panel") => handlers.open_panel(),
        (Method::Post, "/open/clips") => handlers.open_clips_folder(),
        (Method::Post, "/last-clip/edit") => handlers.edit_last_clip(),
        (Method::Post, "/last-clip/copy") => handlers.copy_last_clip(),
        (Method::Post, "/last-clip/compress") => handlers.compress_last_clip(),
        (Method::Post, "/last-clip/discord") => handlers.send_last_clip(),
        (Method::Post, "/last-clip/vertical") => handlers.vertical_last_clip(),
        (Method::Post, "/last-clip/star") => handlers.star_last_clip(),
        (Method::Post, path) if path.starts_with("/last-clip/recipe?") => {
            let name = reqwest::Url::parse(&format!("http://localhost{path}")).ok()
                .and_then(|url| url.query_pairs().find(|(key, _)| key == "name").map(|(_, name)| name.into_owned()));
            match name {
                Some(name) if !name.is_empty() => handlers.recipe_last_clip(&name),
                _ => Err("Set the recipe name in the Stream Deck key settings.".into()),
            }
        }
        (Method::Post, "/camera/toggle") => handlers.toggle_camera(),
        (Method::Post, "/talk/on") => handlers.deck_talk(true),
        (Method::Post, "/talk/off") => handlers.deck_talk(false),

        (Method::Post, volume) if volume.starts_with("/mic/volume/") => match volume.trim_start_matches("/mic/volume/").parse::<i64>() {
            Ok(percent) => handlers.change_mic_volume(percent.clamp(-200, 200)),
            Err(_) => return (404, json!({ "error": "not found" })),
        },
        (Method::Post, space) if space.starts_with("/test/free-space/") && is_test_instance() => {
            handlers.test_free_space(space.trim_start_matches("/test/free-space/").parse().ok());
            Ok(json!({}))
        }
        (Method::Post, "/test/engine/crash" | "/test/engine/stop-replay") if is_test_instance() => {
            handlers.test_engine(url.trim_start_matches("/test/engine/"));
            Ok(json!({}))
        }
        (Method::Post, "/test/talk/on" | "/test/talk/off") if is_test_instance() => {

            handlers.test_talk(url.ends_with("/on"));
            Ok(json!({}))
        }

        (Method::Post, "/test/open/panel" | "/test/open/editor") if is_test_instance() => {
            handlers.test_open(url.rsplit('/').next().unwrap_or(""));
            Ok(json!({}))
        }
        (Method::Post, game) if game.starts_with("/test/game/") && is_test_instance() => {
            handlers.test_game(game.trim_start_matches("/test/game/"));
            Ok(json!({}))
        }
        (Method::Post, pad) if pad.starts_with("/test/controller/") && is_test_instance() => {
            let mut parts = pad.trim_start_matches("/test/controller/").splitn(2, '/');
            handlers.test_controller(parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            Ok(json!({}))
        }
        (Method::Post, "/test/panel-hold/on" | "/test/panel-hold/off") if is_test_instance() => {
            handlers.test_hold_panel(url.ends_with("/on"));
            Ok(json!({}))
        }
        (Method::Post, _) => {
            // /clip/30 saves a clip, and /clip/30/star saves it and stars it.
            let rest = url.strip_prefix("/clip/").unwrap_or("");
            let (length, star) = match rest.strip_suffix("/star") {
                Some(length) => (length, true),
                None => (rest, false),
            };
            match length.parse::<u32>() {
                Ok(seconds) if star => handlers.clip_and_star(seconds as i64),
                Ok(seconds) => handlers.clip(seconds as i64),
                Err(_) => return (404, json!({ "error": "not found" })),
            }
        }
        _ => return (404, json!({ "error": "not found" })),
    };
    match result {
        Ok(value) => (200, value),
        Err(error) => (500, json!({ "error": error })),
    }
}
