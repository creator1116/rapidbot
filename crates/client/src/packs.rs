//! Server resource packs, as far as the server can tell: the prompt, the
//! HTTP download, and the status packets in vanilla's order (`ACCEPTED`,
//! `DOWNLOADED`, `SUCCESSFULLY_LOADED`). The pack itself is not used.
//!
//! The download is real because servers can see it: some check their web
//! server's log for the request before letting a player through. The
//! request is written by hand so it is byte for byte what Java's
//! `HttpURLConnection` sends for `HttpUtil.downloadFile` (checked against
//! the JVM). The TLS handshake of an `https`
//! download is rustls', not Java's.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;
use rapidbot_human::Noise;
use rapidbot_protocol::packets::common::ResourcePackAction as Action;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, info, warn};
use url::Url;
use uuid::Uuid;

use crate::ResourcePackPolicy;

/// `MAX_PACK_SIZE_BYTES`.
const MAX_PACK_SIZE: u64 = 262_144_000;
/// `http.maxRedirects`.
const MAX_REDIRECTS: usize = 20;

/// `ServerPackManager.PackPromptStatus`, plus the prompt being on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Prompt {
    Pending,
    /// The confirm screen is up; the player clicks at `until`.
    Showing { until: Instant },
    Allowed,
    Declined,
}

#[derive(Debug, Clone, PartialEq)]
enum Download {
    /// Not started: the prompt has not been answered.
    Waiting,
    Running,
    Done { valid: bool },
    Failed,
}

#[derive(Debug, Clone)]
struct Pack {
    id: Uuid,
    url: Url,
    hash: Option<String>,
    download: Download,
    /// `DOWNLOADED` has been reported.
    reported: bool,
    /// Loaded by a finished reload (or failed to).
    settled: bool,
}

type DownloadResult = (Uuid, Result<Downloaded, String>);

struct Downloaded {
    sha1: String,
    size: u64,
    /// Starts like a zip file. Anything else fails to load.
    is_zip: bool,
    from_cache: bool,
}

pub(crate) struct PackManager {
    policy: ResourcePackPolicy,
    username: String,
    profile: Uuid,
    cache: Option<PathBuf>,
    noise: Noise,
    prompt: Prompt,
    packs: Vec<Pack>,
    results_tx: Sender<DownloadResult>,
    results: Receiver<DownloadResult>,
    /// A resource reload in progress: when it ends and which packs it loads.
    reload: Option<(Instant, Vec<Uuid>)>,
    downloaded_bytes: u64,
}

/// What the game loop should do after giving the manager a turn.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct PackTurn {
    pub responses: Vec<(Uuid, Action)>,
    /// The player clicked "Disconnect" on a required pack's prompt.
    pub disconnect: bool,
}

impl PackManager {
    pub fn new(policy: ResourcePackPolicy, username: String, profile: Uuid, cache: Option<PathBuf>, seed: u64) -> Self {
        let (results_tx, results) = channel();
        Self {
            policy,
            username,
            profile,
            cache,
            noise: Noise::new(seed),
            // `ServerData.ServerPackStatus`: a saved server set to
            // "enabled" skips the prompt.
            prompt: if policy == ResourcePackPolicy::Enabled { Prompt::Allowed } else { Prompt::Pending },
            packs: Vec::new(),
            results_tx,
            results,
            reload: None,
            downloaded_bytes: 0,
        }
    }

    /// True while the pack prompt is on screen: no keys, no mouse.
    pub fn screen_open(&self) -> bool {
        matches!(self.prompt, Prompt::Showing { .. })
    }

    /// `handleResourcePackPush`.
    pub fn push(&mut self, id: Uuid, url: &str, hash: &str, required: bool) -> PackTurn {
        let mut turn = PackTurn::default();
        // parseResourcePackUrl
        let url = match Url::parse(url) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => url,
            _ => {
                turn.responses.push((id, Action::InvalidUrl));
                return turn;
            }
        };
        // tryParseSha1AsHash: anything else means "no hash".
        let hash = (hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase());
        info!(%url, required, "server resource pack");

