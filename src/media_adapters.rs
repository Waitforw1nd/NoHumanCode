//! Root-crate adapters for `src/media/` traits (`WorkspaceMediaArtifactStore`,
//! `CliMediaCredentialSource`, `WhisperCliRunner` / `ProcessLocalAsrRunner`) keeping `src/media/`
//! free of `Store`, `secrets`, `workspace`, `rusqlite`, `tokio::process`, and `windows_sys`.

use crate::media::crypto::sha256_hex;
use crate::media::stt::{
    LOCAL_ASR_FFMPEG_TIMEOUT_MS, LOCAL_ASR_FFPROBE_TIMEOUT_MS, LOCAL_ASR_MAX_OUTPUT_BYTES,
    LOCAL_ASR_TOTAL_TIMEOUT_MS, LocalAsrInput, LocalAsrRequest, LocalAsrRunner, LocalAsrStatus,
    SttTranscript, build_ffmpeg_args, build_ffprobe_args, build_whisper_cli_args,
    demuxer_for_audio_extension, normalize_whisper_language, parse_ffprobe_duration_json,
};
use crate::media::{
    MediaArtifactRef, MediaArtifactStore, MediaAuthKind, MediaCredential, MediaCredentialSource,
    MediaEndpointConfig, MediaError, MediaErrorCode, sanitize_extension,
};
use crate::redaction::validate_persisted_id;
use crate::store::Store;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_MAX_WORKSPACE_ARTIFACT_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct WorkspaceMediaArtifactStore {
    root: PathBuf,
    workspace_root: Option<PathBuf>,
    max_artifact_bytes: usize,
}

impl WorkspaceMediaArtifactStore {
    pub fn new(root: PathBuf, workspace_root: Option<PathBuf>) -> Self {
        Self {
            root,
            workspace_root,
            max_artifact_bytes: DEFAULT_MAX_WORKSPACE_ARTIFACT_BYTES,
        }
    }

    pub fn for_workspace(workspace: &Path) -> Self {
        Self::new(
            workspace.join(".coda").join("artifacts"),
            Some(workspace.to_path_buf()),
        )
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_artifact_bytes = max_bytes;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write_blocking(
        &self,
        owner_id: &str,
        suggested_ext: &str,
        mime_type: &str,
        bytes: &[u8],
    ) -> Result<MediaArtifactRef, MediaError> {
        validate_owner_segment(owner_id)?;
        if bytes.is_empty() {
            return Err(MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                "cannot write empty media artifact",
            ));
        }
        if bytes.len() > self.max_artifact_bytes {
            return Err(MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!(
                    "media artifact size {} exceeds limit {}",
                    bytes.len(),
                    self.max_artifact_bytes
                ),
            ));
        }

        let ext = sanitize_extension(suggested_ext);
        let sha256 = sha256_hex(bytes);

        std::fs::create_dir_all(&self.root).map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to create artifact root: {e}"),
            )
        })?;
        ensure_no_reparse_or_symlink(&self.root)?;

        let owner_dir = self.root.join(owner_id);
        std::fs::create_dir_all(&owner_dir).map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to create artifact owner dir: {e}"),
            )
        })?;
        ensure_no_reparse_or_symlink(&owner_dir)?;

        let canonical_root = self.root.canonicalize().map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to canonicalize artifact root: {e}"),
            )
        })?;
        let canonical_owner = owner_dir.canonicalize().map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to canonicalize artifact owner dir: {e}"),
            )
        })?;
        if canonical_owner == canonical_root || !canonical_owner.starts_with(&canonical_root) {
            return Err(MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                "artifact owner directory escaped root",
            ));
        }

        let filename = format!("{}.{ext}", uuid::Uuid::now_v7());
        let target = canonical_owner.join(&filename);
        let temp = canonical_owner.join(format!(".coda-media-{}.tmp", uuid::Uuid::new_v4()));

        struct TempCleanup(PathBuf);
        impl Drop for TempCleanup {
            fn drop(&mut self) {
                if !self.0.as_os_str().is_empty() {
                    let _ = std::fs::remove_file(&self.0);
                }
            }
        }

        let mut cleanup = TempCleanup(temp.clone());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| {
                MediaError::new(
                    MediaErrorCode::ArtifactWriteFailed,
                    format!("failed to open temp artifact file: {e}"),
                )
            })?;
        file.write_all(bytes).map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to write temp artifact file: {e}"),
            )
        })?;
        file.sync_all().map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to sync temp artifact file: {e}"),
            )
        })?;
        drop(file);

        std::fs::rename(&temp, &target).map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to finalize artifact file: {e}"),
            )
        })?;
        cleanup.0.clear();

        let rel_path = if let Some(ws) = &self.workspace_root {
            if let Ok(canon_ws) = ws.canonicalize()
                && let Ok(rel) = target.strip_prefix(&canon_ws)
            {
                rel.to_string_lossy().replace('\\', "/")
            } else {
                target.to_string_lossy().replace('\\', "/")
            }
        } else if let Ok(rel) = target.strip_prefix(&canonical_root) {
            rel.to_string_lossy().replace('\\', "/")
        } else {
            target.to_string_lossy().replace('\\', "/")
        };

        Ok(MediaArtifactRef {
            path: rel_path,
            mime_type: mime_type.to_owned(),
            size_bytes: bytes.len() as u64,
            sha256,
        })
    }

    pub fn read_blocking(&self, artifact: &MediaArtifactRef) -> Result<bytes::Bytes, MediaError> {
        let raw = artifact.path.trim();
        if raw.is_empty() || raw.bytes().any(|b| b.is_ascii_control()) {
            return Err(MediaError::invalid_request(
                "artifact path must not be empty or contain control characters",
            ));
        }
        let rel_or_abs = Path::new(raw);
        if rel_or_abs
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(MediaError::invalid_request(
                "artifact path must not contain '..' parent traversal",
            ));
        }

        if !self.root.exists() {
            return Err(MediaError::new(
                MediaErrorCode::NotFound,
                "artifact root directory does not exist",
            ));
        }
        ensure_no_reparse_or_symlink(&self.root)?;
        let canonical_root = self.root.canonicalize().map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to canonicalize artifact root: {e}"),
            )
        })?;

        let candidate = if rel_or_abs.is_absolute() {
            rel_or_abs.to_path_buf()
        } else if let Some(ws) = &self.workspace_root {
            let ws_joined = ws.join(rel_or_abs);
            if ws_joined.exists() {
                ws_joined
            } else {
                self.root.join(rel_or_abs)
            }
        } else {
            self.root.join(rel_or_abs)
        };

        if let Some(parent) = candidate.parent()
            && parent.exists()
        {
            ensure_no_reparse_or_symlink(parent)?;
        }
        let meta = match std::fs::symlink_metadata(&candidate) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(MediaError::new(
                    MediaErrorCode::NotFound,
                    format!("media artifact not found: {}", artifact.path),
                ));
            }
            Err(e) => {
                return Err(MediaError::new(
                    MediaErrorCode::ArtifactWriteFailed,
                    format!("failed to stat media artifact: {e}"),
                ));
            }
        };
        if meta.file_type().is_symlink() {
            return Err(MediaError::invalid_request(
                "media artifact must not be a symlink",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(MediaError::invalid_request(
                    "media artifact must not be a Windows reparse point",
                ));
            }
        }

        let canonical_target = candidate.canonicalize().map_err(|e| {
            MediaError::new(
                MediaErrorCode::NotFound,
                format!("failed to canonicalize media artifact path: {e}"),
            )
        })?;
        if canonical_target == canonical_root || !canonical_target.starts_with(&canonical_root) {
            return Err(MediaError::invalid_request(
                "media artifact path escapes artifact root",
            ));
        }
        if !meta.is_file() {
            return Err(MediaError::invalid_request(
                "media artifact path is not a regular file",
            ));
        }
        if meta.len() > self.max_artifact_bytes as u64 {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!(
                    "media artifact size {} exceeds read limit {}",
                    meta.len(),
                    self.max_artifact_bytes
                ),
            ));
        }

        let data = std::fs::read(&canonical_target).map_err(|e| {
            MediaError::new(
                MediaErrorCode::NotFound,
                format!("failed to read media artifact: {e}"),
            )
        })?;
        if !artifact.sha256.is_empty() && sha256_hex(&data) != artifact.sha256 {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "media artifact sha256 verification failed",
            ));
        }
        Ok(bytes::Bytes::from(data))
    }
}

