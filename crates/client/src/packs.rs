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

use std::io::{Cursor, Read};
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
use zip::ZipArchive;

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
    Showing {
        until: Instant,
    },
    Allowed,
    Declined,
}

#[derive(Debug, Clone, PartialEq)]
enum Download {
    /// Not started: the prompt has not been answered.
    Waiting,
    Running,
    Done {
        valid: bool,
    },
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
    /// Passes our best-effort `Pack.readPackMetadata`: a zip with a root
    /// `pack.mcmeta`, a `pack` object, a component-shaped description, and
    /// parseable feature and overlay sections. Format errors use vanilla's
    /// fallback rather than failing the pack.
    valid: bool,
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
    pub fn new(
        policy: ResourcePackPolicy,
        username: String,
        profile: Uuid,
        cache: Option<PathBuf>,
        seed: u64,
    ) -> Self {
        let (results_tx, results) = channel();
        Self {
            policy,
            username,
            profile,
            cache,
            noise: Noise::new(seed),
            // `ServerData.ServerPackStatus`: a saved server set to
            // "enabled" skips the prompt.
            prompt: if policy == ResourcePackPolicy::Enabled {
                Prompt::Allowed
            } else {
                Prompt::Pending
            },
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
        let hash = (hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase());
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
                self.prompt = Prompt::Showing {
                    until: Instant::now() + Duration::from_secs_f64(delay),
                };
            }
            Prompt::Showing { .. } | Prompt::Allowed => {}
        }
        // markExistingPacksAsRemoved: a pack pushed again replaces itself.
        self.packs.retain(|p| p.id != id);
        self.packs.push(Pack {
            id,
            url,
            hash,
            download: Download::Waiting,
            reported: false,
            settled: false,
        });
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
                    let result =
                        tokio::time::timeout(Duration::from_secs(300), download(request)).await;
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