        match self.prompt {
            Prompt::Declined => {
                // ServerPackManager.pushPack
                turn.responses.push((id, Action::Declined));
                return turn;
            }
            Prompt::Pending => {
                // Reading the prompt and clicking takes a moment; a
                // required pack is a quicker decision.
                let median = if required { 2.2 } else { 3.0 };
                let delay = self.noise.lognormal(median, 0.45).clamp(0.9, 15.0);
                self.prompt = Prompt::Showing { until: Instant::now() + Duration::from_secs_f64(delay) };
            }
            Prompt::Showing { .. } | Prompt::Allowed => {}
        }
        // markExistingPacksAsRemoved: a pack pushed again replaces itself.
        self.packs.retain(|p| p.id != id);
        self.packs.push(Pack { id, url, hash, download: Download::Waiting, reported: false, settled: false });
        if self.prompt == Prompt::Allowed {
            self.accept(self.packs.len() - 1, &mut turn);
        }
        let _ = required;
        turn
    }

    /// `handleResourcePackPop`: nothing is reported for a pack the server
    /// removes.
    pub fn pop(&mut self, id: Option<Uuid>) {
        match id {
            Some(id) => self.packs.retain(|p| p.id != id),
            None => self.packs.clear(),
        }
    }

    /// `acceptPack`, then the download starts (`updateDownloads`).
    fn accept(&mut self, index: usize, turn: &mut PackTurn) {
        let pack = &mut self.packs[index];
        turn.responses.push((pack.id, Action::Accepted));
        pack.download = Download::Running;
        let request = Request {
            id: pack.id,
            url: pack.url.clone(),
            hash: pack.hash.clone(),
            username: self.username.clone(),
            profile: self.profile,
            cache: self.cache.clone(),
        };
        let results = self.results_tx.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let id = request.id;
                    let result = tokio::time::timeout(Duration::from_secs(300), download(request)).await;
                    let result = result.unwrap_or_else(|_| Err("timed out".into()));
                    let _ = results.send((id, result));
                });
            }
            Err(_) => {
                let _ = results.send((request.id, Err("no async runtime to download with".into())));
            }
        }
    }

    /// Once per frame: the prompt click, finished downloads, the reload.
    pub fn poll(&mut self, required: bool) -> PackTurn {
        let mut turn = PackTurn::default();
        let now = Instant::now();

        if let Prompt::Showing { until } = self.prompt {
            if now >= until {
                if self.policy == ResourcePackPolicy::Decline {
                    // rejectServerPacks: every pack waiting is declined.
                    self.prompt = Prompt::Declined;
                    for pack in self.packs.drain(..) {
                        turn.responses.push((pack.id, Action::Declined));
                    }
                    turn.disconnect = required;
                    return turn;
                }
                // allowServerPacks
                self.prompt = Prompt::Allowed;
                for index in 0..self.packs.len() {
                    if self.packs[index].download == Download::Waiting {
                        self.accept(index, &mut turn);
                    }
                }
            }
        }

        while let Ok((id, result)) = self.results.try_recv() {
            let Some(pack) = self.packs.iter_mut().find(|p| p.id == id && p.download == Download::Running) else { continue };
            match result {
                Ok(done) => {
                    info!(%id, size = done.size, sha1 = %done.sha1, cached = done.from_cache, "resource pack downloaded");
                    self.downloaded_bytes += done.size;
                    pack.download = Download::Done { valid: done.is_zip };
                }
                Err(error) => {
                    warn!(%id, "resource pack download failed: {error}");
                    pack.download = Download::Failed;
                }
            }
        }
        // ServerPackManager.updateDownloads / cleanupRemovedPacks.
        for pack in &mut self.packs {
            match pack.download {
                Download::Done { .. } if !pack.reported => {
                    pack.reported = true;
                    turn.responses.push((pack.id, Action::Downloaded));
                }
                Download::Failed if !pack.settled => {
                    pack.settled = true;
                    turn.responses.push((pack.id, Action::FailedDownload));
                }
                _ => {}
            }
        }

        // triggerReloadIfNeeded: once nothing is downloading, one reload
        // loads every pack that is ready.
        let downloading = self.packs.iter().any(|p| matches!(p.download, Download::Running | Download::Waiting));
        if self.reload.is_none() && !downloading {
            let ready: Vec<Uuid> =
                self.packs.iter().filter(|p| matches!(p.download, Download::Done { .. }) && !p.settled).map(|p| p.id).collect();
            if !ready.is_empty() {
                // A resource reload takes a real client seconds: longer
                // with more to load.
                let megabytes = self.downloaded_bytes as f64 / 1_048_576.0;
                let seconds = self.noise.lognormal(2.6, 0.3).clamp(1.2, 9.0) + megabytes * 0.04;
                debug!(seconds, packs = ready.len(), "reloading resources");
                self.reload = Some((now + Duration::from_secs_f64(seconds), ready));
            }
        }
        if let Some((_, ready)) = self.reload.take_if(|(at, _)| now >= *at) {
            self.downloaded_bytes = 0;
            for pack in self.packs.iter_mut().filter(|p| ready.contains(&p.id)) {
                pack.settled = true;
                let valid = matches!(pack.download, Download::Done { valid: true });
                turn.responses.push((pack.id, if valid { Action::SuccessfullyLoaded } else { Action::FailedReload }));
            }
        }
        turn
    }
}