impl MediaArtifactStore for WorkspaceMediaArtifactStore {
    async fn write_artifact<'a>(
        &'a self,
        owner_id: &'a str,
        suggested_ext: &'a str,
        mime_type: &'a str,
        bytes: &'a [u8],
    ) -> Result<MediaArtifactRef, MediaError> {
        let store = self.clone();
        let owner = owner_id.to_owned();
        let ext = suggested_ext.to_owned();
        let mime = mime_type.to_owned();
        let data = bytes.to_vec();
        tokio::task::spawn_blocking(move || store.write_blocking(&owner, &ext, &mime, &data))
            .await
            .map_err(|e| {
                MediaError::new(
                    MediaErrorCode::ArtifactWriteFailed,
                    format!("artifact write task panicked: {e}"),
                )
            })?
    }

    async fn read_artifact<'a>(
        &'a self,
        artifact: &'a MediaArtifactRef,
    ) -> Result<bytes::Bytes, MediaError> {
        let store = self.clone();
        let artifact = artifact.clone();
        tokio::task::spawn_blocking(move || store.read_blocking(&artifact))
            .await
            .map_err(|e| {
                MediaError::new(
                    MediaErrorCode::NotFound,
                    format!("artifact read task panicked: {e}"),
                )
            })?
    }
}

fn validate_owner_segment(owner_id: &str) -> Result<(), MediaError> {
    validate_persisted_id("owner_id", owner_id)
        .map_err(|e| MediaError::invalid_request(e.to_string()))?;
    if owner_id == "." || owner_id.ends_with('.') {
        return Err(MediaError::invalid_request(
            "owner_id must not be '.' or end with '.'",
        ));
    }
    if owner_id.contains(['/', '\\', ':']) || owner_id.contains("..") {
        return Err(MediaError::invalid_request(
            "owner_id contains forbidden path characters",
        ));
    }
    let upper = owner_id.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or(&upper);
    if matches!(
        stem,
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        return Err(MediaError::invalid_request(
            "owner_id uses a reserved Windows device name",
        ));
    }
    Ok(())
}

fn ensure_no_reparse_or_symlink(path: &Path) -> Result<(), MediaError> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        MediaError::new(
            MediaErrorCode::ArtifactWriteFailed,
            format!("failed to stat artifact directory: {e}"),
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(MediaError::new(
            MediaErrorCode::ArtifactWriteFailed,
            "artifact directory must not be a symlink",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                "artifact directory must not be a Windows reparse point",
            ));
        }
    }
    Ok(())
}

pub struct CliMediaCredentialSource {
    store: Option<Arc<Store>>,
}