        if let Prompt::Showing { until } = self.prompt
            && now >= until
        {
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

        while let Ok((id, result)) = self.results.try_recv() {
            let Some(pack) = self
                .packs
                .iter_mut()
                .find(|p| p.id == id && p.download == Download::Running)
            else {
                continue;
            };
            match result {
                Ok(done) => {
                    info!(%id, size = done.size, sha1 = %done.sha1, cached = done.from_cache, "resource pack downloaded");
                    self.downloaded_bytes += done.size;
                    pack.download = Download::Done { valid: done.valid };
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
        let downloading = self
            .packs
            .iter()
            .any(|p| matches!(p.download, Download::Running | Download::Waiting));
        if self.reload.is_none() && !downloading {
            let ready: Vec<Uuid> = self
                .packs
                .iter()
                .filter(|p| matches!(p.download, Download::Done { .. }) && !p.settled)
                .map(|p| p.id)
                .collect();
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
            // `DownloadedPackSource.loadRequestedPacks` returns null when any
            // pack in the batch has bad metadata; the recovery reload then
            // starts without the new packs. So every pack in this reload
            // reports the failure, not just the bad one.
            // A pack popped while the reload was pending is no longer in
            // `self.packs`; don't let its absence fail the others.
            let batch_loads = ready.iter().all(|id| {
                self.packs
                    .iter()
                    .find(|p| p.id == *id)
                    .is_none_or(|p| matches!(p.download, Download::Done { valid: true }))
            });
            for pack in self.packs.iter_mut().filter(|p| ready.contains(&p.id)) {
                pack.settled = true;
                turn.responses.push((
                    pack.id,
                    if batch_loads {
                        Action::SuccessfullyLoaded
                    } else {
                        Action::FailedReload
                    },
                ));
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
/// for, else an HTTP GET, hashed as it arrives. A cached copy is also checked
/// the way a resource reload would check it; an old empty marker from earlier
/// versions is simply downloaded again.
async fn download(request: Request) -> Result<Downloaded, String> {
    // Vanilla keeps each pack as `downloads/<pack id>/<sha1>`.
    let marker = match (&request.cache, &request.hash) {
        (Some(dir), Some(hash)) => Some(dir.join(request.id.to_string()).join(hash)),
        _ => None,
    };
    if let (Some(marker), Some(hash)) = (&marker, &request.hash)
        && marker.exists()
    {
        match std::fs::read(marker) {
            Ok(body) if sha1_hex(&body) == *hash => {
                let valid = validate_pack(&body);
                if let Err(error) = &valid {
                    warn!(%error, "cached resource pack would fail vanilla's reload");
                }
                return Ok(Downloaded {
                    sha1: hash.clone(),
                    size: body.len() as u64,
                    valid: valid.is_ok(),
                    from_cache: true,
                });
            }
            Ok(_) => warn!("cached resource pack hash does not match; downloading it again"),
            Err(error) => {
                warn!(%error, "could not read cached resource pack; downloading it again")
            }
        }
        let _ = std::fs::remove_file(marker);
    }

    let body = fetch(&request).await?;
    let sha1 = sha1_hex(&body);
    if let Some(hash) = &request.hash
        && *hash != sha1
    {
        return Err(format!(
            "hash of downloaded file ({sha1}) did not match requested ({hash})"
        ));
    }
    let valid = match validate_pack(&body) {
        Ok(()) => true,
        Err(error) => {
            warn!(%error, "downloaded resource pack would fail vanilla's reload");
            false
        }
    };
    if let Some(dir) = &request.cache {
        let marker = dir.join(request.id.to_string()).join(&sha1);
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker, &body);
    }
    Ok(Downloaded {
        sha1,
        size: body.len() as u64,
        valid,
        from_cache: false,
    })
}

fn sha1_hex(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `Pack.readPackMetadata` for a downloaded zip: `FilePackResources` opens
/// the archive and reads the root `pack.mcmeta`; a missing pack section or
/// description makes `loadRequestedPacks` fail the reload. The pack's
/// declared version does not have to match the game: vanilla only labels it
/// incompatible, it still loads.
///
/// This is a best-effort version of that reader. It checks the component
/// shape, feature flags and overlays, but does not compile selectors or NBT
/// paths, does not inspect every object/click/hover payload, and does not load
/// the pack's resource files; see `docs/architecture.md`.
fn validate_pack(body: &[u8]) -> Result<(), String> {
    let mut archive =
        ZipArchive::new(Cursor::new(body)).map_err(|e| format!("not a valid zip: {e}"))?;
    let mut meta = archive
        .by_name("pack.mcmeta")
        .map_err(|e| format!("no pack.mcmeta at the zip root: {e}"))?;
    let mut text = String::new();
    meta.read_to_string(&mut text)
        .map_err(|e| format!("could not read pack.mcmeta: {e}"))?;
    drop(meta);

    let root: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("pack.mcmeta is not JSON: {e}"))?;
    let root = root.as_object().ok_or("pack.mcmeta is not a JSON object")?;
    let pack = root
        .get("pack")
        .and_then(serde_json::Value::as_object)
        .ok_or("pack.mcmeta has no pack object")?;
    let description = pack
        .get("description")
        .ok_or("pack.mcmeta has no pack description")?;
    validate_description(description)?;
    // `readPackMetadata` retries with a description-only codec when the full
    // format codec fails, so bad or missing format fields still load as
    // UNKNOWN. Keep the parse for a useful log, but do not reject the pack.
    if let Err(error) = validate_pack_formats(pack) {
        debug!(%error, "pack.mcmeta format declaration failed; vanilla uses its fallback codec");
    }
    // The other metadata sections are read after the pack section, and an
    // error in either one makes readPackMetadata return null for the whole
    // pack rather than falling back.
    validate_features(root)?;
    validate_overlays(root)?;
    Ok(())
}

/// The component shapes `ComponentSerialization.CODEC` accepts. Strings and
/// non-empty lists are complete; object contents are checked against the
/// content codecs and style fields below. The check is deliberately
/// conservative where vanilla would need a command parser (selector and NBT
/// paths).
fn validate_description(value: &serde_json::Value) -> Result<(), String> {
    match value {
        serde_json::Value::String(_) => Ok(()),
        serde_json::Value::Array(items) if !items.is_empty() => {
            for item in items {
                validate_description(item)?;
            }
            Ok(())
        }
        serde_json::Value::Array(_) => {
            Err("pack.mcmeta pack description is an empty component list".into())
        }
        serde_json::Value::Object(object) => validate_component_object(object),
        _ => Err("pack.mcmeta pack description is not a JSON text component".into()),
    }
}

type ComponentValidator = fn(&serde_json::Map<String, serde_json::Value>) -> Result<(), String>;

fn validate_component_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(kind) = object.get("type") {
        let kind = kind
            .as_str()
            .ok_or("pack.mcmeta description type is not a string")?;
        match kind {
            "text" => validate_text(object),
            "translatable" => validate_translatable(object),
            "keybind" => validate_keybind(object),
            "score" => validate_score(object),
            "selector" => validate_selector(object),
            "nbt" => validate_nbt(object),
            "object" => validate_object(object),
            _ => Err(format!("pack.mcmeta description has unknown type {kind}")),
        }?;
    } else {
        // Without a discriminator vanilla tries the content codecs in turn.
        let validators: [ComponentValidator; 7] = [
            validate_text,
            validate_translatable,
            validate_keybind,
            validate_score,
            validate_selector,
            validate_nbt,
            validate_object,
        ];
        if !validators.iter().any(|validate| validate(object).is_ok()) {
            return Err("pack.mcmeta description object has no valid component content".into());
        }
    }
    validate_extra(object)?;
    validate_style(object)
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<&'a str, String> {
    object
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("pack.mcmeta description field {name} is not a string"))
}

fn optional_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<(), String> {
    if let Some(value) = object.get(name)
        && !value.is_string()
    {
        return Err(format!(
            "pack.mcmeta description field {name} is not a string"
        ));
    }
    Ok(())
}

fn validate_text(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    string_field(object, "text").map(|_| ())
}

fn validate_translatable(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    string_field(object, "translate")?;
    optional_string_field(object, "fallback")?;
    if let Some(with) = object.get("with") {
        let with = with
            .as_array()
            .ok_or("pack.mcmeta description translatable `with` is not a list")?;
        for argument in with {
            if argument.is_number() || argument.is_boolean() || argument.is_string() {
                continue;
            }
            validate_description(argument)?;
        }
    }
    Ok(())
}

fn validate_keybind(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    string_field(object, "keybind").map(|_| ())
}

fn validate_score(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let score = object
        .get("score")
        .and_then(serde_json::Value::as_object)
        .ok_or("pack.mcmeta description score is not an object")?;
    string_field(score, "name")?;
    string_field(score, "objective").map(|_| ())
}

fn validate_selector(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    string_field(object, "selector")?;
    if let Some(separator) = object.get("separator") {
        validate_description(separator)?;
    }
    Ok(())
}

fn validate_nbt(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    string_field(object, "nbt")?;
    for name in ["interpret", "plain"] {
        if let Some(value) = object.get(name)
            && !value.is_boolean()
        {
            return Err(format!(
                "pack.mcmeta description NBT field {name} is not a boolean"
            ));
        }
    }
    if object
        .get("interpret")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && object
            .get("plain")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    {
        return Err("pack.mcmeta description NBT flags interpret and plain are both true".into());
    }
    if let Some(separator) = object.get("separator") {
        validate_description(separator)?;
    }
    // DataSources.CODEC has no default; one of its fuzzy fields is required.
    if !["entity", "block", "storage"]
        .iter()
        .any(|name| object.get(*name).is_some_and(serde_json::Value::is_string))
    {
        return Err("pack.mcmeta description NBT component has no data source".into());
    }
    Ok(())
}

fn validate_object(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let kind = string_field(object, "object")?;
    match kind {
        "atlas" => {
            let sprite = string_field(object, "sprite")?;
            if !valid_identifier(sprite) {
                return Err("pack.mcmeta description atlas sprite is not an identifier".into());
            }
            if let Some(atlas) = object.get("atlas") {
                let atlas = atlas
                    .as_str()
                    .ok_or("pack.mcmeta description atlas id is not a string")?;
                if !valid_identifier(atlas) {
                    return Err("pack.mcmeta description atlas id is not an identifier".into());
                }
            }
        }
        "player" => {
            if !matches!(
                object.get("player"),
                Some(serde_json::Value::String(_) | serde_json::Value::Object(_))
            ) {
                return Err("pack.mcmeta description player object has no player profile".into());
            }
            if let Some(hat) = object.get("hat")
                && !hat.is_boolean()
            {
                return Err("pack.mcmeta description player hat is not a boolean".into());
            }
        }
        _ => {
            return Err(format!(
                "pack.mcmeta description has unknown object type {kind}"
            ));
        }
    }
    if let Some(fallback) = object.get("fallback") {
        validate_description(fallback)?;
    }
    Ok(())
}

fn validate_extra(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    if let Some(extra) = object.get("extra") {
        let extra = extra
            .as_array()
            .filter(|items| !items.is_empty())
            .ok_or("pack.mcmeta description extra is not a non-empty list")?;
        for item in extra {
            validate_description(item)?;
        }
    }
    Ok(())
}

fn validate_style(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    for name in [
        "bold",
        "italic",
        "underlined",
        "strikethrough",
        "obfuscated",
    ] {
        if let Some(value) = object.get(name)
            && !value.is_boolean()
        {
            return Err(format!(
                "pack.mcmeta description style field {name} is not a boolean"
            ));
        }
    }
    if let Some(color) = object.get("color") {
        let color = color
            .as_str()
            .ok_or("pack.mcmeta description color is not a string")?;
        if !valid_color(color) {
            return Err(format!("pack.mcmeta description has invalid color {color}"));
        }
    }
    if let Some(shadow) = object.get("shadow_color")
        && !(shadow.is_i64()
            || shadow
                .as_array()
                .is_some_and(|values| values.len() == 4 && values.iter().all(|v| v.is_number())))
    {
        return Err("pack.mcmeta description shadow_color is not an integer or four floats".into());
    }
    optional_string_field(object, "insertion")?;
    if let Some(font) = object.get("font") {
        let font = font
            .as_str()
            .ok_or("pack.mcmeta description font is not a string")?;
        if !valid_identifier(font) {
            return Err("pack.mcmeta description font is not an identifier".into());
        }
    }
    for (name, actions) in [
        (
            "click_event",
            &[
                "open_url",
                "open_file",
                "run_command",
                "suggest_command",
                "show_dialog",
                "change_page",
                "copy_to_clipboard",
                "custom",
            ][..],
        ),
        (
            "hover_event",
            &["show_text", "show_item", "show_entity"][..],
        ),
    ] {
        if let Some(event) = object.get(name) {
            let event = event
                .as_object()
                .ok_or_else(|| format!("pack.mcmeta description {name} is not an object"))?;
            let action = string_field(event, "action")?;
            if !actions.contains(&action) {
                return Err(format!(
                    "pack.mcmeta description {name} has unknown action {action}"
                ));
            }
        }
    }
    Ok(())
}

fn valid_color(color: &str) -> bool {
    const NAMED: [&str; 16] = [
        "black",
        "dark_blue",
        "dark_green",
        "dark_aqua",
        "dark_red",
        "dark_purple",
        "gold",
        "gray",
        "dark_gray",
        "blue",
        "green",
        "aqua",
        "red",
        "light_purple",
        "yellow",
        "white",
    ];
    if let Some(hex) = color.strip_prefix('#') {
        return !hex.is_empty()
            && hex.len() <= 8
            && hex.chars().all(|c| c.is_ascii_hexdigit())
            && u32::from_str_radix(hex, 16).is_ok_and(|value| value <= 0xFF_FF_FF);
    }
    NAMED.contains(&color)
}

fn valid_identifier(value: &str) -> bool {
    let (namespace, path) = match value.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", value),
    };
    !namespace.is_empty()
        && !path.is_empty()
        && namespace
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
        && path.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/')
        })
}

fn valid_overlay_directory(directory: &str) -> bool {
    !directory.is_empty()
        && directory
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// `FeatureFlagsMetadataSection`: the top-level `features.enabled` list.
/// Unknown flags are an error in `FeatureFlagRegistry.codec`, not a warning.
fn validate_features(root: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let Some(features) = root.get("features") else {
        return Ok(());
    };
    let features = features
        .as_object()
        .ok_or("pack.mcmeta features is not an object")?;
    let enabled = features
        .get("enabled")
        .and_then(serde_json::Value::as_array)
        .ok_or("pack.mcmeta features.enabled is not a list")?;
    for flag in enabled {
        let flag = flag
            .as_str()
            .ok_or("pack.mcmeta features.enabled contains a non-string")?;
        let (namespace, path) = flag.split_once(':').unwrap_or(("minecraft", flag));
        let known = namespace == "minecraft"
            && matches!(
                path,
                "vanilla" | "trade_rebalance" | "redstone_experiments" | "minecart_improvements"
            );
        if !valid_identifier(flag) || !known {
            return Err(format!("pack.mcmeta requests unknown feature flag {flag}"));
        }
    }
    Ok(())
}

/// `OverlayMetadataSection`: the top-level `overlays.entries` list. The
/// per-entry format rules are the same as `PackFormat.validateHolderList`,
/// including the `formats` requirement when any entry supports a pre-minor
/// version.
fn validate_overlays(root: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    const LAST_PRE_MINOR: i64 = 64;

    let Some(overlays) = root.get("overlays") else {
        return Ok(());
    };
    let overlays = overlays
        .as_object()
        .ok_or("pack.mcmeta overlays is not an object")?;
    let entries = overlays
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or("pack.mcmeta overlays.entries is not a list")?;

    let mut parsed = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry = entry
            .as_object()
            .ok_or("pack.mcmeta overlays entry is not an object")?;
        let directory = entry
            .get("directory")
            .and_then(serde_json::Value::as_str)
            .ok_or("pack.mcmeta overlays entry has no directory")?;
        if !valid_overlay_directory(directory) {
            return Err(format!(
                "pack.mcmeta overlays entry has invalid directory {directory}"
            ));
        }
        let min = entry
            .get("min_format")
            .map(|value| pack_version(value, 0))
            .transpose()
            .map_err(|e| format!("pack.mcmeta overlays min_format: {e}"))?;
        let max = entry
            .get("max_format")
            .map(|value| pack_version(value, i64::from(i32::MAX)))
            .transpose()
            .map_err(|e| format!("pack.mcmeta overlays max_format: {e}"))?;
        let formats = entry
            .get("formats")
            .map(int_range)
            .transpose()
            .map_err(|e| format!("pack.mcmeta overlays formats: {e}"))?;
        if min.is_some() != max.is_some() {
            return Err(
                "pack.mcmeta overlays entry must declare both min_format and max_format".into(),
            );
        }
        parsed.push((min, max, formats));
    }

    let min_version = parsed
        .iter()
        .filter_map(|(min, _, formats)| match (min, formats) {
            (Some(min), Some((lo, _))) => Some(min.0.min(*lo)),
            (Some(min), None) => Some(min.0),
            (None, Some((lo, _))) => Some(*lo),
            (None, None) => None,
        })
        .min()
        .unwrap_or(i64::from(i32::MAX));
    let require_old_field = min_version <= LAST_PRE_MINOR;

    for (min, max, formats) in parsed {
        if min.is_none() && formats.is_none() {
            // PackFormat.validateHolderList logs and skips such entries.
            continue;
        }
        if let (Some(min), Some(max)) = (min, max) {
            if min > max {
                return Err("pack.mcmeta overlays min_format is greater than max_format".into());
            }
            if min.0 > LAST_PRE_MINOR && !require_old_field {
                if formats.is_some() {
                    return Err(
                        "pack.mcmeta overlays has formats alongside the newer min/max format"
                            .into(),
                    );
                }
            } else {
                let Some((lo, hi)) = formats else {
                    return Err(
                        "pack.mcmeta overlays entry supporting old versions needs formats".into(),
                    );
                };
                if lo != min.0 {
                    return Err("pack.mcmeta overlays formats and min_format disagree".into());
                }
                if hi != max.0 && hi != LAST_PRE_MINOR {
                    return Err("pack.mcmeta overlays formats and max_format disagree".into());
                }
            }
        } else if let Some((_, hi)) = formats
            && hi > LAST_PRE_MINOR
        {
            return Err(
                "pack.mcmeta overlays supports a newer format without min_format and max_format"
                    .into(),
            );
        }
    }
    Ok(())
}

/// The part of `PackFormat.packCodec` that `Pack.readPackMetadata` tries
/// first. Errors here are not fatal: the fallback codec above only reads
/// `description`. `lastPreMinorVersion` for client resources is 64.
fn validate_pack_formats(pack: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    const LAST_PRE_MINOR: i64 = 64;

    let pack_format = pack
        .get("pack_format")
        .map(non_negative_int)
        .transpose()
        .map_err(|e| format!("pack.mcmeta pack_format: {e}"))?;
    let min = pack
        .get("min_format")
        .map(|value| pack_version(value, 0))
        .transpose()
        .map_err(|e| format!("pack.mcmeta min_format: {e}"))?;
    let max = pack
        .get("max_format")
        .map(|value| pack_version(value, i64::from(i32::MAX)))
        .transpose()
        .map_err(|e| format!("pack.mcmeta max_format: {e}"))?;
    let supported = pack
        .get("supported_formats")
        .map(int_range)
        .transpose()
        .map_err(|e| format!("pack.mcmeta supported_formats: {e}"))?;

    if min.is_some() != max.is_some() {
        return Err("pack.mcmeta must declare both min_format and max_format".into());
    }
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            return Err("pack.mcmeta min_format is greater than max_format".into());
        }
        if min.0 > LAST_PRE_MINOR {
            // New-style range: the old supported_formats field is forbidden.
            if supported.is_some() {
                return Err(
                    "pack.mcmeta has supported_formats alongside the newer min/max format".into(),
                );
            }
            if let Some(format) = pack_format {
                check_main_format(format, min.0, max.0)?;
            }
        } else {
            // Old-style range: supported_formats and pack_format are required.
            let Some((lo, hi)) = supported else {
                return Err("pack.mcmeta has min/max format but no supported_formats".into());
            };
            if lo != min.0 {
                return Err("pack.mcmeta supported_formats and min_format disagree".into());
            }
            if hi != max.0 && hi != LAST_PRE_MINOR {
                return Err("pack.mcmeta supported_formats and max_format disagree".into());
            }
            let Some(format) = pack_format else {
                return Err("pack.mcmeta has min/max format but no pack_format".into());
            };
            check_main_format(format, min.0, max.0)?;
        }
    } else if let Some((lo, hi)) = supported {
        if hi > LAST_PRE_MINOR {
            return Err(
                "pack.mcmeta supports a newer format without min_format and max_format".into(),
            );
        }
        let Some(format) = pack_format else {
            return Err("pack.mcmeta has supported_formats but no pack_format".into());
        };
        check_main_format(format, lo, hi)?;
    } else if let Some(format) = pack_format {
        if format > LAST_PRE_MINOR {
            return Err(
                "pack.mcmeta declares a newer pack_format without min_format and max_format".into(),
            );
        }
    } else {
        return Err("pack.mcmeta has no pack format information".into());
    }
    Ok(())
}