struct Request {
    id: Uuid,
    url: Url,
    hash: Option<String>,
    username: String,
    profile: Uuid,
    cache: Option<PathBuf>,
}

/// `HttpUtil.downloadFile`: the cached copy if its hash is the one asked
/// for, else an HTTP GET, hashed as it arrives.
async fn download(request: Request) -> Result<Downloaded, String> {
    // Vanilla keeps each pack as `downloads/<pack id>/<sha1>`. Only the
    // fact that it is there matters here, so the cache holds empty files.
    let marker = match (&request.cache, &request.hash) {
        (Some(dir), Some(hash)) => Some(dir.join(request.id.to_string()).join(hash)),
        _ => None,
    };
    if let (Some(marker), Some(hash)) = (&marker, &request.hash) {
        if marker.exists() {
            return Ok(Downloaded { sha1: hash.clone(), size: 0, is_zip: true, from_cache: true });
        }
    }

    let body = fetch(&request).await?;
    let sha1 = Sha1::digest(&body).iter().map(|b| format!("{b:02x}")).collect::<String>();
    if let Some(hash) = &request.hash {
        if *hash != sha1 {
            return Err(format!("hash of downloaded file ({sha1}) did not match requested ({hash})"));
        }
    }
    let is_zip = body.starts_with(b"PK");
    if let Some(dir) = &request.cache {
        let marker = dir.join(request.id.to_string()).join(&sha1);
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker, b"");
    }
    Ok(Downloaded { sha1, size: body.len() as u64, is_zip, from_cache: false })
}