impl CliMediaCredentialSource {
    pub fn env_only() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: Arc<Store>) -> Self {
        Self { store: Some(store) }
    }

    pub fn auth_kind_for_vendor(vendor: &str) -> MediaAuthKind {
        match vendor.trim().to_ascii_lowercase().as_str() {
            "gemini" | "google" | "imagen" | "veo" | "lyria" | "lyria3" | "lyria-realtime"
            | "gemini-live" => MediaAuthKind::XGoogApiKey,
            "elevenlabs" => MediaAuthKind::XiApiKey,
            "azure" => MediaAuthKind::AzureSubscriptionKey,
            "volcengine" | "doubao" | "seed-audio" | "volcengine-asr" | "doubao-speech" => {
                MediaAuthKind::VolcengineV3ApiKey
            }
            "volcengine-v1" => MediaAuthKind::VolcengineV1SemicolonBearer,
            "kling" | "kling-intl" | "klingai" => MediaAuthKind::KlingAkSk,
            _ => MediaAuthKind::Bearer,
        }
    }

    pub fn default_base_url_for_vendor(vendor: &str) -> &'static str {
        match vendor.trim().to_ascii_lowercase().as_str() {
            "gemini" | "google" | "imagen" | "veo" | "lyria" | "lyria3" | "lyria-realtime"
            | "gemini-live" => "https://generativelanguage.googleapis.com/v1beta",
            "veo-vertex" | "lyria2" | "lyria-vertex" | "gemini-live-vertex" | "vertex"
            | "google-vertex" => "https://us-central1-aiplatform.googleapis.com/v1",
            "jina" => "https://api.jina.ai",
            "ark" | "seedream" | "seedance" | "volcengine-ark" => {
                "https://ark.cn-beijing.volces.com/api/v3"
            }
            "elevenlabs" => "https://api.elevenlabs.io",
            "azure" => "https://eastus.tts.speech.microsoft.com/cognitiveservices/v1",
            "minimax" | "hailuo" => "https://api.minimax.io",
            "minimax-cn" | "hailuo-cn" => "https://api.minimaxi.com",
            "volcengine" | "doubao" | "seed-audio" | "volcengine-v1" | "volcengine-asr"
            | "doubao-speech" => "https://openspeech.bytedance.com",
            "dashscope" | "qwen" | "cosyvoice" | "paraformer" | "wanx" => {
                "https://dashscope.aliyuncs.com"
            }
            "kling" | "klingai" => "https://api-beijing.klingai.com",
            "kling-intl" => "https://api-singapore.klingai.com",
            "runway" => "https://api.dev.runwayml.com",
            "partner-seed-audio" | "sora" | "suno" | "coda-music-gateway" | "coda-music"
            | "treeapi" | "newapi-video" | "new-api" => "https://api.openai.com/v1",
            _ => "https://api.openai.com/v1",
        }
    }

    pub fn env_key_candidates_for_vendor(vendor: &str) -> &'static [&'static str] {
        match vendor.trim().to_ascii_lowercase().as_str() {
            "gemini" | "google" | "imagen" | "veo" | "lyria" | "lyria3" | "lyria-realtime"
            | "gemini-live" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            "veo-vertex" | "lyria2" | "lyria-vertex" | "gemini-live-vertex" | "vertex"
            | "google-vertex" => &[
                "GOOGLE_OAUTH_ACCESS_TOKEN",
                "VERTEX_ACCESS_TOKEN",
                "GOOGLE_ACCESS_TOKEN",
            ],
            "jina" => &["JINA_API_KEY"],
            "ark" | "seedream" | "seedance" | "volcengine-ark" => {
                &["ARK_API_KEY", "VOLCENGINE_ARK_API_KEY"]
            }
            "elevenlabs" => &["ELEVENLABS_API_KEY"],
            "azure" => &["AZURE_SPEECH_KEY", "AZURE_TTS_KEY"],
            "minimax" | "minimax-cn" | "hailuo" | "hailuo-cn" => &["MINIMAX_API_KEY"],
            "volcengine" | "doubao" | "seed-audio" | "volcengine-v1" | "volcengine-asr"
            | "doubao-speech" => &[
                "VOLCENGINE_SPEECH_API_KEY",
                "BYTEDANCE_SPEECH_API_KEY",
                "VOLCENGINE_API_KEY",
            ],
            "dashscope" | "qwen" | "cosyvoice" | "paraformer" | "wanx" => &["DASHSCOPE_API_KEY"],
            "kling" | "kling-intl" | "klingai" => &["KLING_ACCESS_KEY", "KLING_AK"],
            "runway" => &["RUNWAYML_API_SECRET", "RUNWAY_API_KEY"],
            "suno" | "newapi-video" | "new-api" => &["NEW_API_KEY", "OPENAI_API_KEY"],
            "coda-music-gateway" | "coda-music" | "treeapi" => {
                &["CODA_MUSIC_API_KEY", "TREEAPI_API_KEY", "OPENAI_API_KEY"]
            }
            _ => &["OPENAI_API_KEY"],
        }
    }

    pub fn secondary_env_key_candidates_for_vendor(vendor: &str) -> &'static [&'static str] {
        match vendor.trim().to_ascii_lowercase().as_str() {
            "kling" | "kling-intl" | "klingai" => &["KLING_SECRET_KEY", "KLING_SK"],
            _ => &[],
        }
    }

    pub async fn resolve_media_endpoint_with_overrides(
        &self,
        route_id: Option<&str>,
        vendor: &str,
        model: &str,
        auth_kind_override: Option<MediaAuthKind>,
        app_key_override: Option<&str>,
        secondary_override: Option<String>,
    ) -> Result<(MediaEndpointConfig, MediaCredential), MediaError> {
        let auth_kind = auth_kind_override.unwrap_or_else(|| Self::auth_kind_for_vendor(vendor));
        let clean_secondary_override = secondary_override.filter(|s| !s.trim().is_empty());

        if let Some(route_id) = route_id {
            let store = self.store.as_ref().ok_or_else(|| {
                MediaError::invalid_request("route credential resolution requires an open Store")
            })?;
            let settings = store
                .settings()
                .map_err(|e| MediaError::new(MediaErrorCode::PersistenceFailed, e.to_string()))?
                .ok_or_else(|| {
                    MediaError::new(MediaErrorCode::NotFound, "no host settings configured")
                })?;
            let route = settings
                .routes
                .iter()
                .find(|r| r.id == route_id)
                .ok_or_else(|| {
                    MediaError::new(
                        MediaErrorCode::NotFound,
                        format!("route '{route_id}' not found"),
                    )
                })?;

            let mut secret = route
                .key_env
                .as_deref()
                .and_then(|env_name| std::env::var(env_name).ok())
                .filter(|s| !s.trim().is_empty());

            if secret.is_none() {
                secret = store
                    .secret(&format!("route::{}", route.id))
                    .map_err(|e| MediaError::new(MediaErrorCode::PersistenceFailed, e.to_string()))?
                    .or_else(|| store.secret(&route.id).ok().flatten())
                    .filter(|s| !s.trim().is_empty());
            }

            let key = secret.ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::AuthRejected,
                    format!("missing credential for route '{}'", route.id),
                )
            })?;

            let primary = if auth_kind == MediaAuthKind::VolcengineV3AppAccess {
                app_key_override
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or(key)
            } else {
                key
            };

            let secondary = clean_secondary_override.or_else(|| {
                if auth_kind == MediaAuthKind::KlingAkSk {
                    let from_store = store
                        .secret(&format!("route::{}::secret", route.id))
                        .ok()
                        .flatten()
                        .or_else(|| {
                            store
                                .secret(&format!("route::{}::secondary", route.id))
                                .ok()
                                .flatten()
                        })
                        .filter(|s| !s.trim().is_empty());
                    from_store.or_else(|| {
                        Self::secondary_env_key_candidates_for_vendor(vendor)
                            .iter()
                            .find_map(|env_name| {
                                std::env::var(env_name)
                                    .ok()
                                    .filter(|s| !s.trim().is_empty())
                            })
                    })
                } else {
                    None
                }
            });

            let chosen_model = if model.trim().is_empty() {
                route.model.clone()
            } else {
                model.to_owned()
            };
            let endpoint = MediaEndpointConfig {
                vendor: vendor.to_owned(),
                base_url: route.base_url.clone(),
                model: chosen_model,
                route_id: Some(route.id.clone()),
                timeout_ms: None,
                embedded_proxy: false,
            };
            let cred = MediaCredential::new(auth_kind, primary, secondary)?;
            return Ok((endpoint, cred));
        }

        for env_name in Self::env_key_candidates_for_vendor(vendor) {
            if let Ok(key) = std::env::var(env_name)
                && !key.trim().is_empty()
            {
                let primary = if auth_kind == MediaAuthKind::VolcengineV3AppAccess {
                    app_key_override
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned)
                        .unwrap_or(key)
                } else {
                    key
                };
                let secondary = clean_secondary_override.or_else(|| {
                    if auth_kind == MediaAuthKind::KlingAkSk {
                        Self::secondary_env_key_candidates_for_vendor(vendor)
                            .iter()
                            .find_map(|sec_env| {
                                std::env::var(sec_env).ok().filter(|s| !s.trim().is_empty())
                            })
                    } else {
                        None
                    }
                });
                let endpoint = MediaEndpointConfig::new(
                    vendor,
                    Self::default_base_url_for_vendor(vendor),
                    model,
                );
                let cred = MediaCredential::new(auth_kind, primary, secondary)?;
                return Ok((endpoint, cred));
            }
        }

        Err(MediaError::new(
            MediaErrorCode::AuthRejected,
            format!("no API key found for media vendor '{vendor}'"),
        ))
    }
}