fn check_main_format(format: i64, min: i64, max: i64) -> Result<(), String> {
    if !(min..=max).contains(&format) {
        return Err(format!(
            "pack.mcmeta pack_format {format} is outside the declared range {min}..={max}"
        ));
    }
    if format < 15 {
        return Err("pack.mcmeta pack_format is below 15".into());
    }
    Ok(())
}

fn non_negative_int(value: &serde_json::Value) -> Result<i64, String> {
    value
        .as_i64()
        .filter(|n| (0..=i64::from(i32::MAX)).contains(n))
        .ok_or_else(|| "expected a non-negative 32-bit integer".into())
}

/// `PackFormat` JSON: an integer, or `[major, minor]`. An integer leaves the
/// minor at the codec's default (0 for `min_format`, `Integer.MAX_VALUE` for
/// `max_format`).
fn pack_version(value: &serde_json::Value, default_minor: i64) -> Result<(i64, i64), String> {
    if let Some(major) = value.as_i64() {
        return if (0..=i64::from(i32::MAX)).contains(&major) {
            Ok((major, default_minor))
        } else {
            Err("expected a non-negative 32-bit integer".into())
        };
    }
    let array = value
        .as_array()
        .filter(|a| (1..=256).contains(&a.len()))
        .ok_or_else(|| "expected an integer or a list of up to 256 integers".to_string())?;
    let major = non_negative_int(&array[0])?;
    let minor = match array.get(1) {
        Some(minor) => non_negative_int(minor)?,
        None => default_minor,
    };
    Ok((major, minor))
}