/// The request line and headers `HttpURLConnection` writes. `Map.of`
/// iterates the six custom headers in an order that differs from one JVM
/// run to the next; Java's own three always follow.
fn request_bytes(url: &Url, username: &str, profile: Uuid, shuffle: &mut impl rand::Rng) -> Vec<u8> {
    let mut custom = [
        ("X-Minecraft-Username", username.to_owned()),
        // UndashedUuid.toString
        ("X-Minecraft-UUID", profile.simple().to_string()),
        ("X-Minecraft-Version", rapidbot_protocol::VERSION_NAME.to_owned()),
        ("X-Minecraft-Version-ID", rapidbot_protocol::VERSION_ID.to_owned()),
        ("X-Minecraft-Pack-Format", rapidbot_protocol::RESOURCE_PACK_FORMAT.to_owned()),
        ("User-Agent", format!("Minecraft Java/{}", rapidbot_protocol::VERSION_NAME)),
    ];
    custom.shuffle(shuffle);
    let file = &url[url::Position::BeforePath..url::Position::AfterQuery];
    let mut out = format!("GET {} HTTP/1.1\r\n", if file.is_empty() { "/" } else { file });
    for (name, value) in custom {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    // The port only when it is not the scheme's default.
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => out.push_str(&format!("Host: {host}:{port}\r\n")),
        None => out.push_str(&format!("Host: {host}\r\n")),
    }
    out.push_str("Accept: */*\r\nConnection: keep-alive\r\n\r\n");
    out.into_bytes()
}

/// GET with redirects followed the way `setInstanceFollowRedirects(true)`
/// does: up to 20, and never from one scheme to the other (the redirect's
/// own body is then what was "downloaded").
async fn fetch(request: &Request) -> Result<Vec<u8>, String> {
    let mut url = request.url.clone();
    // One order for the whole download, as within one JVM run.
    let seed: u64 = rand::random();
    for _ in 0..=MAX_REDIRECTS {
        let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(seed);
        let bytes = request_bytes(&url, &request.username, request.profile, &mut rng);
        let response = round_trip(&url, &bytes).await?;
        if matches!(response.status, 300..=303 | 305 | 307 | 308) {
            if let Some(next) = response.location.as_deref().and_then(|l| url.join(l).ok()) {
                if next.scheme() == url.scheme() {
                    url = next;
                    continue;
                }
            }
        }
        if response.status >= 400 {
            return Err(format!("server returned HTTP {}", response.status));
        }
        return Ok(response.body);
    }
    Err("too many redirects".into())
}

struct Response {
    status: u16,
    location: Option<String>,
    body: Vec<u8>,
}

async fn round_trip(url: &Url, request: &[u8]) -> Result<Response, String> {
    let host = url.host_str().ok_or("no host in URL")?.trim_matches(['[', ']']).to_owned();
    let port = url.port_or_known_default().ok_or("no port")?;
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await.map_err(|e| format!("connect: {e}"))?;
    if url.scheme() == "https" {
        use tokio_rustls::rustls;
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("tls: {e}"))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host).map_err(|e| format!("tls name: {e}"))?;
        let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .map_err(|e| format!("tls: {e}"))?;
        exchange(stream, request).await
    } else {
        exchange(tcp, request).await
    }
}