impl MediaCredentialSource for CliMediaCredentialSource {
    async fn resolve_media_endpoint<'a>(
        &'a self,
        route_id: Option<&'a str>,
        vendor: &'a str,
        model: &'a str,
    ) -> Result<(MediaEndpointConfig, MediaCredential), MediaError> {
        self.resolve_media_endpoint_with_overrides(route_id, vendor, model, None, None, None)
            .await
    }
}

// ============================================================================
// Local `whisper.cpp` Subprocess Runner (`WhisperCliRunner` / `ProcessLocalAsrRunner`)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct WhisperCliConfig {
    pub data_dir: Option<PathBuf>,
    pub executable_path: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub ffmpeg_path: Option<PathBuf>,
    pub ffprobe_path: Option<PathBuf>,
    pub threads: Option<usize>,
    pub temp_root: Option<PathBuf>,
}

pub struct WhisperCliRunner {
    config: WhisperCliConfig,
    busy: Arc<TokioMutex<()>>,
}

pub type ProcessLocalAsrRunner = WhisperCliRunner;

impl WhisperCliRunner {
    pub fn new(config: WhisperCliConfig) -> Self {
        Self {
            config,
            busy: Arc::new(TokioMutex::new(())),
        }
    }

    fn asr_dir(&self) -> Option<PathBuf> {
        self.config.data_dir.as_ref().map(|d| d.join("asr"))
    }

    fn discover_whisper_bin(&self) -> String {
        if let Some(p) = &self.config.executable_path
            && !p.as_os_str().is_empty()
        {
            return p.to_string_lossy().into_owned();
        }
        if let Ok(env_bin) = std::env::var("CODA_WHISPER_BIN")
            && !env_bin.trim().is_empty()
        {
            return env_bin.trim().to_owned();
        }
        let program = if cfg!(windows) {
            "whisper-cli.exe"
        } else {
            "whisper-cli"
        };
        if let Some(asr_dir) = self.asr_dir() {
            let installed = asr_dir.join("bin").join(program);
            if installed.exists() {
                return installed.to_string_lossy().into_owned();
            }
        }
        which_program(program).unwrap_or_default()
    }

    fn discover_model_path(&self) -> String {
        if let Some(p) = &self.config.model_path
            && !p.as_os_str().is_empty()
        {
            return p.to_string_lossy().into_owned();
        }
        if let Ok(env_model) = std::env::var("CODA_WHISPER_MODEL")
            && !env_model.trim().is_empty()
        {
            return env_model.trim().to_owned();
        }
        if let Some(asr_dir) = self.asr_dir() {
            return asr_dir
                .join("ggml-small-q5_1.bin")
                .to_string_lossy()
                .into_owned();
        }
        String::new()
    }