/// `InclusiveRange<Integer>` JSON: an integer, `[min, max]`, or the object
/// form `{ "min_inclusive": .., "max_inclusive": .. }`.
fn int_range(value: &serde_json::Value) -> Result<(i64, i64), String> {
    if let Some(both) = value.as_i64() {
        return if (0..=i64::from(i32::MAX)).contains(&both) {
            Ok((both, both))
        } else {
            Err("expected a non-negative 32-bit integer".into())
        };
    }
    if let Some(array) = value.as_array() {
        if array.len() != 2 {
            return Err("expected [min, max]".into());
        }
        let min = non_negative_int(&array[0])?;
        let max = non_negative_int(&array[1])?;
        return if min <= max {
            Ok((min, max))
        } else {
            Err("minimum is greater than maximum".into())
        };
    }
    if let Some(object) = value.as_object() {
        let min = object
            .get("min_inclusive")
            .ok_or_else(|| "missing min_inclusive".to_string())
            .and_then(non_negative_int)?;
        let max = object
            .get("max_inclusive")
            .ok_or_else(|| "missing max_inclusive".to_string())
            .and_then(non_negative_int)?;
        return if min <= max {
            Ok((min, max))
        } else {
            Err("minimum is greater than maximum".into())
        };
    }
    Err("expected an integer, [min, max] or an object".into())
}