/// Writes the request and reads one HTTP/1.1 response.
async fn exchange(mut stream: impl AsyncRead + AsyncWrite + Unpin, request: &[u8]) -> Result<Response, String> {
    let io = |e: std::io::Error| format!("http: {e}");
    stream.write_all(request).await.map_err(io)?;
    stream.flush().await.map_err(io)?;

    // Head: up to the blank line.
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if buffer.len() > 65_536 {
            return Err("response headers too long".into());
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.map_err(io)?;
        if n == 0 {
            return Err("connection closed before the response".into());
        }
        buffer.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|s| s.parse().ok()).ok_or("bad status line")?;
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
        })
    };
    let location = header("location");
    let mut body = buffer[head_end..].to_vec();

    if header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        // Read to the end, then undo the chunking.
        read_to_limit(&mut stream, &mut body, MAX_PACK_SIZE * 2).await?;
        body = dechunk(&body)?;
    } else if let Some(length) = header("content-length").and_then(|v| v.parse::<u64>().ok()) {
        if length > MAX_PACK_SIZE {
            return Err(format!("filesize is bigger than maximum allowed (file is {length}, limit is {MAX_PACK_SIZE})"));
        }
        while (body.len() as u64) < length {
            let mut chunk = vec![0u8; 65_536];
            let n = stream.read(&mut chunk).await.map_err(io)?;
            if n == 0 {
                return Err("connection closed mid-download".into());
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(length as usize);
    } else {
        read_to_limit(&mut stream, &mut body, MAX_PACK_SIZE).await?;
    }
    if body.len() as u64 > MAX_PACK_SIZE {
        return Err("filesize was bigger than maximum allowed".into());
    }
    Ok(Response { status, location, body })
}

async fn read_to_limit(stream: &mut (impl AsyncRead + Unpin), body: &mut Vec<u8>, limit: u64) -> Result<(), String> {
    let mut chunk = vec![0u8; 65_536];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => return Ok(()),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            // Servers often drop the connection without a TLS goodbye.
            Err(_) if !body.is_empty() => return Ok(()),
            Err(e) => return Err(format!("http: {e}")),
        }
        if body.len() as u64 > limit {
            return Err("filesize was bigger than maximum allowed".into());
        }
        // A chunked body says when it is over.
        if body.ends_with(b"0\r\n\r\n") {
            return Ok(());
        }
    }
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n").ok_or("bad chunked body")?;
        let size_text = std::str::from_utf8(&data[..line_end]).map_err(|_| "bad chunk size")?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk size")?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 {
            return Err("truncated chunked body".into());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::*;

    fn profile() -> Uuid {
        Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap()
    }

    #[test]
    fn request_is_what_java_sends() {
        // Captured from HttpURLConnection on the JVM (headers set from
        // `Map.of`, whose order varies by run):
        //
        //   GET /packs/a%20b.zip?x=1 HTTP/1.1
        //   <the six custom headers, in some order>
        //   Host: 127.0.0.1:57441
        //   Accept: */*
        //   Connection: keep-alive
        let url = Url::parse("http://127.0.0.1:57441/packs/a%20b.zip?x=1").unwrap();
        let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(1);
        let text = String::from_utf8(request_bytes(&url, "Steve", profile(), &mut rng)).unwrap();
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines[0], "GET /packs/a%20b.zip?x=1 HTTP/1.1");
        let mut custom: Vec<&str> = lines[1..7].to_vec();
        custom.sort_unstable();
        assert_eq!(
            custom,
            [
                "User-Agent: Minecraft Java/26.3",
                "X-Minecraft-Pack-Format: 97.1",
                "X-Minecraft-UUID: 0123456789abcdef0123456789abcdef",
                "X-Minecraft-Username: Steve",
                "X-Minecraft-Version-ID: 26.3",
                "X-Minecraft-Version: 26.3",
            ]
        );
        assert_eq!(lines[7..], ["Host: 127.0.0.1:57441", "Accept: */*", "Connection: keep-alive", "", ""]);

        // Default ports are left out of Host; an empty path is "/".
        let url = Url::parse("https://cdn.example.net").unwrap();
        let text = String::from_utf8(request_bytes(&url, "Steve", profile(), &mut rng)).unwrap();
        assert!(text.starts_with("GET / HTTP/1.1\r\n"));
        assert!(text.contains("\r\nHost: cdn.example.net\r\nAccept: */*\r\n"));
        // The order differs between "runs".
        let orders: std::collections::HashSet<String> = (0..20)
            .map(|seed| {
                let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(seed);
                String::from_utf8(request_bytes(&url, "Steve", profile(), &mut rng)).unwrap()
            })
            .collect();
        assert!(orders.len() > 5);
    }

    #[test]
    fn chunked_bodies() {
        assert_eq!(dechunk(b"4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n").unwrap(), b"Wikipedia");
        assert!(dechunk(b"4\r\nWi").is_err());
    }

    /// Serves `pack` at /pack.zip (and a redirect to it at /moved), and
    /// keeps the requests it saw.
    async fn serve(pack: Vec<u8>) -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let mut buffer = vec![0u8; 4096];
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
                let path = request.split(' ').nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(request);
                let response = match path.as_str() {
                    "/pack.zip" => {
                        let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", pack.len()).into_bytes();
                        r.extend_from_slice(&pack);
                        r
                    }
                    "/moved" => b"HTTP/1.1 302 Found\r\nLocation: /pack.zip\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                };
                let _ = socket.write_all(&response).await;
            }
        });
        (port, seen)
    }

    async fn settle(manager: &mut PackManager, wanted: usize) -> Vec<(Uuid, Action)> {
        let mut all = Vec::new();
        for _ in 0..2_000 {
            all.extend(manager.poll(false).responses);
            if all.len() >= wanted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        all
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accepts_downloads_and_loads() {
        let pack = b"PK\x03\x04 pretend zip".to_vec();
        let sha1 = Sha1::digest(&pack).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let (port, seen) = serve(pack).await;
        let id = Uuid::from_u128(1);

        // Server saved with packs enabled: no prompt, straight to ACCEPTED.
        let mut manager = PackManager::new(ResourcePackPolicy::Enabled, "Steve".into(), profile(), None, 1);
        let turn = manager.push(id, &format!("http://127.0.0.1:{port}/moved"), &sha1.to_uppercase(), true);
        assert_eq!(turn.responses, [(id, Action::Accepted)]);
        assert!(!manager.screen_open());
        let rest = settle(&mut manager, 2).await;
        assert_eq!(rest, [(id, Action::Downloaded), (id, Action::SuccessfullyLoaded)]);
        // The redirect was followed with the same headers.
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].starts_with("GET /moved HTTP/1.1\r\n") && seen[1].starts_with("GET /pack.zip HTTP/1.1\r\n"));
        assert_eq!(seen[0].replace("/moved", "/pack.zip"), seen[1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prompt_failures_and_declining() {
        let (port, _) = serve(b"not a zip".to_vec()).await;
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));

        // With a prompt: nothing until the click, then both packs at once.
        let mut manager = PackManager::new(ResourcePackPolicy::Accept, "Steve".into(), profile(), None, 1);
        assert!(manager.push(a, &format!("http://127.0.0.1:{port}/pack.zip"), "", false).responses.is_empty());
        assert!(manager.push(b, &format!("http://127.0.0.1:{port}/missing.zip"), "", false).responses.is_empty());
        assert!(manager.screen_open());
        let all = settle(&mut manager, 5).await;
        assert!(!manager.screen_open());
        assert_eq!(all[..2], [(a, Action::Accepted), (b, Action::Accepted)]);
        // One loads badly (not a zip), the other was a 404.
        assert!(all.contains(&(a, Action::Downloaded)) && all.contains(&(b, Action::FailedDownload)));
        assert_eq!(all.last(), Some(&(a, Action::FailedReload)));

        // A wrong hash fails the download.
        let turn = manager.push(c, &format!("http://127.0.0.1:{port}/pack.zip"), &"ab".repeat(20), false);
        assert_eq!(turn.responses, [(c, Action::Accepted)], "already allowed: no second prompt");
        assert_eq!(settle(&mut manager, 1).await, [(c, Action::FailedDownload)]);

        // Not a URL the client will fetch.
        assert_eq!(manager.push(c, "ftp://example.net/pack.zip", "", false).responses, [(c, Action::InvalidUrl)]);

        // Declining: after the click, and again at once for later packs.
        let mut manager = PackManager::new(ResourcePackPolicy::Decline, "Steve".into(), profile(), None, 1);
        assert!(manager.push(a, "http://127.0.0.1:1/pack.zip", "", false).responses.is_empty());
        assert_eq!(settle(&mut manager, 1).await, [(a, Action::Declined)]);
        assert_eq!(manager.push(b, "http://127.0.0.1:1/pack.zip", "", false).responses, [(b, Action::Declined)]);
    }
}