    fn discover_ffmpeg_bin(&self) -> String {
        if let Some(p) = &self.config.ffmpeg_path
            && !p.as_os_str().is_empty()
        {
            return p.to_string_lossy().into_owned();
        }
        if let Ok(env_bin) = std::env::var("CODA_FFMPEG_BIN")
            && !env_bin.trim().is_empty()
        {
            return env_bin.trim().to_owned();
        }
        let prog = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        which_program(prog).unwrap_or_default()
    }

    fn discover_ffprobe_bin(&self) -> String {
        if let Some(p) = &self.config.ffprobe_path
            && !p.as_os_str().is_empty()
        {
            return p.to_string_lossy().into_owned();
        }
        if let Ok(env_bin) = std::env::var("CODA_FFPROBE_BIN")
            && !env_bin.trim().is_empty()
        {
            return env_bin.trim().to_owned();
        }
        let prog = if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        };
        which_program(prog).unwrap_or_default()
    }

    fn resolve_status_sync(&self) -> LocalAsrStatus {
        let exe = self.discover_whisper_bin();
        let model = self.discover_model_path();
        let ffmpeg = self.discover_ffmpeg_bin();
        let ffprobe = self.discover_ffprobe_bin();

        let make_err = |reason: String| LocalAsrStatus {
            supported: false,
            executable_path: exe.clone(),
            model_path: model.clone(),
            ffmpeg_path: ffmpeg.clone(),
            ffprobe_path: ffprobe.clone(),
            reason: Some(reason),
        };

        if let Err(e) = validate_executable_file(&exe, "whisper-cli") {
            return make_err(e);
        }
        if let Err(e) = validate_model_file(&model) {
            return make_err(e);
        }
        if let Err(e) = validate_executable_file(&ffmpeg, "ffmpeg") {
            return make_err(e);
        }
        if let Err(e) = validate_executable_file(&ffprobe, "ffprobe") {
            return make_err(e);
        }

        LocalAsrStatus {
            supported: true,
            executable_path: exe,
            model_path: model,
            ffmpeg_path: ffmpeg,
            ffprobe_path: ffprobe,
            reason: None,
        }
    }
}

impl LocalAsrRunner for WhisperCliRunner {
    async fn status<'a>(&'a self, _lang: Option<&'a str>) -> Result<LocalAsrStatus, MediaError> {
        Ok(self.resolve_status_sync())
    }

    async fn transcribe<'a>(
        &'a self,
        req: &'a LocalAsrRequest,
        cancel: &'a CancellationToken,
    ) -> Result<SttTranscript, MediaError> {
        let _busy_guard = self.busy.try_lock().map_err(|_| {
            MediaError::new(
                MediaErrorCode::Conflict,
                "another local audio transcription is already in progress",
            )
        })?;

        let status = self.resolve_status_sync();
        if !status.supported {
            return Err(MediaError::new(
                MediaErrorCode::Conflict,
                status
                    .reason
                    .unwrap_or_else(|| "local whisper.cpp ASR is not configured".into()),
            ));
        }

        let temp_base = self
            .config
            .temp_root
            .clone()
            .or_else(|| self.asr_dir())
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&temp_base).map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to create ASR temp root: {e}"),
            )
        })?;
        let temp_dir = temp_base.join(format!("transcription-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).map_err(|e| {
            MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                format!("failed to create ASR temp workspace: {e}"),
            )
        })?;
        struct TempDirGuard(PathBuf);
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _temp_guard = TempDirGuard(temp_dir.clone());

        let (input_path, ext) = match &req.input {
            LocalAsrInput::FilePath(p) => {
                let p_str = p.to_string_lossy();
                if p_str.trim().is_empty()
                    || p_str.chars().count() > 4096
                    || !p.is_absolute()
                    || p_str.chars().any(|c| c.is_control())
                {
                    return Err(MediaError::invalid_request(
                        "local audio file path must be an absolute path without control chars",
                    ));
                }
                let ext = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| MediaError::invalid_request("audio file missing extension"))?
                    .to_owned();
                (p.clone(), ext)
            }
            LocalAsrInput::Bytes { filename, bytes } => {
                if bytes.is_empty() {
                    return Err(MediaError::invalid_request("audio bytes must not be empty"));
                }
                let ext = filename
                    .rsplit('.')
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| MediaError::invalid_request("audio filename missing extension"))?
                    .to_owned();
                let safe_ext = sanitize_extension(&ext);
                let in_path = temp_dir.join(format!("input.{safe_ext}"));
                std::fs::write(&in_path, bytes).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::ArtifactWriteFailed,
                        format!("failed to stage ASR input file: {e}"),
                    )
                })?;
                (in_path, ext)
            }
        };

        let demuxer = demuxer_for_audio_extension(&ext).ok_or_else(|| {
            MediaError::invalid_request(format!("unsupported audio extension '.{ext}'"))
        })?;
        let overall_deadline = Instant::now() + Duration::from_millis(LOCAL_ASR_TOTAL_TIMEOUT_MS);

        let input_str = input_path.to_string_lossy().into_owned();
        let probe1_args = build_ffprobe_args(demuxer, &input_str);
        let probe1_out = run_bounded_process(
            &status.ffprobe_path,
            &probe1_args,
            remaining_timeout(overall_deadline, LOCAL_ASR_FFPROBE_TIMEOUT_MS)?,
            1024 * 1024,
            cancel,
        )
        .await?;
        let _ = parse_ffprobe_duration_json(&probe1_out)?;

        let wave_path = temp_dir.join("audio.wav");
        let wave_str = wave_path.to_string_lossy().into_owned();
        let ffmpeg_args = build_ffmpeg_args(demuxer, &input_str, &wave_str);
        let _ = run_bounded_process(
            &status.ffmpeg_path,
            &ffmpeg_args,
            remaining_timeout(overall_deadline, LOCAL_ASR_FFMPEG_TIMEOUT_MS)?,
            1024 * 1024,
            cancel,
        )
        .await?;

        // Second ffprobe check on transcoded WAV catches lying container metadata.
        let probe2_args = build_ffprobe_args("wav", &wave_str);
        let probe2_out = run_bounded_process(
            &status.ffprobe_path,
            &probe2_args,
            remaining_timeout(overall_deadline, LOCAL_ASR_FFPROBE_TIMEOUT_MS)?,
            1024 * 1024,
            cancel,
        )
        .await?;
        let duration_secs = parse_ffprobe_duration_json(&probe2_out)?;

        let output_prefix = temp_dir.join("transcript");
        let output_prefix_str = output_prefix.to_string_lossy().into_owned();
        let lang_primary = normalize_whisper_language(req.lang.as_deref());
        let threads = self.config.threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
        });
        let whisper_args = build_whisper_cli_args(
            &status.model_path,
            &wave_str,
            &lang_primary,
            threads,
            &output_prefix_str,
        );
        let _ = run_bounded_process(
            &status.executable_path,
            &whisper_args,
            remaining_timeout(overall_deadline, LOCAL_ASR_TOTAL_TIMEOUT_MS)?,
            LOCAL_ASR_MAX_OUTPUT_BYTES,
            cancel,
        )
        .await?;

        let result_path = temp_dir.join("transcript.txt");
        let meta = std::fs::symlink_metadata(&result_path).map_err(|e| {
            MediaError::new(
                MediaErrorCode::ProviderError,
                format!("whisper-cli did not produce transcript.txt: {e}"),
            )
        })?;
        if meta.len() > LOCAL_ASR_MAX_OUTPUT_BYTES as u64 {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "whisper-cli transcript output exceeded 2 MiB limit",
            ));
        }
        let raw_text = std::fs::read_to_string(&result_path).map_err(|e| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                format!("failed to read whisper-cli transcript.txt: {e}"),
            )
        })?;
        let trimmed = raw_text.trim().to_owned();
        if trimmed.is_empty() {
            return Err(MediaError::invalid_request(
                "no clear speech recognized in audio",
            ));
        }

        let model_name = Path::new(&status.model_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("whisper.cpp")
            .to_owned();

        Ok(SttTranscript {
            vendor: "whisper".into(),
            model: model_name,
            text: trimmed,
            language: Some(lang_primary),
            duration_secs: Some(duration_secs),
            segments: Vec::new(),
            words: Vec::new(),
            raw_artifact: None,
        })
    }
}