/// The request line and headers `HttpURLConnection` writes. `Map.of`
/// iterates the six custom headers in an order that differs from one JVM
/// run to the next; Java's own three always follow.
fn request_bytes(
    url: &Url,
    username: &str,
    profile: Uuid,
    shuffle: &mut impl rand::Rng,
) -> Vec<u8> {
    let mut custom = [
        ("X-Minecraft-Username", username.to_owned()),
        // UndashedUuid.toString
        ("X-Minecraft-UUID", profile.simple().to_string()),
        (
            "X-Minecraft-Version",
            rapidbot_protocol::VERSION_NAME.to_owned(),
        ),
        (
            "X-Minecraft-Version-ID",
            rapidbot_protocol::VERSION_ID.to_owned(),
        ),
        (
            "X-Minecraft-Pack-Format",
            rapidbot_protocol::RESOURCE_PACK_FORMAT.to_owned(),
        ),
        (
            "User-Agent",
            format!("Minecraft Java/{}", rapidbot_protocol::VERSION_NAME),
        ),
    ];
    custom.shuffle(shuffle);
    let file = &url[url::Position::BeforePath..url::Position::AfterQuery];
    let mut out = format!(
        "GET {} HTTP/1.1\r\n",
        if file.is_empty() { "/" } else { file }
    );
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
        if matches!(response.status, 300..=303 | 305 | 307 | 308)
            && let Some(next) = response.location.as_deref().and_then(|l| url.join(l).ok())
            && next.scheme() == url.scheme()
        {
            url = next;
            continue;
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
    let host = url
        .host_str()
        .ok_or("no host in URL")?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = url.port_or_known_default().ok_or("no port")?;
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("connect: {e}"))?;
    if url.scheme() == "https" {
        use tokio_rustls::rustls;
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let name =
            rustls::pki_types::ServerName::try_from(host).map_err(|e| format!("tls name: {e}"))?;
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
async fn exchange(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    request: &[u8],
) -> Result<Response, String> {
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
    let status: u16 = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or("bad status line")?;
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
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
            return Err(format!(
                "filesize is bigger than maximum allowed (file is {length}, limit is {MAX_PACK_SIZE})"
            ));
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
    Ok(Response {
        status,
        location,
        body,
    })
}

async fn read_to_limit(
    stream: &mut (impl AsyncRead + Unpin),
    body: &mut Vec<u8>,
    limit: u64,
) -> Result<(), String> {
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
        let line_end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("bad chunked body")?;
        let size_text = std::str::from_utf8(&data[..line_end]).map_err(|_| "bad chunk size")?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk size")?;
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
    use std::io::Write;

    use tokio::net::TcpListener;

    use super::*;

    const VALID_META: &str =
        r#"{"pack":{"description":"test","min_format":[97,0],"max_format":[97,1]}}"#;

    fn pack_zip(meta: &str) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("pack.mcmeta", options).unwrap();
            zip.write_all(meta.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        out.into_inner()
    }

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
        assert_eq!(
            lines[7..],
            [
                "Host: 127.0.0.1:57441",
                "Accept: */*",
                "Connection: keep-alive",
                "",
                ""
            ]
        );

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
        assert_eq!(
            dechunk(b"4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n").unwrap(),
            b"Wikipedia"
        );
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
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0u8; 4096];
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
                let path = request.split(' ').nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(request);
                let response = match path.as_str() {
                    "/pack.zip" => {
                        let mut r =
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", pack.len())
                                .into_bytes();
                        r.extend_from_slice(&pack);
                        r
                    }
                    "/moved" => {
                        b"HTTP/1.1 302 Found\r\nLocation: /pack.zip\r\nContent-Length: 0\r\n\r\n"
                            .to_vec()
                    }
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
        let pack = pack_zip(VALID_META);
        let sha1 = sha1_hex(&pack);
        let (port, seen) = serve(pack).await;
        let id = Uuid::from_u128(1);

        // Server saved with packs enabled: no prompt, straight to ACCEPTED.
        let mut manager = PackManager::new(
            ResourcePackPolicy::Enabled,
            "Steve".into(),
            profile(),
            None,
            1,
        );
        let turn = manager.push(
            id,
            &format!("http://127.0.0.1:{port}/moved"),
            &sha1.to_uppercase(),
            true,
        );
        assert_eq!(turn.responses, [(id, Action::Accepted)]);
        assert!(!manager.screen_open());
        let rest = settle(&mut manager, 2).await;
        assert_eq!(
            rest,
            [(id, Action::Downloaded), (id, Action::SuccessfullyLoaded)]
        );
        // The redirect was followed with the same headers.
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(
            seen[0].starts_with("GET /moved HTTP/1.1\r\n")
                && seen[1].starts_with("GET /pack.zip HTTP/1.1\r\n")
        );
        assert_eq!(seen[0].replace("/moved", "/pack.zip"), seen[1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prompt_failures_and_declining() {
        let (port, _) = serve(b"not a zip".to_vec()).await;
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));

        // With a prompt: nothing until the click, then both packs at once.
        let mut manager = PackManager::new(
            ResourcePackPolicy::Accept,
            "Steve".into(),
            profile(),
            None,
            1,
        );
        assert!(
            manager
                .push(a, &format!("http://127.0.0.1:{port}/pack.zip"), "", false)
                .responses
                .is_empty()
        );
        assert!(
            manager
                .push(
                    b,
                    &format!("http://127.0.0.1:{port}/missing.zip"),
                    "",
                    false
                )
                .responses
                .is_empty()
        );
        assert!(manager.screen_open());
        let all = settle(&mut manager, 5).await;
        assert!(!manager.screen_open());
        assert_eq!(all[..2], [(a, Action::Accepted), (b, Action::Accepted)]);
        // One loads badly (not a zip), the other was a 404.
        assert!(
            all.contains(&(a, Action::Downloaded)) && all.contains(&(b, Action::FailedDownload))
        );
        assert_eq!(all.last(), Some(&(a, Action::FailedReload)));

        // A wrong hash fails the download.
        let turn = manager.push(
            c,
            &format!("http://127.0.0.1:{port}/pack.zip"),
            &"ab".repeat(20),
            false,
        );
        assert_eq!(
            turn.responses,
            [(c, Action::Accepted)],
            "already allowed: no second prompt"
        );
        assert_eq!(settle(&mut manager, 1).await, [(c, Action::FailedDownload)]);

        // Not a URL the client will fetch.
        assert_eq!(
            manager
                .push(c, "ftp://example.net/pack.zip", "", false)
                .responses,
            [(c, Action::InvalidUrl)]
        );

        // Declining: after the click, and again at once for later packs.
        let mut manager = PackManager::new(
            ResourcePackPolicy::Decline,
            "Steve".into(),
            profile(),
            None,
            1,
        );
        assert!(
            manager
                .push(a, "http://127.0.0.1:1/pack.zip", "", false)
                .responses
                .is_empty()
        );
        assert_eq!(settle(&mut manager, 1).await, [(a, Action::Declined)]);
        assert_eq!(
            manager
                .push(b, "http://127.0.0.1:1/pack.zip", "", false)
                .responses,
            [(b, Action::Declined)]
        );
    }

    #[test]
    fn pack_metadata_validation() {
        assert!(validate_pack(&pack_zip(VALID_META)).is_ok());
        assert!(
            validate_pack(&pack_zip(
                r#"{"pack":{"description":"old","pack_format":64,"supported_formats":[64,64]}}"#
            ))
            .is_ok()
        );
        assert!(
            validate_pack(&pack_zip(
                r#"{"pack":{"description":"new","pack_format":97,"min_format":[97,0],"max_format":[97,1]}}"#
            ))
            .is_ok()
        );
        // An integer max_format leaves the minor at Integer.MAX_VALUE.
        assert!(
            validate_pack(&pack_zip(
                r#"{"pack":{"description":"new","min_format":[97,2],"max_format":97}}"#
            ))
            .is_ok()
        );
        // PackFormat's compact list ignores entries after the minor.
        assert!(
            validate_pack(&pack_zip(
                r#"{"pack":{"description":"new","min_format":[97,0,1],"max_format":[97,1,2]}}"#
            ))
            .is_ok()
        );
        assert!(validate_pack(&pack_zip(r#"{"pack":{"description":5}}"#)).is_err());
        assert!(validate_pack(&pack_zip(r#"{"pack":{"description":[]}}"#)).is_err());
        assert!(validate_pack(&pack_zip(r#"{"pack":{"description":["x",5]}}"#)).is_err());
        // Vanilla's metadata reader needs a description, but it retries with
        // a description-only codec when the format codec fails. A missing or
        // malformed format declaration therefore still loads (as UNKNOWN).
        assert!(validate_pack(&pack_zip(r#"{"pack":{"description":"x"}}"#)).is_ok());
        assert!(
            validate_pack(&pack_zip(
                r#"{"pack":{"description":"x","pack_format":97}}"#
            ))
            .is_ok()
        );
        assert!(validate_pack(&pack_zip(r#"{"pack":{"pack_format":97}}"#)).is_err());
        assert!(validate_pack(b"not a zip").is_err());
        // The strict parser is still useful for logging why vanilla fell back.
        let pack = serde_json::from_str::<serde_json::Value>(
            r#"{"pack":{"description":"x","pack_format":97}}"#,
        )
        .unwrap();
        assert!(validate_pack_formats(pack["pack"].as_object().unwrap()).is_err());
    }

    #[test]
    fn description_component_validation() {
        for description in [
            r##"{"text":"hello","color":"#abcdef","bold":true,"extra":[{"translate":"key","with":["x",1,true]}]}"##,
            r#"["a",{"text":"b"}]"#,
            r#"{"type":"selector","selector":"@e","separator":";"}"#,
            r#"{"nbt":"foo","entity":"@e","interpret":true}"#,
            r#"{"object":"atlas","sprite":"block/stone"}"#,
        ] {
            let meta = format!(r#"{{"pack":{{"description":{description}}}}}"#);
            assert!(
                validate_pack(&pack_zip(&meta)).is_ok(),
                "should accept {description}"
            );
        }
        for description in [
            "{}",
            r#"{"color":"red"}"#,
            r#"{"text":5}"#,
            r#"{"text":"x","extra":[]}"#,
            r#"{"text":"x","bold":"yes"}"#,
            r#"{"type":"nope","text":"x"}"#,
            r#"{"nbt":"foo"}"#,
            r#"{"object":"atlas"}"#,
            r#"{"text":"x","click_event":{"action":"nope"}}"#,
        ] {
            let meta = format!(r#"{{"pack":{{"description":{description}}}}}"#);
            assert!(
                validate_pack(&pack_zip(&meta)).is_err(),
                "should reject {description}"
            );
        }
    }

    #[test]
    fn feature_flags_are_validated() {
        for features in [
            r#"{"enabled":[]}"#,
            r#"{"enabled":["minecraft:vanilla","trade_rebalance"]}"#,
        ] {
            let meta = format!(r#"{{"pack":{{"description":"x"}},"features":{features}}}"#);
            assert!(
                validate_pack(&pack_zip(&meta)).is_ok(),
                "should accept features {features}"
            );
        }
        for features in [
            r#"[]"#,
            r#"{"enabled":"vanilla"}"#,
            r#"{"enabled":[1]}"#,
            r#"{"enabled":["minecraft:nope"]}"#,
        ] {
            let meta = format!(r#"{{"pack":{{"description":"x"}},"features":{features}}}"#);
            assert!(
                validate_pack(&pack_zip(&meta)).is_err(),
                "should reject features {features}"
            );
        }
    }

    #[test]
    fn overlay_metadata_is_validated() {
        for overlays in [
            r#"{"entries":[]}"#,
            r#"{"entries":[{"directory":"overlay","min_format":97,"max_format":97}]}"#,
            r#"{"entries":[{"directory":"overlay","formats":[64,64]}]}"#,
            r#"{"entries":[{"directory":"no-format"}]}"#,
            r#"{"entries":[{"directory":"old","formats":[64,64]},{"directory":"new","min_format":97,"max_format":97,"formats":[97,97]}]}"#,
        ] {
            let meta = format!(r#"{{"pack":{{"description":"x"}},"overlays":{overlays}}}"#);
            assert!(
                validate_pack(&pack_zip(&meta)).is_ok(),
                "should accept overlays {overlays}"
            );
        }
        for overlays in [
            r#"[]"#,
            r#"{}"#,
            r#"{"entries":[{}]}"#,
            r#"{"entries":[{"directory":"bad/name"}]}"#,
            r#"{"entries":[{"directory":"x","min_format":97}]}"#,
            r#"{"entries":[{"directory":"x","min_format":98,"max_format":97}]}"#,
            r#"{"entries":[{"directory":"x","min_format":64,"max_format":64}]}"#,
            r#"{"entries":[{"directory":"x","min_format":97,"max_format":97,"formats":[97,97]}]}"#,
            r#"{"entries":[{"directory":"x","formats":[65,65]}]}"#,
        ] {
            let meta = format!(r#"{{"pack":{{"description":"x"}},"overlays":{overlays}}}"#);
            assert!(
                validate_pack(&pack_zip(&meta)).is_err(),
                "should reject overlays {overlays}"
            );
        }
    }

    #[test]
    fn a_bad_pack_fails_the_whole_metadata_batch() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut manager = PackManager::new(
            ResourcePackPolicy::Enabled,
            "Steve".into(),
            profile(),
            None,
            1,
        );
        for (id, valid) in [(a, true), (b, false)] {
            manager.packs.push(Pack {
                id,
                url: Url::parse("http://127.0.0.1:1/pack.zip").unwrap(),
                hash: None,
                download: Download::Done { valid },
                reported: true,
                settled: false,
            });
        }
        assert!(manager.poll(false).responses.is_empty());
        // Skip the varying reload delay.
        manager.reload = manager.reload.take().map(|(_, ids)| (Instant::now(), ids));
        assert_eq!(
            manager.poll(false).responses,
            [(a, Action::FailedReload), (b, Action::FailedReload)]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cached_pack_is_reused_and_validated() {
        let pack = pack_zip(VALID_META);
        let sha1 = sha1_hex(&pack);
        let (port, _) = serve(pack.clone()).await;
        let dir = std::env::temp_dir().join(format!("rapidbot-pack-cache-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let id = Uuid::from_u128(7);
        let request = || Request {
            id,
            url: Url::parse(&format!("http://127.0.0.1:{port}/pack.zip")).unwrap(),
            hash: Some(sha1.clone()),
            username: "Steve".into(),
            profile: profile(),
            cache: Some(dir.clone()),
        };

        let first = download(request()).await.unwrap();
        assert!(!first.from_cache && first.valid);
        let cached = dir.join(id.to_string()).join(&sha1);
        assert_eq!(std::fs::read(&cached).unwrap(), pack);

        let second = download(request()).await.unwrap();
        assert!(second.from_cache && second.valid);

        // A corrupt cached file is not trusted; it is downloaded again.
        std::fs::write(&cached, b"broken").unwrap();
        let third = download(request()).await.unwrap();
        assert!(!third.from_cache && third.valid);

        // A cache entry that matches the requested hash is authoritative even
        // when the pack is invalid: vanilla reuses it and reports FailedReload
        // instead of downloading it every time.
        let invalid = b"not a zip".to_vec();
        let invalid_id = Uuid::from_u128(8);
        let invalid_sha = sha1_hex(&invalid);
        let invalid_path = dir.join(invalid_id.to_string()).join(&invalid_sha);
        std::fs::create_dir_all(invalid_path.parent().unwrap()).unwrap();
        std::fs::write(&invalid_path, &invalid).unwrap();
        let invalid = download(Request {
            id: invalid_id,
            url: Url::parse("http://127.0.0.1:9/pack.zip").unwrap(),
            hash: Some(invalid_sha),
            username: "Steve".into(),
            profile: profile(),
            cache: Some(dir.clone()),
        })
        .await
        .unwrap();
        assert!(invalid.from_cache);
        assert!(!invalid.valid);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