fn remaining_timeout(deadline: Instant, cap_ms: u64) -> Result<Duration, MediaError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(MediaError::new(
            MediaErrorCode::Timeout,
            "local ASR transcription exceeded 600s deadline",
        ));
    }
    Ok((deadline - now).min(Duration::from_millis(cap_ms)))
}

fn validate_local_path_syntax(raw: &str, label: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > 4096
        || trimmed.chars().any(|c| c.is_control())
    {
        return Err(format!("{label} path is empty or invalid"));
    }
    if !Path::new(trimmed).is_absolute() {
        return Err(format!("{label} path must be absolute"));
    }
    Ok(())
}

fn validate_executable_file(raw: &str, label: &str) -> Result<(), String> {
    validate_local_path_syntax(raw, label)?;
    let path = Path::new(raw);
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("{label} not accessible: {e}"))?;
    let meta =
        std::fs::symlink_metadata(&resolved).map_err(|e| format!("{label} metadata error: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{label} is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!("{label} is not executable"));
        }
    }
    Ok(())
}

fn validate_model_file(raw: &str) -> Result<(), String> {
    validate_local_path_syntax(raw, "whisper model")?;
    let meta = std::fs::symlink_metadata(Path::new(raw))
        .map_err(|e| format!("whisper model not found: {e}"))?;
    if !meta.is_file() || meta.len() == 0 {
        return Err("whisper model must be a non-empty regular file".into());
    }
    Ok(())
}

fn which_program(program: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

async fn run_bounded_process(
    program: &str,
    args: &[String],
    timeout: Duration,
    max_output_bytes: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, MediaError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("FFREPORT")
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn().map_err(|e| {
        MediaError::new(
            MediaErrorCode::ProviderError,
            format!("failed to spawn {program}: {e}"),
        )
    })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| MediaError::new(MediaErrorCode::ProviderError, "missing child stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| MediaError::new(MediaErrorCode::ProviderError, "missing child stderr"))?;

    let out_task = tokio::spawn(async move { read_limited(&mut stdout, max_output_bytes).await });
    let err_task = tokio::spawn(async move { read_limited(&mut stderr, max_output_bytes).await });

    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(MediaError::new(MediaErrorCode::Cancelled, "local ASR process cancelled"));
        }
        res = tokio::time::timeout(timeout, child.wait()) => {
            match res {
                Ok(Ok(st)) => st,
                Ok(Err(e)) => {
                    return Err(MediaError::new(
                        MediaErrorCode::ProviderError,
                        format!("failed waiting for {program}: {e}"),
                    ));
                }
                Err(_) => {
                    let _ = child.kill().await;
                    return Err(MediaError::new(
                        MediaErrorCode::Timeout,
                        format!("{program} timed out"),
                    ));
                }
            }
        }
    };

    let out_bytes = out_task
        .await
        .map_err(|e| MediaError::new(MediaErrorCode::ProviderError, e.to_string()))??;
    let err_bytes = err_task
        .await
        .map_err(|e| MediaError::new(MediaErrorCode::ProviderError, e.to_string()))??;

    if !status.success() {
        let err_msg = String::from_utf8_lossy(&err_bytes).trim().to_owned();
        let detail = if err_msg.is_empty() {
            format!("{program} exited with status {status}")
        } else {
            format!("{program} failed ({status}): {err_msg}")
        };
        return Err(MediaError::new(MediaErrorCode::ProviderError, detail));
    }

    Ok(out_bytes)
}

async fn read_limited<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> Result<Vec<u8>, MediaError> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader.read(&mut chunk).await.map_err(|e| {
            MediaError::new(
                MediaErrorCode::Transport,
                format!("process read error: {e}"),
            )
        })?;
        if n == 0 {
            break;
        }
        if buf.len().saturating_add(n) > limit {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("subprocess output exceeded {limit} byte limit"),
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workspace_media_artifact_store_writes_atomically_with_sha256_and_rejects_bad_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceMediaArtifactStore::for_workspace(dir.path());

        let err = store
            .write_artifact("CON", "png", "image/png", b"123")
            .await
            .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::InvalidRequest);

        let err2 = store
            .write_artifact("../escape", "png", "image/png", b"123")
            .await
            .unwrap_err();
        assert_eq!(err2.code, MediaErrorCode::InvalidRequest);

        let err_dot = store
            .write_artifact(".", "png", "image/png", b"123")
            .await
            .unwrap_err();
        assert_eq!(err_dot.code, MediaErrorCode::InvalidRequest);

        let err_trailing_dot = store
            .write_artifact("foo.", "png", "image/png", b"123")
            .await
            .unwrap_err();
        assert_eq!(err_trailing_dot.code, MediaErrorCode::InvalidRequest);

        // Verify neither rejected call created anything directly in .coda/artifacts/
        if store.root().exists() {
            assert_eq!(std::fs::read_dir(store.root()).unwrap().count(), 0);
        }

        let payload = b"\x89PNG\r\n\x1a\nhello";
        let artifact = store
            .write_artifact("task-1", ".PNG", "image/png", payload)
            .await
            .unwrap();
        assert!(artifact.path.starts_with(".coda/artifacts/task-1/"));
        assert!(artifact.path.ends_with(".png"));
        assert_eq!(artifact.size_bytes, payload.len() as u64);
        assert_eq!(artifact.sha256, sha256_hex(payload));
        let disk_bytes = std::fs::read(dir.path().join(&artifact.path)).unwrap();
        assert_eq!(disk_bytes, payload);

        // Read back via MediaArtifactStore::read_artifact
        let read_back = store.read_artifact(&artifact).await.unwrap();
        assert_eq!(read_back.as_ref(), payload);

        // Traversal / out-of-root / missing / sha256 mismatch rejections
        let outside_file = dir.path().join("secret.txt");
        std::fs::write(&outside_file, b"SECRET").unwrap();
        let escape_err = store
            .read_artifact(&MediaArtifactRef {
                path: "../secret.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 6,
                sha256: String::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(escape_err.code, MediaErrorCode::InvalidRequest);

        let outside_ws_err = store
            .read_artifact(&MediaArtifactRef {
                path: "secret.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 6,
                sha256: String::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(outside_ws_err.code, MediaErrorCode::InvalidRequest);

        let missing_err = store
            .read_artifact(&MediaArtifactRef {
                path: ".coda/artifacts/task-1/does-not-exist.png".into(),
                mime_type: "image/png".into(),
                size_bytes: 1,
                sha256: String::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(missing_err.code, MediaErrorCode::NotFound);

        let mut bad_sha = artifact.clone();
        bad_sha.sha256 = "0".repeat(64);
        let sha_err = store.read_artifact(&bad_sha).await.unwrap_err();
        assert_eq!(sha_err.code, MediaErrorCode::DecodeError);
    }

    #[tokio::test]
    async fn cli_media_credential_source_resolves_route_from_store_and_dpapi_secret() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("media-route.sqlite")).unwrap());
        let settings = crate::domain::Settings {
            workspace: dir.path().to_string_lossy().into_owned(),
            max_concurrency: 2,
            routes: vec![crate::domain::Route {
                id: "gem-media".into(),
                name: "Gemini Media".into(),
                base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
                model: "gemini-2.5-flash-image".into(),
                max_tokens: 4096,
                parallel_limit: 1,
                key_env: None,
                protocol: None,
            }],
            newapi: None,
        };
        store.save_settings(&settings).unwrap();
        store
            .put_secret("route::gem-media", "dpapi-route-secret-xyz")
            .unwrap();

        let source = CliMediaCredentialSource::with_store(store);
        let (endpoint, cred) = source
            .resolve_media_endpoint(Some("gem-media"), "gemini", "")
            .await
            .unwrap();
        assert_eq!(
            endpoint.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(endpoint.model, "gemini-2.5-flash-image");
        assert_eq!(endpoint.route_id.as_deref(), Some("gem-media"));
        assert_eq!(cred.kind, MediaAuthKind::XGoogApiKey);
        assert_eq!(
            source
                .credential_set(&cred)
                .scrub("key=dpapi-route-secret-xyz"),
            "key=[redacted]"
        );

        let (override_ep, _) = source
            .resolve_media_endpoint(Some("gem-media"), "gemini", "imagen-3.0")
            .await
            .unwrap();
        assert_eq!(override_ep.model, "imagen-3.0");

        let missing = source
            .resolve_media_endpoint(Some("missing-route"), "gemini", "")
            .await
            .unwrap_err();
        assert_eq!(missing.code, MediaErrorCode::NotFound);
    }

    #[tokio::test]
    async fn cli_media_credential_source_resolves_kling_route_with_secondary_override() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("kling-route.sqlite")).unwrap());
        let settings = crate::domain::Settings {
            workspace: dir.path().to_string_lossy().into_owned(),
            max_concurrency: 2,
            routes: vec![crate::domain::Route {
                id: "my-kling".into(),
                name: "Kling Route".into(),
                base_url: "https://api-beijing.klingai.com".into(),
                model: "kling-v1-6".into(),
                max_tokens: 4096,
                parallel_limit: 1,
                key_env: None,
                protocol: None,
            }],
            newapi: None,
        };
        store.save_settings(&settings).unwrap();
        store
            .put_secret("route::my-kling", "route-ak-only")
            .unwrap();

        let source = CliMediaCredentialSource::with_store(store);
        let (endpoint, cred) = source
            .resolve_media_endpoint_with_overrides(
                Some("my-kling"),
                "kling",
                "",
                None,
                None,
                Some("route-sk-override".into()),
            )
            .await
            .unwrap();
        assert_eq!(endpoint.model, "kling-v1-6");
        assert_eq!(cred.kind, MediaAuthKind::KlingAkSk);
        assert_eq!(
            source
                .credential_set(&cred)
                .scrub("ak=route-ak-only sk=route-sk-override"),
            "ak=[redacted] sk=[redacted]"
        );
        let (hdrs, _, _) = cred.materialize_headers(1000).unwrap();
        assert!(
            hdrs.iter()
                .any(|(k, v)| k == "Authorization" && v.starts_with("Bearer eyJ"))
        );
    }

    fn write_mock_exec(dir: &Path, stem: &str, cmd_body: &str, sh_body: &str) -> PathBuf {
        #[cfg(windows)]
        {
            let _ = sh_body;
            let p = dir.join(format!("{stem}.cmd"));
            std::fs::write(&p, format!("@echo off\r\n{cmd_body}\r\n")).unwrap();
            p
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = cmd_body;
            let p = dir.join(stem);
            std::fs::write(&p, format!("#!/bin/sh\n{sh_body}\n")).unwrap();
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&p, perms).unwrap();
            p
        }
    }

    #[tokio::test]
    async fn whisper_cli_runner_transcode_double_probe_ffreport_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let model_path = dir.path().join("ggml-small-q5_1.bin");
        std::fs::write(&model_path, b"MODEL-DATA").unwrap();
        let input_audio = dir.path().join("sample.mp3");
        std::fs::write(&input_audio, b"ID3-AUDIO").unwrap();
        let temp_root = dir.path().join("asr-tmp");

        let ffprobe = write_mock_exec(
            dir.path(),
            "mock_ffprobe",
            "if defined FFREPORT (echo FFREPORT_LEAKED 1>&2 & exit /b 9)\r\necho {\"format\":{\"duration\":\"8.25\"}}",
            "if [ -n \"$FFREPORT\" ]; then echo FFREPORT_LEAKED >&2; exit 9; fi\necho '{\"format\":{\"duration\":\"8.25\"}}'",
        );
        let ffmpeg = write_mock_exec(
            dir.path(),
            "mock_ffmpeg",
            "if defined FFREPORT (exit /b 9)\r\nfor %%A in (%*) do set LAST=%%A\r\necho RIFF-WAV> \"%LAST%\"",
            "if [ -n \"$FFREPORT\" ]; then exit 9; fi\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf 'RIFF-WAV' > \"$last\"",
        );
        let whisper = write_mock_exec(
            dir.path(),
            "mock_whisper",
            ":loop\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"--output-file\" set OUT=%~2\r\nshift\r\ngoto loop\r\n:done\r\necho   hello mock whisper  > \"%OUT%.txt\"",
            "while [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--output-file\" ]; then OUT=\"$2\"; shift 2; continue; fi\n  shift\ndone\nprintf '  hello mock whisper  ' > \"${OUT}.txt\"",
        );

        let runner = WhisperCliRunner::new(WhisperCliConfig {
            executable_path: Some(whisper.clone()),
            model_path: Some(model_path.clone()),
            ffmpeg_path: Some(ffmpeg.clone()),
            ffprobe_path: Some(ffprobe.clone()),
            temp_root: Some(temp_root.clone()),
            ..Default::default()
        });

        let status = runner.status(Some("en-US")).await.unwrap();
        assert!(status.supported);

        let res = crate::media::stt::local_whisper_transcribe(
            &runner,
            &LocalAsrRequest {
                input: LocalAsrInput::FilePath(input_audio.clone()),
                lang: Some("en-US".into()),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(res.vendor, "whisper");
        assert_eq!(res.text, "hello mock whisper");
        assert_eq!(res.language.as_deref(), Some("en"));
        assert_eq!(res.duration_secs, Some(8.25));
        // Temp directory must be cleaned up by Drop guard
        assert_eq!(std::fs::read_dir(&temp_root).unwrap().count(), 0);

        // Lying-metadata test: input probe reports 10s, transcoded WAV probe reports 1250s -> rejected!
        let lying_ffprobe = write_mock_exec(
            dir.path(),
            "mock_lying_ffprobe",
            "echo %* | findstr /C:\"-f wav\" >nul\r\nif errorlevel 1 (echo {\"format\":{\"duration\":\"10.0\"}}) else (echo {\"format\":{\"duration\":\"1250.0\"}})",
            "case \"$*\" in *-f\\ wav*) echo '{\"format\":{\"duration\":\"1250.0\"}}';; *) echo '{\"format\":{\"duration\":\"10.0\"}}';; esac",
        );
        let lying_runner = WhisperCliRunner::new(WhisperCliConfig {
            executable_path: Some(whisper),
            model_path: Some(model_path),
            ffmpeg_path: Some(ffmpeg),
            ffprobe_path: Some(lying_ffprobe),
            temp_root: Some(temp_root.clone()),
            ..Default::default()
        });
        let err = crate::media::stt::local_whisper_transcribe(
            &lying_runner,
            &LocalAsrRequest {
                input: LocalAsrInput::FilePath(input_audio),
                lang: None,
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::InvalidRequest);
        // Temp dir also cleaned up on error
        assert_eq!(std::fs::read_dir(&temp_root).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn whisper_cli_runner_rejects_concurrent_busy_calls() {
        let dir = tempfile::tempdir().unwrap();
        let runner = WhisperCliRunner::new(WhisperCliConfig {
            temp_root: Some(dir.path().to_path_buf()),
            ..Default::default()
        });
        let _held = runner.busy.lock().await;
        let err = runner
            .transcribe(
                &LocalAsrRequest {
                    input: LocalAsrInput::Bytes {
                        filename: "a.wav".into(),
                        bytes: bytes::Bytes::from_static(b"RIFF"),
                    },
                    lang: None,
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::Conflict);
    }
}
