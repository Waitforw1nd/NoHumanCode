//! Local workbench CLI subcommand (`coda media {embed, rerank, image, tts, stt, music, video, realtime, task}`).

use super::{Failure, HostSecret, host_route_id_valid, resolve_user_data_dir};
use clap::{Args, Subcommand, ValueEnum};
use coda::media::engine2::MediaHttpClient;
use coda::media::engine3::{Engine3Adapter, Engine3Runner, Engine3SubmitInput};
use coda::media::images::{
    ArkSeedreamRequest, GeminiImageRequest, ImageInputBytes, OpenAiImageRequest,
    ark_seedream_generate, gemini_generate_image, imagen_predict, openai_image,
};
use coda::media::music::{
    CodaMusicGatewayAdapter, CodaMusicGatewayMode, CodaMusicGatewayRequest, ElevenLabsMusicMode,
    ElevenLabsMusicPlanRequest, ElevenLabsMusicRequest, Lyria2Request, Lyria3Request,
    LyriaRealTimeRequest, LyriaWeightedPrompt, MiniMaxMusicFormat, MiniMaxMusicRequest, SunoAction,
    SunoMusicAdapter, SunoMusicRequest, build_coda_music_payload, build_suno_submit_payload,
    coda_music_gateway_run, elevenlabs_music, elevenlabs_music_plan, lyria_realtime_ws,
    lyria2_predict, lyria3_generate, minimax_music, suno_music_submit_and_wait,
};
use coda::media::realtime::{RealtimeTurnRequest, gemini_live_turn, openai_realtime_turn};
use coda::media::stt::{
    DashScopeParaformerAdapter, DashScopeSttRequest, ElevenLabsSttRequest, GeminiSttRequest,
    LocalAsrInput, LocalAsrRequest, OpenAiSttGranularity, OpenAiSttRequest, SttAudioSource,
    VolcengineAsrAdapter, VolcengineAsrRequest, build_paraformer_submit_payload,
    build_volcengine_asr_payload, dashscope_asr_ws, dashscope_paraformer_transcribe,
    dashscope_qwen_asr, elevenlabs_stt, gemini_stt, local_whisper_transcribe, openai_stt,
    volcengine_asr_flash, volcengine_asr_transcribe, volcengine_sauc_ws,
};
use coda::media::tts::{
    AzureTtsRequest, DashScopeTtsRequest, ElevenLabsTtsRequest, GeminiSpeakerVoice,
    GeminiSpeechVoice, GeminiTtsRequest, MiniMaxAsyncTtsAdapter, MiniMaxTtsRequest,
    OpenAiTtsRequest, OpenAiTtsStreamFormat, PartnerSeedAudioAdapter, PartnerSeedAudioRequest,
    TtsAudioFormat, VolcengineTtsRequest, VolcengineV3AsyncTtsAdapter, azure_tts,
    azure_tts_regional_base, build_minimax_payload, build_partner_seed_audio_payload,
    build_volcengine_v3_tts_body, dashscope_cosyvoice_ws, dashscope_qwen_tts,
    dashscope_speech_synthesizer_http, elevenlabs_tts, gemini_tts, minimax_regional_base,
    minimax_tts, minimax_tts_async, minimax_tts_ws, openai_tts, partner_seed_audio_tts,
    volcengine_seed_audio_create, volcengine_v1_tts_http, volcengine_v1_tts_ws,
    volcengine_v3_tts_async, volcengine_v3_tts_http, volcengine_v3_tts_ws,
};
use coda::media::vectors::{
    EmbeddingEncodingFormat, EmbeddingInput, GeminiEmbedRequest, JinaRerankRequest,
    OpenAiEmbedRequest, gemini_embed, jina_rerank, openai_embed,
};
use coda::media::video::{
    ArkContentMedia, ArkContentRole, ArkSeedanceAdapter, ArkSeedanceRequest, DashScopeWanxAdapter,
    DashScopeWanxRequest, KlingDuration, KlingVideoAdapter, KlingVideoMode, KlingVideoRequest,
    MiniMaxVideoAdapter, MiniMaxVideoRequest, NewApiVideoAdapter, NewApiVideoPathMode,
    NewApiVideoRequest, RunwayVideoAdapter, RunwayVideoMode, RunwayVideoRequest,
    SoraContentVariant, SoraInputReference, SoraRemixRequest, SoraSeconds, SoraVideoAdapter,
    SoraVideoRequest, VeoMediaInput, VeoPlatform, VeoVideoAdapter, VeoVideoRequest,
    ark_seedance_generate, build_ark_seedance_payload, build_dashscope_wanx_payload,
    build_kling_video_payload, build_minimax_video_payload, build_newapi_video_payload,
    build_runway_video_payload, build_sora_submit_payload, build_veo_submit_payload,
    dashscope_wanx_generate, kling_video_generate, minimax_video_generate, newapi_video_generate,
    runway_video_generate, sora_generate_video, sora_remix_video, veo_generate_video,
};
use coda::media::{
    MediaAuthKind, MediaCredential, MediaEndpointConfig, MediaError, MediaKind, MediaTaskRecord,
    MediaTaskRepository,
};
use coda::media_adapters::{
    CliMediaCredentialSource, WhisperCliConfig, WhisperCliRunner, WorkspaceMediaArtifactStore,
};
use coda::store::{SqliteMediaTaskRepository, Store};
use serde_json::json;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Args)]
pub struct MediaArgs {
    #[command(subcommand)]
    pub command: MediaCommand,
}

#[derive(Subcommand)]
pub enum MediaCommand {
    /// Generate vector embeddings (OpenAI / Gemini).
    Embed(Box<EmbedArgs>),
    /// Rerank documents against a query (Jina Rerank).
    Rerank(Box<RerankArgs>),
    /// Generate or edit images (OpenAI / Gemini / Imagen / Volcengine Ark Seedream).
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
    /// Synthesize speech audio (D.4: 8 vendors, Engine 2/3/4).
    Tts(Box<TtsArgs>),
    /// Transcribe or translate audio (D.5: 6 vendors incl. local whisper.cpp).
    Stt(Box<SttArgs>),
    /// Generate music or plans (D.6: MiniMax / ElevenLabs / Lyria / Suno / Coda Gateway).
    Music(Box<MusicArgs>),
    /// Generate or remix videos (D.7: Sora / Veo / Seedance / MiniMax / Kling / Wanx / Runway / New API).
    Video {
        #[command(subcommand)]
        command: VideoCommand,
    },
    /// Run a realtime voice/multimodal WebSocket turn (D.8: OpenAI Realtime GA / Gemini Live).
    Realtime(Box<RealtimeArgs>),
    /// Inspect, poll, resume, or cancel durable Engine 3 media tasks.
    Task {
        #[command(subcommand)]
        command: MediaTaskCommand,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum EmbedVendor {
    Openai,
    Gemini,
}

impl EmbedVendor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Gemini => "gemini",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum EmbedFormat {
    Float,
    Base64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ImageVendor {
    Openai,
    Gemini,
    Imagen,
    Ark,
}

impl ImageVendor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Gemini => "gemini",
            Self::Imagen => "imagen",
            Self::Ark => "ark",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TtsVendor {
    Openai,
    Gemini,
    Elevenlabs,
    Azure,
    Minimax,
    MinimaxCn,
    Volcengine,
    Dashscope,
    PartnerSeedAudio,
}

impl TtsVendor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Gemini => "gemini",
            Self::Elevenlabs => "elevenlabs",
            Self::Azure => "azure",
            Self::Minimax => "minimax",
            Self::MinimaxCn => "minimax-cn",
            Self::Volcengine => "volcengine",
            Self::Dashscope => "dashscope",
            Self::PartnerSeedAudio => "partner-seed-audio",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TtsTransport {
    Auto,
    Http,
    Sse,
    Async,
    Ws,
    V1Http,
    V1Ws,
    SeedAudio,
    Bailian,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SttVendor {
    Openai,
    Elevenlabs,
    Gemini,
    Dashscope,
    Volcengine,
    Whisper,
}

impl SttVendor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Elevenlabs => "elevenlabs",
            Self::Gemini => "gemini",
            Self::Dashscope => "dashscope",
            Self::Volcengine => "volcengine",
            Self::Whisper => "whisper",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SttTransport {
    Auto,
    Http,
    Async,
    Ws,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MusicVendor {
    Minimax,
    MinimaxCn,
    Elevenlabs,
    Lyria3,
    Lyria2,
    LyriaRealtime,
    Suno,
    CodaGateway,
    PartnerMusic,
}

impl MusicVendor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Minimax => "minimax",
            Self::MinimaxCn => "minimax-cn",
            Self::Elevenlabs => "elevenlabs",
            Self::Lyria3 => "lyria3",
            Self::Lyria2 => "lyria2",
            Self::LyriaRealtime => "lyria-realtime",
            Self::Suno => "suno",
            Self::CodaGateway | Self::PartnerMusic => "coda-music-gateway",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MusicTransport {
    Auto,
    Sync,
    Stream,
    Detailed,
    Plan,
    Ws,
    PostFetch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum VideoVendor {
    Sora,
    Veo,
    VeoVertex,
    Seedance,
    Minimax,
    MinimaxCn,
    Kling,
    KlingIntl,
    Wanx,
    Runway,
    NewapiVideo,
}

impl VideoVendor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sora => "sora",
            Self::Veo => "veo",
            Self::VeoVertex => "veo-vertex",
            Self::Seedance => "seedance",
            Self::Minimax => "minimax",
            Self::MinimaxCn => "minimax-cn",
            Self::Kling => "kling",
            Self::KlingIntl => "kling-intl",
            Self::Wanx => "wanx",
            Self::Runway => "runway",
            Self::NewapiVideo => "newapi-video",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RealtimeVendor {
    Openai,
    GeminiLive,
    GeminiLiveVertex,
}

impl RealtimeVendor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::GeminiLive => "gemini-live",
            Self::GeminiLiveVertex => "gemini-live-vertex",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AuthModeOverride {
    Auto,
    Bearer,
    XApiKey,
    AppAccess,
    V1Semicolon,
    OcpApim,
    XiApiKey,
    XGoogApiKey,
    KlingAkSk,
}

impl AuthModeOverride {
    fn to_media_auth_kind(self, default: MediaAuthKind) -> MediaAuthKind {
        match self {
            Self::Auto => default,
            Self::Bearer => MediaAuthKind::Bearer,
            Self::XApiKey => MediaAuthKind::VolcengineV3ApiKey,
            Self::AppAccess => MediaAuthKind::VolcengineV3AppAccess,
            Self::V1Semicolon => MediaAuthKind::VolcengineV1SemicolonBearer,
            Self::OcpApim => MediaAuthKind::AzureSubscriptionKey,
            Self::XiApiKey => MediaAuthKind::XiApiKey,
            Self::XGoogApiKey => MediaAuthKind::XGoogApiKey,
            Self::KlingAkSk => MediaAuthKind::KlingAkSk,
        }
    }
}

#[derive(Args, Clone)]
pub struct AuthTargetArgs {
    /// Custom provider base URL override.
    #[arg(long)]
    pub base_url: Option<String>,
    /// Configured route ID to resolve base URL & credential from Store.
    #[arg(long)]
    pub route: Option<String>,
    /// Read API key from a single line of stdin.
    #[arg(long)]
    pub api_key_stdin: bool,
    /// Read API key from the specified environment variable.
    #[arg(long)]
    pub api_key_env: Option<String>,
    /// Read secondary secret (e.g. Volcengine Access Key) from env var.
    #[arg(long)]
    pub secondary_key_env: Option<String>,
    /// Non-secret App Key for Volcengine V3 app-access auth.
    #[arg(long)]
    pub app_key: Option<String>,
    /// Override authentication header scheme.
    #[arg(long, value_enum, default_value_t = AuthModeOverride::Auto)]
    pub auth_mode: AuthModeOverride,
    /// Optional Coda data directory when using --route or SQLite tasks.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

impl AuthTargetArgs {
    fn valid(&self) -> bool {
        self.route.as_deref().is_none_or(host_route_id_valid)
            && !(self.api_key_stdin && self.api_key_env.is_some())
            && self
                .api_key_env
                .as_deref()
                .is_none_or(|e| !e.trim().is_empty())
            && self
                .secondary_key_env
                .as_deref()
                .is_none_or(|e| !e.trim().is_empty())
    }
}

#[derive(Args)]
pub struct EmbedArgs {
    #[arg(long, value_enum, default_value_t = EmbedVendor::Openai)]
    pub vendor: EmbedVendor,
    #[arg(long)]
    pub model: String,
    #[arg(long = "input", required = true)]
    pub inputs: Vec<String>,
    #[arg(long)]
    pub dimensions: Option<u32>,
    #[arg(long, value_enum)]
    pub encoding_format: Option<EmbedFormat>,
    #[arg(long)]
    pub task_type: Option<String>,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct RerankArgs {
    #[arg(long, default_value = "jina")]
    pub vendor: String,
    #[arg(long)]
    pub model: String,
    #[arg(long)]
    pub query: String,
    #[arg(long = "doc", required = true)]
    pub documents: Vec<String>,
    #[arg(long)]
    pub top_n: Option<usize>,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub return_documents: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Subcommand)]
pub enum ImageCommand {
    /// Generate image(s) from a text prompt (and optional reference images).
    Generate(Box<ImageGenerateArgs>),
    /// Edit image(s) with optional PNG mask (OpenAI multipart /v1/images/edits).
    Edit(Box<ImageEditArgs>),
}

#[derive(Args)]
pub struct ImageGenerateArgs {
    #[arg(long, value_enum, default_value_t = ImageVendor::Openai)]
    pub vendor: ImageVendor,
    #[arg(long)]
    pub model: String,
    #[arg(long)]
    pub prompt: String,
    #[arg(long)]
    pub n: Option<u32>,
    #[arg(long)]
    pub size: Option<String>,
    #[arg(long)]
    pub quality: Option<String>,
    #[arg(long)]
    pub background: Option<String>,
    #[arg(long)]
    pub output_format: Option<String>,
    #[arg(long)]
    pub response_format: Option<String>,
    #[arg(long)]
    pub aspect_ratio: Option<String>,
    #[arg(long)]
    pub watermark: Option<bool>,
    #[arg(long = "ref-image")]
    pub ref_images: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct ImageEditArgs {
    #[arg(long, value_enum, default_value_t = ImageVendor::Openai)]
    pub vendor: ImageVendor,
    #[arg(long)]
    pub model: String,
    #[arg(long)]
    pub prompt: String,
    #[arg(long = "image", required = true)]
    pub images: Vec<PathBuf>,
    #[arg(long)]
    pub mask: Option<PathBuf>,
    #[arg(long)]
    pub n: Option<u32>,
    #[arg(long)]
    pub size: Option<String>,
    #[arg(long)]
    pub quality: Option<String>,
    #[arg(long)]
    pub background: Option<String>,
    #[arg(long)]
    pub output_format: Option<String>,
    #[arg(long)]
    pub response_format: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct TtsArgs {
    #[arg(long, value_enum, default_value_t = TtsVendor::Openai)]
    pub vendor: TtsVendor,
    #[arg(long, default_value = "")]
    pub model: String,
    #[arg(long, default_value = "")]
    pub text: String,
    #[arg(long)]
    pub ssml: Option<String>,
    #[arg(long)]
    pub ssml_file: Option<PathBuf>,
    #[arg(long)]
    pub voice: Option<String>,
    /// Multi-speaker mapping for Gemini (`speaker=voice_name`), repeatable.
    #[arg(long = "speaker")]
    pub speakers: Vec<String>,
    #[arg(long)]
    pub language: Option<String>,
    #[arg(long)]
    pub instructions: Option<String>,
    #[arg(long)]
    pub response_format: Option<String>,
    #[arg(long)]
    pub speed: Option<f64>,
    #[arg(long)]
    pub pitch: Option<i32>,
    #[arg(long)]
    pub sample_rate: Option<u32>,
    #[arg(long, value_enum, default_value_t = TtsTransport::Auto)]
    pub transport: TtsTransport,
    #[arg(long)]
    pub region: Option<String>,
    #[arg(long)]
    pub resource_id: Option<String>,
    #[arg(long)]
    pub user_agent: Option<String>,
    #[arg(long)]
    pub v1_appid: Option<String>,
    #[arg(long)]
    pub v1_cluster: Option<String>,
    #[arg(long, default_value_t = false)]
    pub no_wait: bool,
    #[arg(long)]
    pub idempotency_key: Option<String>,
    #[arg(long)]
    pub session_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct SttArgs {
    #[arg(long, value_enum, default_value_t = SttVendor::Openai)]
    pub vendor: SttVendor,
    #[arg(long, default_value = "")]
    pub model: String,
    #[arg(long)]
    pub file: Option<PathBuf>,
    #[arg(long)]
    pub audio_url: Option<String>,
    #[arg(long)]
    pub mime_type: Option<String>,
    #[arg(long)]
    pub language: Option<String>,
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub response_format: Option<String>,
    #[arg(long = "timestamp-granularity")]
    pub timestamp_granularities: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub translate: bool,
    #[arg(long, default_value_t = false)]
    pub diarize: bool,
    #[arg(long, default_value_t = false)]
    pub stream: bool,
    #[arg(long, value_enum, default_value_t = SttTransport::Auto)]
    pub transport: SttTransport,
    #[arg(long)]
    pub resource_id: Option<String>,
    #[arg(long)]
    pub whisper_bin: Option<PathBuf>,
    #[arg(long)]
    pub whisper_model: Option<PathBuf>,
    #[arg(long)]
    pub ffmpeg_bin: Option<PathBuf>,
    #[arg(long)]
    pub ffprobe_bin: Option<PathBuf>,
    #[arg(long)]
    pub threads: Option<usize>,
    #[arg(long, default_value_t = false)]
    pub no_wait: bool,
    #[arg(long)]
    pub idempotency_key: Option<String>,
    #[arg(long)]
    pub session_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct MusicArgs {
    #[arg(long, value_enum, default_value_t = MusicVendor::Minimax)]
    pub vendor: MusicVendor,
    #[arg(long, default_value = "")]
    pub model: String,
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub lyrics: Option<String>,
    #[arg(long)]
    pub composition_plan: Option<String>,
    #[arg(long)]
    pub composition_plan_file: Option<PathBuf>,
    #[arg(long)]
    pub music_length_ms: Option<u32>,
    #[arg(long)]
    pub is_instrumental: Option<bool>,
    #[arg(long)]
    pub lyrics_optimizer: Option<bool>,
    #[arg(long)]
    pub sample_rate: Option<u32>,
    #[arg(long)]
    pub bitrate: Option<u32>,
    #[arg(long)]
    pub response_format: Option<String>,
    #[arg(long)]
    pub seed: Option<i64>,
    #[arg(long)]
    pub sample_count: Option<u32>,
    #[arg(long)]
    pub duration_secs: Option<u32>,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub wrap_wav: bool,
    #[arg(long)]
    pub action: Option<String>,
    #[arg(long)]
    pub tags: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub mv: Option<String>,
    #[arg(long)]
    pub clip_id: Option<String>,
    #[arg(long)]
    pub continue_clip_id: Option<String>,
    #[arg(long)]
    pub continue_at: Option<f64>,
    #[arg(long, value_enum, default_value_t = MusicTransport::Auto)]
    pub transport: MusicTransport,
    /// Opt-in flag required for Suno New API proxy (disabled by default).
    #[arg(long, default_value_t = false)]
    pub allow_suno_proxy: bool,
    #[arg(long, default_value_t = false)]
    pub no_wait: bool,
    #[arg(long)]
    pub idempotency_key: Option<String>,
    #[arg(long)]
    pub session_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Subcommand)]
pub enum VideoCommand {
    /// Generate a video from a text prompt and/or reference media.
    Generate(Box<VideoGenerateArgs>),
    /// Remix an existing Sora video (`/v1/videos/{id}/remix`).
    Remix(Box<VideoRemixArgs>),
}

#[derive(Args)]
pub struct VideoGenerateArgs {
    #[arg(long, value_enum, default_value_t = VideoVendor::Sora)]
    pub vendor: VideoVendor,
    #[arg(long, default_value = "")]
    pub model: String,
    #[arg(long, default_value = "")]
    pub prompt: String,
    #[arg(long)]
    pub image: Option<String>,
    #[arg(long)]
    pub first_frame: Option<String>,
    #[arg(long)]
    pub last_frame: Option<String>,
    #[arg(long = "ref-image")]
    pub ref_images: Vec<String>,
    #[arg(long = "ref-video")]
    pub ref_videos: Vec<String>,
    #[arg(long = "ref-audio")]
    pub ref_audios: Vec<String>,
    #[arg(long)]
    pub duration: Option<String>,
    #[arg(long)]
    pub ratio: Option<String>,
    #[arg(long)]
    pub resolution: Option<String>,
    #[arg(long)]
    pub size: Option<String>,
    #[arg(long)]
    pub variant: Option<String>,
    #[arg(long)]
    pub seed: Option<i64>,
    #[arg(long, default_value_t = false)]
    pub compat_generations: bool,
    #[arg(long, default_value_t = false)]
    pub no_wait: bool,
    #[arg(long)]
    pub idempotency_key: Option<String>,
    #[arg(long)]
    pub session_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct VideoRemixArgs {
    #[arg(long)]
    pub video_id: String,
    #[arg(long, default_value = "sora-2")]
    pub model: String,
    #[arg(long)]
    pub prompt: String,
    #[arg(long)]
    pub variant: Option<String>,
    #[arg(long)]
    pub parent_task_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub no_wait: bool,
    #[arg(long)]
    pub idempotency_key: Option<String>,
    #[arg(long)]
    pub session_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct RealtimeArgs {
    #[arg(long, value_enum, default_value_t = RealtimeVendor::Openai)]
    pub vendor: RealtimeVendor,
    #[arg(long, default_value = "")]
    pub model: String,
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub audio_file: Option<PathBuf>,
    #[arg(long)]
    pub video_file: Option<PathBuf>,
    #[arg(long)]
    pub instructions: Option<String>,
    #[arg(long)]
    pub voice: Option<String>,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub wrap_wav: bool,
    #[arg(long)]
    pub timeout_ms: Option<u64>,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Subcommand)]
pub enum MediaTaskCommand {
    /// Inspect a persisted Engine 3 media task record by ID.
    Status {
        #[arg(long)]
        id: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Poll a pending/submitted Engine 3 media task until completion.
    Poll(Box<TaskPollArgs>),
    /// Resume all non-terminal Engine 3 media tasks for a vendor/kind.
    Resume(Box<TaskResumeArgs>),
    /// Cancel a pending/submitted Engine 3 media task.
    Cancel(Box<TaskCancelArgs>),
}

#[derive(Args)]
pub struct TaskPollArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub resource_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_suno_proxy: bool,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct TaskResumeArgs {
    #[arg(long)]
    pub vendor: String,
    #[arg(long)]
    pub kind: String,
    #[arg(long)]
    pub session_id: Option<String>,
    #[arg(long)]
    pub resource_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_suno_proxy: bool,
    #[arg(long, default_value_t = false)]
    pub allow_loopback_download: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

#[derive(Args)]
pub struct TaskCancelArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub reason: Option<String>,
    #[arg(long)]
    pub resource_id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub allow_suno_proxy: bool,
    #[command(flatten)]
    pub auth: AuthTargetArgs,
}

impl MediaArgs {
    pub fn valid(&self) -> bool {
        match &self.command {
            MediaCommand::Embed(a) => {
                !a.model.trim().is_empty() && !a.inputs.is_empty() && a.auth.valid()
            }
            MediaCommand::Rerank(a) => {
                !a.model.trim().is_empty()
                    && !a.query.trim().is_empty()
                    && !a.documents.is_empty()
                    && a.auth.valid()
            }
            MediaCommand::Image { command } => match command {
                ImageCommand::Generate(a) => {
                    !a.model.trim().is_empty() && !a.prompt.trim().is_empty() && a.auth.valid()
                }
                ImageCommand::Edit(a) => {
                    !a.model.trim().is_empty()
                        && !a.prompt.trim().is_empty()
                        && !a.images.is_empty()
                        && a.auth.valid()
                }
            },
            MediaCommand::Tts(a) => {
                let has_text = !a.text.trim().is_empty()
                    || a.ssml.as_deref().is_some_and(|s| !s.trim().is_empty())
                    || a.ssml_file.is_some();
                has_text && a.auth.valid()
            }
            MediaCommand::Stt(a) => {
                (a.file.is_some() ^ a.audio_url.is_some())
                    && a.threads.is_none_or(|t| t >= 1)
                    && a.auth.valid()
            }
            MediaCommand::Music(a) => a.auth.valid(),
            MediaCommand::Video { command } => match command {
                VideoCommand::Generate(a) => a.auth.valid(),
                VideoCommand::Remix(a) => {
                    !a.video_id.trim().is_empty() && !a.prompt.trim().is_empty() && a.auth.valid()
                }
            },
            MediaCommand::Realtime(a) => {
                (a.prompt.as_deref().is_some_and(|s| !s.trim().is_empty())
                    || a.audio_file.is_some()
                    || a.video_file.is_some())
                    && a.auth.valid()
            }
            MediaCommand::Task { command } => match command {
                MediaTaskCommand::Status { id, .. } => !id.trim().is_empty(),
                MediaTaskCommand::Poll(a) => !a.id.trim().is_empty() && a.auth.valid(),
                MediaTaskCommand::Resume(a) => {
                    !a.vendor.trim().is_empty() && !a.kind.trim().is_empty() && a.auth.valid()
                }
                MediaTaskCommand::Cancel(a) => !a.id.trim().is_empty() && a.auth.valid(),
            },
        }
    }
}

fn read_secret_stdin() -> Result<HostSecret, Failure> {
    let mut line = String::new();
    let mut reader = io::BufReader::new(io::stdin());
    reader.read_line(&mut line).map_err(|e| {
        Failure::custom(
            "credential_invalid",
            format!("读取 stdin API Key 失败: {e}"),
        )
    })?;
    let trimmed = line.trim_end_matches(['\r', '\n']).trim().to_owned();
    if trimmed.is_empty() || trimmed.len() >= 4096 || trimmed.chars().any(char::is_control) {
        return Err(Failure::fixed(
            "credential_invalid",
            "API Key 为空或包含无效控制字符",
            false,
        ));
    }
    Ok(HostSecret(trimmed))
}

fn read_env_secret(env_name: &str) -> Result<HostSecret, Failure> {
    let val = std::env::var(env_name).map_err(|_| {
        Failure::custom("credential_missing", format!("环境变量 {env_name} 未设置"))
    })?;
    let trimmed = val.trim().to_owned();
    if trimmed.is_empty() || trimmed.len() >= 4096 || trimmed.chars().any(char::is_control) {
        return Err(Failure::fixed(
            "credential_invalid",
            "环境变量 API Key 为空或包含无效控制字符",
            false,
        ));
    }
    Ok(HostSecret(trimmed))
}

fn ensure_store_opened(
    cache: &mut Option<Arc<Store>>,
    explicit_data_dir: Option<PathBuf>,
    top_data_dir: Option<PathBuf>,
) -> Result<Arc<Store>, Failure> {
    if let Some(existing) = cache {
        return Ok(existing.clone());
    }
    let data_dir = resolve_user_data_dir(explicit_data_dir.or(top_data_dir));
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        Failure::custom(
            "storage_error",
            format!("无法创建数据目录 {}: {e}", data_dir.display()),
        )
    })?;
    let store = Arc::new(
        Store::open(&data_dir.join("coda.sqlite3"))
            .map_err(|e| Failure::custom("storage_error", format!("无法打开本地数据库: {e}")))?,
    );
    *cache = Some(store.clone());
    Ok(store)
}

async fn resolve_endpoint_and_cred(
    vendor: &str,
    model: &str,
    auth: &AuthTargetArgs,
    top_data_dir: Option<PathBuf>,
    store_cache: &mut Option<Arc<Store>>,
) -> Result<(MediaEndpointConfig, MediaCredential), Failure> {
    let default_kind = CliMediaCredentialSource::auth_kind_for_vendor(vendor);
    let chosen_kind = auth.auth_mode.to_media_auth_kind(default_kind);
    let secondary = match &auth.secondary_key_env {
        Some(env_name) => Some(read_env_secret(env_name)?.as_str().to_owned()),
        None => None,
    };

    if auth.api_key_stdin {
        let secret = read_secret_stdin()?;
        let base = auth.base_url.clone().unwrap_or_else(|| {
            CliMediaCredentialSource::default_base_url_for_vendor(vendor).to_owned()
        });
        let primary = if chosen_kind == MediaAuthKind::VolcengineV3AppAccess {
            auth.app_key
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(secret.as_str())
                .to_owned()
        } else {
            secret.as_str().to_owned()
        };
        let sec = if chosen_kind == MediaAuthKind::VolcengineV3AppAccess && secondary.is_none() {
            Some(secret.as_str().to_owned())
        } else {
            secondary
        };
        let cred = MediaCredential::new(chosen_kind, primary, sec).map_err(media_failure)?;
        return Ok((MediaEndpointConfig::new(vendor, base, model), cred));
    }

    if let Some(env_name) = auth.api_key_env.as_deref() {
        let secret = read_env_secret(env_name)?;
        let base = auth.base_url.clone().unwrap_or_else(|| {
            CliMediaCredentialSource::default_base_url_for_vendor(vendor).to_owned()
        });
        let primary = if chosen_kind == MediaAuthKind::VolcengineV3AppAccess {
            auth.app_key
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(secret.as_str())
                .to_owned()
        } else {
            secret.as_str().to_owned()
        };
        let sec = if chosen_kind == MediaAuthKind::VolcengineV3AppAccess && secondary.is_none() {
            Some(secret.as_str().to_owned())
        } else {
            secondary
        };
        let cred = MediaCredential::new(chosen_kind, primary, sec).map_err(media_failure)?;
        return Ok((MediaEndpointConfig::new(vendor, base, model), cred));
    }

    let source = if auth.route.is_some() {
        let store = ensure_store_opened(store_cache, auth.data_dir.clone(), top_data_dir)?;
        CliMediaCredentialSource::with_store(store)
    } else {
        CliMediaCredentialSource::env_only()
    };

    let auth_kind_override = if auth.auth_mode != AuthModeOverride::Auto {
        Some(chosen_kind)
    } else {
        None
    };
    let (mut endpoint, cred) = source
        .resolve_media_endpoint_with_overrides(
            auth.route.as_deref(),
            vendor,
            model,
            auth_kind_override,
            auth.app_key.as_deref(),
            secondary,
        )
        .await
        .map_err(media_failure)?;

    if let Some(base_override) = &auth.base_url {
        endpoint.base_url = base_override.clone();
    }
    Ok((endpoint, cred))
}

fn is_loopback_base_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw.trim()) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn media_failure(err: MediaError) -> Failure {
    Failure {
        code: err.code.as_str(),
        message: err.message,
        retryable: err.retryable,
        details: None,
        after: None,
    }
}

fn print_json_or_pretty<T: serde::Serialize>(val: &T, json_output: bool) -> Result<(), Failure> {
    let serialized = if json_output {
        serde_json::to_string(val)
    } else {
        serde_json::to_string_pretty(val)
    }
    .map_err(|e| Failure::custom("serialize_error", e.to_string()))?;
    println!("{serialized}");
    Ok(())
}

pub(super) async fn run(
    args: MediaArgs,
    top_data_dir: Option<PathBuf>,
    workspace: Option<PathBuf>,
    json_output: bool,
) -> Result<(), Failure> {
    let http = MediaHttpClient::new().map_err(media_failure)?;
    let cancel = CancellationToken::new();
    let ws = workspace.unwrap_or_else(|| PathBuf::from("."));
    let artifact_store = Arc::new(WorkspaceMediaArtifactStore::for_workspace(&ws));
    let mut store_cache: Option<Arc<Store>> = None;

    match args.command {
        MediaCommand::Embed(embed) => {
            let embed = *embed;
            let (endpoint, cred) = resolve_endpoint_and_cred(
                embed.vendor.as_str(),
                &embed.model,
                &embed.auth,
                top_data_dir,
                &mut store_cache,
            )
            .await?;
            let resp = match embed.vendor {
                EmbedVendor::Openai => {
                    let input = if embed.inputs.len() == 1 {
                        EmbeddingInput::Single(embed.inputs.into_iter().next().unwrap())
                    } else {
                        EmbeddingInput::Batch(embed.inputs)
                    };
                    let encoding_format = embed.encoding_format.map(|f| match f {
                        EmbedFormat::Float => EmbeddingEncodingFormat::Float,
                        EmbedFormat::Base64 => EmbeddingEncodingFormat::Base64,
                    });
                    openai_embed(
                        &http,
                        &endpoint,
                        &cred,
                        &OpenAiEmbedRequest {
                            input,
                            encoding_format,
                            dimensions: embed.dimensions,
                            user: None,
                        },
                        &cancel,
                    )
                    .await
                    .map_err(media_failure)?
                }
                EmbedVendor::Gemini => gemini_embed(
                    &http,
                    &endpoint,
                    &cred,
                    &GeminiEmbedRequest {
                        inputs: embed.inputs,
                        task_type: embed.task_type,
                        title: None,
                        output_dimensionality: embed.dimensions,
                    },
                    &cancel,
                )
                .await
                .map_err(media_failure)?,
            };
            print_json_or_pretty(&resp, json_output)
        }
        MediaCommand::Rerank(rerank) => {
            let rerank = *rerank;
            let (endpoint, cred) = resolve_endpoint_and_cred(
                &rerank.vendor,
                &rerank.model,
                &rerank.auth,
                top_data_dir,
                &mut store_cache,
            )
            .await?;
            let resp = jina_rerank(
                &http,
                &endpoint,
                &cred,
                &JinaRerankRequest {
                    query: rerank.query,
                    documents: rerank.documents,
                    top_n: rerank.top_n,
                    return_documents: rerank.return_documents,
                },
                &cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        MediaCommand::Image { command } => match command {
            ImageCommand::Generate(generate) => {
                let generate = *generate;
                let (endpoint, cred) = resolve_endpoint_and_cred(
                    generate.vendor.as_str(),
                    &generate.model,
                    &generate.auth,
                    top_data_dir,
                    &mut store_cache,
                )
                .await?;
                let allow_loopback =
                    generate.allow_loopback_download || is_loopback_base_url(&endpoint.base_url);
                let owner_id = format!("cli-{}", uuid::Uuid::now_v7());

                let resp = match generate.vendor {
                    ImageVendor::Openai => openai_image(
                        &http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &OpenAiImageRequest {
                            prompt: generate.prompt,
                            n: generate.n,
                            size: generate.size,
                            quality: generate.quality,
                            background: generate.background,
                            output_format: generate.output_format,
                            response_format: generate.response_format,
                            allow_http_loopback_download: allow_loopback,
                            ..Default::default()
                        },
                        &cancel,
                    )
                    .await
                    .map_err(media_failure)?,
                    ImageVendor::Gemini => {
                        let mut refs = Vec::with_capacity(generate.ref_images.len());
                        for p in &generate.ref_images {
                            refs.push(read_image_file(Path::new(p))?);
                        }
                        gemini_generate_image(
                            &http,
                            artifact_store.as_ref(),
                            &endpoint,
                            &cred,
                            &owner_id,
                            &GeminiImageRequest {
                                prompt: generate.prompt,
                                reference_images: refs,
                                aspect_ratio: generate.aspect_ratio,
                                image_size: generate.size,
                                ..Default::default()
                            },
                            &cancel,
                        )
                        .await
                        .map_err(media_failure)?
                    }
                    ImageVendor::Imagen => imagen_predict(
                        &http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &GeminiImageRequest {
                            prompt: generate.prompt,
                            aspect_ratio: generate.aspect_ratio,
                            image_size: generate.size,
                            sample_count: generate.n,
                            output_mime_type: generate.output_format,
                            ..Default::default()
                        },
                        &cancel,
                    )
                    .await
                    .map_err(media_failure)?,
                    ImageVendor::Ark => ark_seedream_generate(
                        &http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &ArkSeedreamRequest {
                            prompt: generate.prompt,
                            reference_images: generate.ref_images,
                            size: generate.size,
                            response_format: generate.response_format,
                            watermark: generate.watermark,
                            allow_http_loopback_download: allow_loopback,
                            ..Default::default()
                        },
                        &cancel,
                    )
                    .await
                    .map_err(media_failure)?,
                };
                print_json_or_pretty(&resp, json_output)
            }
            ImageCommand::Edit(edit) => {
                let edit = *edit;
                if edit.vendor != ImageVendor::Openai {
                    return Err(Failure::custom(
                        "invalid_request",
                        "multipart image edit is only supported for --vendor openai",
                    ));
                }
                let (endpoint, cred) = resolve_endpoint_and_cred(
                    edit.vendor.as_str(),
                    &edit.model,
                    &edit.auth,
                    top_data_dir,
                    &mut store_cache,
                )
                .await?;
                let allow_loopback =
                    edit.allow_loopback_download || is_loopback_base_url(&endpoint.base_url);
                let owner_id = format!("cli-{}", uuid::Uuid::now_v7());

                let mut images = Vec::with_capacity(edit.images.len());
                for p in &edit.images {
                    images.push(read_image_file(p)?);
                }
                let mask = match &edit.mask {
                    Some(p) => Some(read_image_file(p)?),
                    None => None,
                };

                let resp = openai_image(
                    &http,
                    artifact_store.as_ref(),
                    &endpoint,
                    &cred,
                    &owner_id,
                    &OpenAiImageRequest {
                        prompt: edit.prompt,
                        n: edit.n,
                        size: edit.size,
                        quality: edit.quality,
                        background: edit.background,
                        output_format: edit.output_format,
                        response_format: edit.response_format,
                        images,
                        mask,
                        allow_http_loopback_download: allow_loopback,
                        ..Default::default()
                    },
                    &cancel,
                )
                .await
                .map_err(media_failure)?;
                print_json_or_pretty(&resp, json_output)
            }
        },
        MediaCommand::Tts(tts) => {
            run_tts(
                *tts,
                &http,
                artifact_store,
                top_data_dir,
                &mut store_cache,
                json_output,
                &cancel,
            )
            .await
        }
        MediaCommand::Stt(stt) => {
            run_stt(
                *stt,
                &http,
                artifact_store,
                top_data_dir,
                &mut store_cache,
                json_output,
                &cancel,
            )
            .await
        }
        MediaCommand::Music(music) => {
            run_music(
                *music,
                &http,
                artifact_store,
                top_data_dir,
                &mut store_cache,
                json_output,
                &cancel,
            )
            .await
        }
        MediaCommand::Video { command } => {
            run_video(
                command,
                &http,
                artifact_store,
                top_data_dir,
                &mut store_cache,
                json_output,
                &cancel,
            )
            .await
        }
        MediaCommand::Realtime(rt) => {
            run_realtime(
                *rt,
                artifact_store,
                top_data_dir,
                &mut store_cache,
                json_output,
                &cancel,
            )
            .await
        }
        MediaCommand::Task { command } => {
            run_task(
                command,
                &http,
                artifact_store,
                top_data_dir,
                &mut store_cache,
                json_output,
                &cancel,
            )
            .await
        }
    }
}

async fn run_tts(
    args: TtsArgs,
    http: &MediaHttpClient,
    artifact_store: Arc<WorkspaceMediaArtifactStore>,
    top_data_dir: Option<PathBuf>,
    store_cache: &mut Option<Arc<Store>>,
    json_output: bool,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    let mut vendor_str = args.vendor.as_str();
    if args.vendor == TtsVendor::Volcengine
        && matches!(args.transport, TtsTransport::V1Http | TtsTransport::V1Ws)
    {
        vendor_str = "volcengine-v1";
    }

    let (mut endpoint, cred) = resolve_endpoint_and_cred(
        vendor_str,
        &args.model,
        &args.auth,
        top_data_dir.clone(),
        store_cache,
    )
    .await?;

    if args.auth.base_url.is_none() && args.auth.route.is_none() {
        match args.vendor {
            TtsVendor::Azure => {
                if let Some(region) = args.region.as_deref() {
                    endpoint.base_url = azure_tts_regional_base(region).map_err(media_failure)?;
                }
            }
            TtsVendor::Minimax => {
                endpoint.base_url = minimax_regional_base(false).to_owned();
            }
            TtsVendor::MinimaxCn => {
                endpoint.base_url = minimax_regional_base(true).to_owned();
            }
            _ => {}
        }
    }

    let allow_loopback = args.allow_loopback_download || is_loopback_base_url(&endpoint.base_url);
    let owner_id = format!("cli-{}", uuid::Uuid::now_v7());
    let parsed_fmt = match args.response_format.as_deref() {
        Some(raw) => Some(TtsAudioFormat::parse(raw).ok_or_else(|| {
            Failure::custom("invalid_request", format!("不支持的音频格式 '{raw}'"))
        })?),
        None => None,
    };

    match args.vendor {
        TtsVendor::Openai => {
            let stream_format = match args.transport {
                TtsTransport::Sse => Some(OpenAiTtsStreamFormat::Sse),
                _ => Some(OpenAiTtsStreamFormat::Audio),
            };
            let resp = openai_tts(
                http,
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &OpenAiTtsRequest {
                    input: args.text,
                    voice: args.voice.unwrap_or_else(|| "alloy".into()),
                    instructions: args.instructions,
                    response_format: parsed_fmt,
                    speed: args.speed,
                    stream_format,
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        TtsVendor::Gemini => {
            let voice = if !args.speakers.is_empty() {
                let mut pairs = Vec::with_capacity(args.speakers.len());
                for item in &args.speakers {
                    let (spk, vname) = item.split_once('=').ok_or_else(|| {
                        Failure::custom(
                            "invalid_request",
                            format!("--speaker 必须为 speaker=voice 格式: {item}"),
                        )
                    })?;
                    pairs.push(GeminiSpeakerVoice {
                        speaker: spk.trim().to_owned(),
                        voice_name: vname.trim().to_owned(),
                    });
                }
                GeminiSpeechVoice::MultiSpeaker(pairs)
            } else {
                GeminiSpeechVoice::Single(args.voice.unwrap_or_else(|| "Kore".into()))
            };
            let resp = gemini_tts(
                http,
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &GeminiTtsRequest {
                    text: args.text,
                    voice,
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        TtsVendor::Elevenlabs => {
            let voice_id = args.voice.ok_or_else(|| {
                Failure::custom("invalid_request", "ElevenLabs TTS 需要 --voice <voice_id>")
            })?;
            let resp = elevenlabs_tts(
                http,
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &ElevenLabsTtsRequest {
                    voice_id,
                    text: args.text,
                    output_format: args.response_format,
                    stream: args.transport == TtsTransport::Sse,
                    language_code: args.language,
                    ..Default::default()
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        TtsVendor::Azure => {
            let raw_ssml = match (args.ssml, args.ssml_file) {
                (Some(s), _) => Some(s),
                (None, Some(path)) => Some(std::fs::read_to_string(&path).map_err(|e| {
                    Failure::custom("io_error", format!("读取 SSML 文件失败: {e}"))
                })?),
                (None, None) => None,
            };
            let resp = azure_tts(
                http,
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &AzureTtsRequest {
                    text: args.text,
                    raw_ssml,
                    voice: args.voice.unwrap_or_else(|| "en-US-JennyNeural".into()),
                    lang: args.language.unwrap_or_else(|| "en-US".into()),
                    output_format: args
                        .response_format
                        .unwrap_or_else(|| "audio-24khz-48kbitrate-mono-mp3".into()),
                    user_agent: args.user_agent,
                    ..Default::default()
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        TtsVendor::Minimax | TtsVendor::MinimaxCn => {
            let mm_req = MiniMaxTtsRequest {
                text: args.text,
                voice_id: args
                    .voice
                    .unwrap_or_else(|| "English_expressive_narrator".into()),
                speed: args.speed,
                pitch: args.pitch,
                sample_rate: args.sample_rate,
                format: parsed_fmt,
                stream: args.transport == TtsTransport::Sse,
                language_boost: args.language,
                allow_http_loopback_download: allow_loopback,
                ..Default::default()
            };
            match args.transport {
                TtsTransport::Ws => {
                    let resp = minimax_tts_ws(
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &mm_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
                TtsTransport::Async => {
                    let store =
                        ensure_store_opened(store_cache, args.auth.data_dir.clone(), top_data_dir)?;
                    let repo = Arc::new(SqliteMediaTaskRepository(store));
                    let format = parsed_fmt.unwrap_or(TtsAudioFormat::Mp3);
                    let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                        .with_allow_http_loopback_download(allow_loopback);
                    if args.no_wait {
                        let (adapter, keys) =
                            MiniMaxAsyncTtsAdapter::from_endpoint(&endpoint, &cred, format)
                                .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model: endpoint.model.clone(),
                                    route_id: endpoint.route_id.clone(),
                                    payload: build_minimax_payload(&endpoint.model, &mm_req, false),
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let rec = minimax_tts_async(
                            &runner,
                            &endpoint,
                            &cred,
                            &mm_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    }
                }
                _ => {
                    let resp = minimax_tts(
                        http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &mm_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
            }
        }
        TtsVendor::Volcengine => {
            let volc_req = VolcengineTtsRequest {
                text: args.text,
                speaker: args
                    .voice
                    .unwrap_or_else(|| "zh_female_cancan_mars_bigtts".into()),
                resource_id: args.resource_id.clone(),
                format: parsed_fmt,
                sample_rate: args.sample_rate,
                v1_appid: args.v1_appid,
                v1_cluster: args.v1_cluster,
                allow_http_loopback_download: allow_loopback,
                ..Default::default()
            };
            match args.transport {
                TtsTransport::SeedAudio => {
                    let resp = volcengine_seed_audio_create(
                        http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &volc_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
                TtsTransport::Ws => {
                    let resp = volcengine_v3_tts_ws(
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &volc_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
                TtsTransport::V1Http => {
                    let resp = volcengine_v1_tts_http(
                        http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &volc_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
                TtsTransport::V1Ws => {
                    let resp = volcengine_v1_tts_ws(
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &volc_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
                TtsTransport::Async => {
                    let store =
                        ensure_store_opened(store_cache, args.auth.data_dir.clone(), top_data_dir)?;
                    let repo = Arc::new(SqliteMediaTaskRepository(store));
                    let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                        .with_allow_http_loopback_download(allow_loopback);
                    if args.no_wait {
                        let fmt = parsed_fmt.unwrap_or(TtsAudioFormat::Mp3);
                        let (adapter, keys) = VolcengineV3AsyncTtsAdapter::from_endpoint(
                            &endpoint,
                            &cred,
                            args.resource_id.as_deref(),
                            fmt,
                        )
                        .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model: endpoint.model.clone(),
                                    route_id: endpoint.route_id.clone(),
                                    payload: build_volcengine_v3_tts_body(
                                        &endpoint.model,
                                        &volc_req,
                                    ),
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let rec = volcengine_v3_tts_async(
                            &runner,
                            &endpoint,
                            &cred,
                            &volc_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    }
                }
                _ => {
                    let resp = volcengine_v3_tts_http(
                        http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &volc_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
            }
        }
        TtsVendor::Dashscope => {
            let ds_req = DashScopeTtsRequest {
                text: args.text,
                voice: args.voice.unwrap_or_else(|| "Cherry".into()),
                language_type: args.language,
                format: parsed_fmt,
                sample_rate: args.sample_rate,
                rate: args.speed,
                sse: args.transport == TtsTransport::Sse,
                allow_http_loopback_download: allow_loopback,
                ..Default::default()
            };
            match args.transport {
                TtsTransport::Ws => {
                    let resp = dashscope_cosyvoice_ws(
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &ds_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
                TtsTransport::Bailian => {
                    let resp = dashscope_speech_synthesizer_http(
                        http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &ds_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
                _ => {
                    let resp = dashscope_qwen_tts(
                        http,
                        artifact_store.as_ref(),
                        &endpoint,
                        &cred,
                        &owner_id,
                        &ds_req,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
            }
        }
        TtsVendor::PartnerSeedAudio => {
            let store = ensure_store_opened(store_cache, args.auth.data_dir.clone(), top_data_dir)?;
            let repo = Arc::new(SqliteMediaTaskRepository(store));
            let psa_req = PartnerSeedAudioRequest {
                text: args.text,
                voice: args.voice,
                format: parsed_fmt,
                sample_rate: args.sample_rate,
                speed: args.speed,
                pitch: args.pitch,
                allow_http_loopback_download: allow_loopback,
                ..Default::default()
            };
            let format = parsed_fmt.unwrap_or(TtsAudioFormat::Mp3);
            let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                .with_allow_http_loopback_download(allow_loopback);
            if args.no_wait {
                let (adapter, keys) =
                    PartnerSeedAudioAdapter::from_endpoint(&endpoint, &cred, format)
                        .map_err(media_failure)?;
                let payload = build_partner_seed_audio_payload(&endpoint.model, &psa_req)
                    .map_err(media_failure)?;
                let (rec, _) = runner
                    .submit_and_persist(
                        &adapter,
                        &Engine3SubmitInput {
                            idempotency_key: args.idempotency_key,
                            session_id: args.session_id,
                            parent_task_id: None,
                            model: endpoint.model.clone(),
                            route_id: endpoint.route_id.clone(),
                            payload,
                        },
                        &keys,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                print_json_or_pretty(&rec, json_output)
            } else {
                let rec = partner_seed_audio_tts(
                    &runner,
                    &endpoint,
                    &cred,
                    &psa_req,
                    args.idempotency_key,
                    args.session_id,
                    cancel,
                )
                .await
                .map_err(media_failure)?;
                print_json_or_pretty(&rec, json_output)
            }
        }
    }
}

async fn run_stt(
    args: SttArgs,
    http: &MediaHttpClient,
    artifact_store: Arc<WorkspaceMediaArtifactStore>,
    top_data_dir: Option<PathBuf>,
    store_cache: &mut Option<Arc<Store>>,
    json_output: bool,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    if args.vendor == SttVendor::Whisper {
        let data_dir = resolve_user_data_dir(args.auth.data_dir.clone().or(top_data_dir));
        let runner = WhisperCliRunner::new(WhisperCliConfig {
            data_dir: Some(data_dir),
            executable_path: args.whisper_bin,
            model_path: args.whisper_model,
            ffmpeg_path: args.ffmpeg_bin,
            ffprobe_path: args.ffprobe_bin,
            threads: args.threads,
            temp_root: None,
        });
        let path = args.file.ok_or_else(|| {
            Failure::custom("invalid_request", "whisper 本地转写仅支持 --file <PATH>")
        })?;
        let abs_path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map_err(|e| Failure::custom("io_error", e.to_string()))?
                .join(path)
        };
        let resp = local_whisper_transcribe(
            &runner,
            &LocalAsrRequest {
                input: LocalAsrInput::FilePath(abs_path),
                lang: args.language,
            },
            cancel,
        )
        .await
        .map_err(media_failure)?;
        return print_json_or_pretty(&resp, json_output);
    }

    let source = if let Some(path) = &args.file {
        let bytes = std::fs::read(path).map_err(|e| {
            Failure::custom(
                "io_error",
                format!("读取音频文件失败 {}: {e}", path.display()),
            )
        })?;
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("audio.mp3")
            .to_owned();
        let mime_type = args
            .mime_type
            .clone()
            .unwrap_or_else(|| infer_audio_mime(&filename).to_owned());
        SttAudioSource::Bytes {
            filename,
            mime_type,
            bytes: bytes.into(),
        }
    } else if let Some(url) = &args.audio_url {
        SttAudioSource::Url {
            url: url.clone(),
            mime_type: args.mime_type.clone(),
        }
    } else {
        return Err(Failure::custom(
            "invalid_request",
            "STT 需要 --file 或 --audio-url",
        ));
    };

    let (endpoint, cred) = resolve_endpoint_and_cred(
        args.vendor.as_str(),
        &args.model,
        &args.auth,
        top_data_dir.clone(),
        store_cache,
    )
    .await?;
    let allow_loopback = args.allow_loopback_download || is_loopback_base_url(&endpoint.base_url);

    match args.vendor {
        SttVendor::Openai => {
            let mut grans = Vec::with_capacity(args.timestamp_granularities.len());
            for g in &args.timestamp_granularities {
                match g.trim().to_ascii_lowercase().as_str() {
                    "word" => grans.push(OpenAiSttGranularity::Word),
                    "segment" => grans.push(OpenAiSttGranularity::Segment),
                    other => {
                        return Err(Failure::custom(
                            "invalid_request",
                            format!("不支持的 timestamp granularity '{other}'"),
                        ));
                    }
                }
            }
            let resp = openai_stt(
                http,
                &endpoint,
                &cred,
                &OpenAiSttRequest {
                    source: Some(source),
                    translate: args.translate,
                    language: args.language,
                    prompt: args.prompt,
                    response_format: args.response_format,
                    temperature: None,
                    timestamp_granularities: grans,
                    stream: args.stream,
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        SttVendor::Elevenlabs => {
            let resp = elevenlabs_stt(
                http,
                &endpoint,
                &cred,
                &ElevenLabsSttRequest {
                    source: Some(source),
                    language_code: args.language,
                    diarize: Some(args.diarize),
                    ..Default::default()
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        SttVendor::Gemini => {
            let resp = gemini_stt(
                http,
                &endpoint,
                &cred,
                &GeminiSttRequest {
                    source: Some(source),
                    prompt: args.prompt,
                    inline_threshold_bytes: None,
                    allow_http_loopback_upload: allow_loopback,
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        SttVendor::Dashscope => {
            let ds_req = DashScopeSttRequest {
                source: Some(source),
                language_hints: args.language.into_iter().collect(),
                diarization_enabled: Some(args.diarize),
                allow_http_loopback_download: allow_loopback,
                ..Default::default()
            };
            let use_async = args.transport == SttTransport::Async
                || (args.transport == SttTransport::Auto && args.audio_url.is_some());
            if args.transport == SttTransport::Ws {
                let resp = dashscope_asr_ws(&endpoint, &cred, &ds_req, cancel)
                    .await
                    .map_err(media_failure)?;
                print_json_or_pretty(&resp, json_output)
            } else if use_async {
                let store =
                    ensure_store_opened(store_cache, args.auth.data_dir.clone(), top_data_dir)?;
                let repo = Arc::new(SqliteMediaTaskRepository(store));
                let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                    .with_allow_http_loopback_download(allow_loopback);
                if args.no_wait {
                    let model = if endpoint.model.trim().is_empty() {
                        "paraformer-v2".to_owned()
                    } else {
                        endpoint.model.clone()
                    };
                    let (adapter, keys) =
                        DashScopeParaformerAdapter::from_endpoint(&endpoint, &cred)
                            .map_err(media_failure)?;
                    let payload =
                        build_paraformer_submit_payload(&model, &ds_req).map_err(media_failure)?;
                    let (rec, _) = runner
                        .submit_and_persist(
                            &adapter,
                            &Engine3SubmitInput {
                                idempotency_key: args.idempotency_key,
                                session_id: args.session_id,
                                parent_task_id: None,
                                model,
                                route_id: endpoint.route_id.clone(),
                                payload,
                            },
                            &keys,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                    print_json_or_pretty(&rec, json_output)
                } else {
                    let resp = dashscope_paraformer_transcribe(
                        &runner,
                        &endpoint,
                        &cred,
                        &ds_req,
                        args.idempotency_key,
                        args.session_id,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
            } else {
                let resp = dashscope_qwen_asr(http, &endpoint, &cred, &ds_req, cancel)
                    .await
                    .map_err(media_failure)?;
                print_json_or_pretty(&resp, json_output)
            }
        }
        SttVendor::Volcengine => {
            let volc_req = VolcengineAsrRequest {
                source: Some(source),
                resource_id: args.resource_id.clone(),
                language: args.language,
                ..Default::default()
            };
            match args.transport {
                SttTransport::Ws => {
                    let resp = volcengine_sauc_ws(&endpoint, &cred, &volc_req, cancel)
                        .await
                        .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
                SttTransport::Async => {
                    let store =
                        ensure_store_opened(store_cache, args.auth.data_dir.clone(), top_data_dir)?;
                    let repo = Arc::new(SqliteMediaTaskRepository(store));
                    let runner = Engine3Runner::new(http.clone(), repo, artifact_store);
                    if args.no_wait {
                        let model = if endpoint.model.trim().is_empty() {
                            "bigmodel".to_owned()
                        } else {
                            endpoint.model.clone()
                        };
                        let (adapter, keys) = VolcengineAsrAdapter::from_endpoint(
                            &endpoint,
                            &cred,
                            args.resource_id.as_deref(),
                        )
                        .map_err(media_failure)?;
                        let payload =
                            build_volcengine_asr_payload(&volc_req).map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model,
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let resp = volcengine_asr_transcribe(
                            &runner,
                            &endpoint,
                            &cred,
                            &volc_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&resp, json_output)
                    }
                }
                _ => {
                    let resp = volcengine_asr_flash(http, &endpoint, &cred, &volc_req, cancel)
                        .await
                        .map_err(media_failure)?;
                    print_json_or_pretty(&resp, json_output)
                }
            }
        }
        SttVendor::Whisper => unreachable!(),
    }
}

async fn run_music(
    args: MusicArgs,
    http: &MediaHttpClient,
    artifact_store: Arc<WorkspaceMediaArtifactStore>,
    top_data_dir: Option<PathBuf>,
    store_cache: &mut Option<Arc<Store>>,
    json_output: bool,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    let (mut endpoint, cred) = resolve_endpoint_and_cred(
        args.vendor.as_str(),
        &args.model,
        &args.auth,
        top_data_dir.clone(),
        store_cache,
    )
    .await?;
    if args.auth.base_url.is_none() && args.auth.route.is_none() {
        match args.vendor {
            MusicVendor::Minimax => {
                endpoint.base_url = minimax_regional_base(false).to_owned();
            }
            MusicVendor::MinimaxCn => {
                endpoint.base_url = minimax_regional_base(true).to_owned();
            }
            _ => {}
        }
    }
    let allow_loopback = args.allow_loopback_download || is_loopback_base_url(&endpoint.base_url);
    let owner_id = format!("cli-{}", uuid::Uuid::now_v7());

    match args.vendor {
        MusicVendor::Minimax | MusicVendor::MinimaxCn => {
            let format = match args.response_format.as_deref() {
                Some(raw) => Some(MiniMaxMusicFormat::parse(raw).ok_or_else(|| {
                    Failure::custom("invalid_request", format!("不支持的音频格式 '{raw}'"))
                })?),
                None => None,
            };
            let resp = minimax_music(
                http,
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &MiniMaxMusicRequest {
                    prompt: args.prompt,
                    lyrics: args.lyrics,
                    is_instrumental: args.is_instrumental,
                    lyrics_optimizer: args.lyrics_optimizer,
                    sample_rate: args.sample_rate,
                    bitrate: args.bitrate,
                    format,
                    stream: args.transport == MusicTransport::Stream,
                    output_format: None,
                    allow_http_loopback_download: allow_loopback,
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        MusicVendor::Elevenlabs => {
            if args.transport == MusicTransport::Plan {
                let prompt = args.prompt.unwrap_or_default();
                let resp = elevenlabs_music_plan(
                    http,
                    &endpoint,
                    &cred,
                    &ElevenLabsMusicPlanRequest {
                        prompt,
                        music_length_ms: args.music_length_ms,
                    },
                    cancel,
                )
                .await
                .map_err(media_failure)?;
                return print_json_or_pretty(&resp, json_output);
            }
            let plan_val = match (args.composition_plan, args.composition_plan_file) {
                (Some(raw), _) => Some(serde_json::from_str(&raw).map_err(|e| {
                    Failure::custom(
                        "invalid_request",
                        format!("composition_plan JSON 无效: {e}"),
                    )
                })?),
                (None, Some(path)) => {
                    let txt = std::fs::read_to_string(&path).map_err(|e| {
                        Failure::custom("io_error", format!("读取 composition_plan 文件失败: {e}"))
                    })?;
                    Some(serde_json::from_str(&txt).map_err(|e| {
                        Failure::custom(
                            "invalid_request",
                            format!("composition_plan JSON 无效: {e}"),
                        )
                    })?)
                }
                (None, None) => None,
            };
            let mode = match args.transport {
                MusicTransport::Stream => ElevenLabsMusicMode::Stream,
                MusicTransport::Detailed => ElevenLabsMusicMode::Detailed,
                _ => ElevenLabsMusicMode::Compose,
            };
            let resp = elevenlabs_music(
                http,
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &ElevenLabsMusicRequest {
                    prompt: args.prompt,
                    composition_plan: plan_val,
                    music_length_ms: args.music_length_ms,
                    force_instrumental: args.is_instrumental,
                    respect_sections_durations: None,
                    output_format: args.response_format,
                    mode,
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        MusicVendor::Lyria3 => {
            let resp = lyria3_generate(
                http,
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &Lyria3Request {
                    prompt: args.prompt.unwrap_or_default(),
                    ..Default::default()
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        MusicVendor::Lyria2 => {
            let resp = lyria2_predict(
                http,
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &Lyria2Request {
                    prompt: args.prompt.unwrap_or_default(),
                    seed: args.seed,
                    sample_count: args.sample_count,
                    ..Default::default()
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        MusicVendor::LyriaRealtime => {
            let prompt = args.prompt.unwrap_or_default();
            let prompts = if prompt.trim().is_empty() {
                Vec::new()
            } else {
                vec![LyriaWeightedPrompt {
                    text: prompt,
                    weight: 1.0,
                }]
            };
            let resp = lyria_realtime_ws(
                artifact_store.as_ref(),
                &endpoint,
                &cred,
                &owner_id,
                &LyriaRealTimeRequest {
                    prompts,
                    duration_secs: args.duration_secs,
                    wrap_wav: args.wrap_wav,
                    ..Default::default()
                },
                cancel,
            )
            .await
            .map_err(media_failure)?;
            print_json_or_pretty(&resp, json_output)
        }
        MusicVendor::Suno => {
            let store = ensure_store_opened(store_cache, args.auth.data_dir.clone(), top_data_dir)?;
            let repo = Arc::new(SqliteMediaTaskRepository(store));
            let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                .with_allow_http_loopback_download(allow_loopback);
            let action = match args.action.as_deref() {
                Some(raw) => SunoAction::parse(raw).ok_or_else(|| {
                    Failure::custom("invalid_request", format!("不支持的 Suno action '{raw}'"))
                })?,
                None => SunoAction::Music,
            };
            let suno_req = SunoMusicRequest {
                action,
                prompt: args.prompt,
                tags: args.tags,
                title: args.title,
                mv: args.mv,
                make_instrumental: args.is_instrumental,
                gpt_description_prompt: None,
                continue_clip_id: args.continue_clip_id,
                continue_at: args.continue_at,
                clip_id: args.clip_id,
                use_post_fetch: args.transport == MusicTransport::PostFetch,
                allow_suno_proxy: args.allow_suno_proxy,
            };
            if args.no_wait {
                let (adapter, keys) = SunoMusicAdapter::from_endpoint(
                    &endpoint,
                    &cred,
                    suno_req.use_post_fetch,
                    suno_req.allow_suno_proxy,
                )
                .map_err(media_failure)?;
                let payload = build_suno_submit_payload(&suno_req).map_err(media_failure)?;
                let (rec, _) = runner
                    .submit_and_persist(
                        &adapter,
                        &Engine3SubmitInput {
                            idempotency_key: args.idempotency_key,
                            session_id: args.session_id,
                            parent_task_id: None,
                            model: if endpoint.model.trim().is_empty() {
                                "suno".to_owned()
                            } else {
                                endpoint.model.clone()
                            },
                            route_id: endpoint.route_id.clone(),
                            payload,
                        },
                        &keys,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                print_json_or_pretty(&rec, json_output)
            } else {
                let completed = suno_music_submit_and_wait(
                    &runner,
                    &endpoint,
                    &cred,
                    &suno_req,
                    args.idempotency_key,
                    args.session_id,
                    cancel,
                )
                .await
                .map_err(media_failure)?;
                print_json_or_pretty(&completed.record, json_output)
            }
        }
        MusicVendor::CodaGateway | MusicVendor::PartnerMusic => {
            let store = ensure_store_opened(store_cache, args.auth.data_dir.clone(), top_data_dir)?;
            let repo = Arc::new(SqliteMediaTaskRepository(store));
            let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                .with_allow_http_loopback_download(allow_loopback);
            let mode = if args.vendor == MusicVendor::PartnerMusic {
                CodaMusicGatewayMode::PartnerUpstream
            } else {
                CodaMusicGatewayMode::CodaGateway
            };
            let gw_req = CodaMusicGatewayRequest {
                action: args.action.unwrap_or_else(|| "generate".to_owned()),
                prompt: args.prompt,
                mode,
                ..Default::default()
            };
            if args.no_wait {
                let (adapter, keys) =
                    CodaMusicGatewayAdapter::from_endpoint(&endpoint, &cred, mode)
                        .map_err(media_failure)?;
                let payload =
                    build_coda_music_payload(&endpoint.model, &gw_req).map_err(media_failure)?;
                let (rec, _) = runner
                    .submit_and_persist(
                        &adapter,
                        &Engine3SubmitInput {
                            idempotency_key: args.idempotency_key,
                            session_id: args.session_id,
                            parent_task_id: None,
                            model: endpoint.model.clone(),
                            route_id: endpoint.route_id.clone(),
                            payload,
                        },
                        &keys,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                print_json_or_pretty(&rec, json_output)
            } else {
                let completed = coda_music_gateway_run(
                    &runner,
                    &endpoint,
                    &cred,
                    &gw_req,
                    args.idempotency_key,
                    args.session_id,
                    cancel,
                )
                .await
                .map_err(media_failure)?;
                print_json_or_pretty(&completed.record, json_output)
            }
        }
    }
}

async fn run_video(
    command: VideoCommand,
    http: &MediaHttpClient,
    artifact_store: Arc<WorkspaceMediaArtifactStore>,
    top_data_dir: Option<PathBuf>,
    store_cache: &mut Option<Arc<Store>>,
    json_output: bool,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    match command {
        VideoCommand::Remix(remix) => {
            let remix = *remix;
            let (endpoint, cred) = resolve_endpoint_and_cred(
                "sora",
                &remix.model,
                &remix.auth,
                top_data_dir.clone(),
                store_cache,
            )
            .await?;
            let allow_loopback =
                remix.allow_loopback_download || is_loopback_base_url(&endpoint.base_url);
            let store =
                ensure_store_opened(store_cache, remix.auth.data_dir.clone(), top_data_dir)?;
            let repo = Arc::new(SqliteMediaTaskRepository(store));
            let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                .with_allow_http_loopback_download(allow_loopback);
            let variant = SoraContentVariant::parse(remix.variant.as_deref());
            if remix.no_wait {
                let (adapter, keys) = SoraVideoAdapter::from_endpoint(
                    &endpoint,
                    &cred,
                    variant,
                    None,
                    Some(remix.video_id.clone()),
                )
                .map_err(media_failure)?;
                let (rec, _) = runner
                    .submit_and_persist(
                        &adapter,
                        &Engine3SubmitInput {
                            idempotency_key: remix.idempotency_key,
                            session_id: remix.session_id,
                            parent_task_id: remix.parent_task_id,
                            model: endpoint.model.clone(),
                            route_id: endpoint.route_id.clone(),
                            payload: json!({
                                "remix_of": remix.video_id,
                                "prompt": remix.prompt,
                            }),
                        },
                        &keys,
                        cancel,
                    )
                    .await
                    .map_err(media_failure)?;
                print_json_or_pretty(&rec, json_output)
            } else {
                let res = sora_remix_video(
                    &runner,
                    &endpoint,
                    &cred,
                    &SoraRemixRequest {
                        video_id: remix.video_id,
                        prompt: remix.prompt,
                        variant,
                        parent_task_id: remix.parent_task_id,
                    },
                    remix.idempotency_key,
                    remix.session_id,
                    cancel,
                )
                .await
                .map_err(media_failure)?;
                print_json_or_pretty(&res, json_output)
            }
        }
        VideoCommand::Generate(args) => {
            let args = *args;
            let (mut endpoint, cred) = resolve_endpoint_and_cred(
                args.vendor.as_str(),
                &args.model,
                &args.auth,
                top_data_dir.clone(),
                store_cache,
            )
            .await?;
            if args.auth.base_url.is_none() && args.auth.route.is_none() {
                match args.vendor {
                    VideoVendor::Minimax => {
                        endpoint.base_url = minimax_regional_base(false).to_owned();
                    }
                    VideoVendor::MinimaxCn => {
                        endpoint.base_url = minimax_regional_base(true).to_owned();
                    }
                    _ => {}
                }
            }
            let allow_loopback =
                args.allow_loopback_download || is_loopback_base_url(&endpoint.base_url);
            let store = ensure_store_opened(store_cache, args.auth.data_dir.clone(), top_data_dir)?;
            let repo = Arc::new(SqliteMediaTaskRepository(store));
            let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                .with_allow_http_loopback_download(allow_loopback);

            match args.vendor {
                VideoVendor::Sora => {
                    let seconds = args
                        .duration
                        .as_deref()
                        .map(SoraSeconds::parse)
                        .transpose()
                        .map_err(media_failure)?;
                    let input_reference = if let Some(img_path) = args.image.as_deref() {
                        let p = Path::new(img_path);
                        let bytes = std::fs::read(p).map_err(|e| {
                            Failure::custom("io_error", format!("读取参考图像失败: {e}"))
                        })?;
                        let filename = p
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("input_reference.png")
                            .to_owned();
                        let lower = filename.to_ascii_lowercase();
                        let mime_type = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                            "image/jpeg".to_owned()
                        } else if lower.ends_with(".webp") {
                            "image/webp".to_owned()
                        } else {
                            "image/png".to_owned()
                        };
                        Some(SoraInputReference {
                            filename,
                            mime_type,
                            bytes: bytes.into(),
                        })
                    } else {
                        None
                    };
                    let variant = SoraContentVariant::parse(args.variant.as_deref());
                    let sora_req = SoraVideoRequest {
                        prompt: args.prompt,
                        size: args.size,
                        seconds,
                        input_reference,
                        variant,
                    };
                    if args.no_wait {
                        let model = if endpoint.model.trim().is_empty() {
                            "sora-2".to_owned()
                        } else {
                            endpoint.model.clone()
                        };
                        let payload =
                            build_sora_submit_payload(&model, &sora_req).map_err(media_failure)?;
                        let (adapter, keys) = SoraVideoAdapter::from_endpoint(
                            &endpoint,
                            &cred,
                            variant,
                            sora_req.input_reference,
                            None,
                        )
                        .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model,
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let res = sora_generate_video(
                            &runner,
                            &endpoint,
                            &cred,
                            &sora_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&res, json_output)
                    }
                }
                VideoVendor::Veo | VideoVendor::VeoVertex => {
                    let platform = if args.vendor == VideoVendor::VeoVertex {
                        VeoPlatform::Vertex
                    } else {
                        VeoPlatform::Gemini
                    };
                    let duration_seconds = args
                        .duration
                        .as_deref()
                        .map(|d| {
                            d.trim().parse::<u32>().map_err(|_| {
                                Failure::custom("invalid_request", format!("无效 duration '{d}'"))
                            })
                        })
                        .transpose()?;
                    let veo_req = VeoVideoRequest {
                        prompt: args.prompt,
                        platform,
                        image: args.image.map(|uri| VeoMediaInput {
                            gcs_uri: Some(uri),
                            ..Default::default()
                        }),
                        last_frame: args.last_frame.map(|uri| VeoMediaInput {
                            gcs_uri: Some(uri),
                            ..Default::default()
                        }),
                        aspect_ratio: args.ratio,
                        resolution: args.resolution,
                        duration_seconds,
                        ..Default::default()
                    };
                    if args.no_wait {
                        let model = if endpoint.model.trim().is_empty() {
                            "veo-2.0-generate-001".to_owned()
                        } else {
                            endpoint.model.clone()
                        };
                        let payload = build_veo_submit_payload(&veo_req).map_err(media_failure)?;
                        let (adapter, keys) =
                            VeoVideoAdapter::from_endpoint(&endpoint, &cred, platform)
                                .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model,
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let res = veo_generate_video(
                            &runner,
                            &endpoint,
                            &cred,
                            &veo_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&res, json_output)
                    }
                }
                VideoVendor::Seedance => {
                    let mut media = Vec::new();
                    if let Some(ff) = args.first_frame.or(args.image) {
                        media.push(ArkContentMedia {
                            url: ff,
                            role: ArkContentRole::FirstFrame,
                        });
                    }
                    if let Some(lf) = args.last_frame {
                        media.push(ArkContentMedia {
                            url: lf,
                            role: ArkContentRole::LastFrame,
                        });
                    }
                    for u in args.ref_images {
                        media.push(ArkContentMedia {
                            url: u,
                            role: ArkContentRole::ReferenceImage,
                        });
                    }
                    for u in args.ref_videos {
                        media.push(ArkContentMedia {
                            url: u,
                            role: ArkContentRole::ReferenceVideo,
                        });
                    }
                    for u in args.ref_audios {
                        media.push(ArkContentMedia {
                            url: u,
                            role: ArkContentRole::ReferenceAudio,
                        });
                    }
                    let duration = args
                        .duration
                        .as_deref()
                        .map(|d| {
                            d.trim().parse::<u32>().map_err(|_| {
                                Failure::custom("invalid_request", format!("无效 duration '{d}'"))
                            })
                        })
                        .transpose()?;
                    let sd_req = ArkSeedanceRequest {
                        prompt: args.prompt,
                        media,
                        duration,
                        resolution: args.resolution,
                        ratio: args.ratio,
                        seed: args.seed,
                        ..Default::default()
                    };
                    if args.no_wait {
                        let model = if endpoint.model.trim().is_empty() {
                            "doubao-seedance-1-0-pro-250528".to_owned()
                        } else {
                            endpoint.model.clone()
                        };
                        let payload =
                            build_ark_seedance_payload(&model, &sd_req).map_err(media_failure)?;
                        let (adapter, keys) = ArkSeedanceAdapter::from_endpoint(&endpoint, &cred)
                            .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model,
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let res = ark_seedance_generate(
                            &runner,
                            &endpoint,
                            &cred,
                            &sd_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&res, json_output)
                    }
                }
                VideoVendor::Minimax | VideoVendor::MinimaxCn => {
                    let duration = args
                        .duration
                        .as_deref()
                        .map(|d| {
                            d.trim().parse::<u32>().map_err(|_| {
                                Failure::custom("invalid_request", format!("无效 duration '{d}'"))
                            })
                        })
                        .transpose()?;
                    let mm_req = MiniMaxVideoRequest {
                        prompt: args.prompt,
                        first_frame_image: args.first_frame.or(args.image),
                        last_frame_image: args.last_frame,
                        duration,
                        resolution: args.resolution,
                        ..Default::default()
                    };
                    if args.no_wait {
                        let model = if endpoint.model.trim().is_empty() {
                            "MiniMax-Hailuo-2.3".to_owned()
                        } else {
                            endpoint.model.clone()
                        };
                        let payload =
                            build_minimax_video_payload(&model, &mm_req).map_err(media_failure)?;
                        let (adapter, keys) = MiniMaxVideoAdapter::from_endpoint(&endpoint, &cred)
                            .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model,
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let res = minimax_video_generate(
                            &runner,
                            &endpoint,
                            &cred,
                            &mm_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&res, json_output)
                    }
                }
                VideoVendor::Kling | VideoVendor::KlingIntl => {
                    let image = args.first_frame.or(args.image);
                    let mode = if image.is_some() {
                        KlingVideoMode::Image2Video
                    } else {
                        KlingVideoMode::Text2Video
                    };
                    let duration = args
                        .duration
                        .as_deref()
                        .map(KlingDuration::parse)
                        .transpose()
                        .map_err(media_failure)?;
                    let kl_req = KlingVideoRequest {
                        mode,
                        prompt: (!args.prompt.trim().is_empty()).then_some(args.prompt),
                        image,
                        image_tail: args.last_frame,
                        duration,
                        aspect_ratio: args.ratio,
                        ..Default::default()
                    };
                    if args.no_wait {
                        let model = if endpoint.model.trim().is_empty() {
                            "kling-v1-6".to_owned()
                        } else {
                            endpoint.model.clone()
                        };
                        let payload =
                            build_kling_video_payload(&model, &kl_req).map_err(media_failure)?;
                        let (adapter, keys) =
                            KlingVideoAdapter::from_endpoint(&endpoint, &cred, mode)
                                .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model,
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let res = kling_video_generate(
                            &runner,
                            &endpoint,
                            &cred,
                            &kl_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&res, json_output)
                    }
                }
                VideoVendor::Wanx => {
                    let duration = args
                        .duration
                        .as_deref()
                        .map(|d| {
                            d.trim().parse::<u32>().map_err(|_| {
                                Failure::custom("invalid_request", format!("无效 duration '{d}'"))
                            })
                        })
                        .transpose()?;
                    let wx_req = DashScopeWanxRequest {
                        prompt: args.prompt,
                        img_url: args.first_frame.or(args.image),
                        size: args.size,
                        duration,
                        seed: args.seed,
                        ..Default::default()
                    };
                    if args.no_wait {
                        let model = if endpoint.model.trim().is_empty() {
                            "wan2.1-t2v-turbo".to_owned()
                        } else {
                            endpoint.model.clone()
                        };
                        let payload =
                            build_dashscope_wanx_payload(&model, &wx_req).map_err(media_failure)?;
                        let (adapter, keys) = DashScopeWanxAdapter::from_endpoint(&endpoint, &cred)
                            .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model,
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let res = dashscope_wanx_generate(
                            &runner,
                            &endpoint,
                            &cred,
                            &wx_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&res, json_output)
                    }
                }
                VideoVendor::Runway => {
                    let image = args.first_frame.or(args.image);
                    let mode = if image.is_some() {
                        RunwayVideoMode::ImageToVideo
                    } else {
                        RunwayVideoMode::TextToVideo
                    };
                    let duration = args
                        .duration
                        .as_deref()
                        .map(|d| {
                            d.trim().parse::<u32>().map_err(|_| {
                                Failure::custom("invalid_request", format!("无效 duration '{d}'"))
                            })
                        })
                        .transpose()?;
                    let rw_req = RunwayVideoRequest {
                        mode,
                        prompt_text: (!args.prompt.trim().is_empty()).then_some(args.prompt),
                        prompt_image: image,
                        ratio: args.ratio,
                        duration,
                        seed: args.seed.and_then(|s| u64::try_from(s).ok()),
                    };
                    if args.no_wait {
                        let model = if endpoint.model.trim().is_empty() {
                            match mode {
                                RunwayVideoMode::ImageToVideo => "gen4_turbo".to_owned(),
                                RunwayVideoMode::TextToVideo => "veo3.1".to_owned(),
                            }
                        } else {
                            endpoint.model.clone()
                        };
                        let payload =
                            build_runway_video_payload(&model, &rw_req).map_err(media_failure)?;
                        let (adapter, keys) =
                            RunwayVideoAdapter::from_endpoint(&endpoint, &cred, mode)
                                .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model,
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let res = runway_video_generate(
                            &runner,
                            &endpoint,
                            &cred,
                            &rw_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&res, json_output)
                    }
                }
                VideoVendor::NewapiVideo => {
                    let path_mode = if args.compat_generations {
                        NewApiVideoPathMode::VideoGenerations
                    } else {
                        NewApiVideoPathMode::Videos
                    };
                    let na_req = NewApiVideoRequest {
                        prompt: args.prompt,
                        path_mode,
                        seconds: args.duration,
                        size: args.size,
                        ratio: args.ratio,
                        first_frame_image: args.first_frame.or(args.image),
                        last_frame_image: args.last_frame,
                    };
                    if args.no_wait {
                        let payload = build_newapi_video_payload(&endpoint.model, &na_req)
                            .map_err(media_failure)?;
                        let (adapter, keys) =
                            NewApiVideoAdapter::from_endpoint(&endpoint, &cred, path_mode)
                                .map_err(media_failure)?;
                        let (rec, _) = runner
                            .submit_and_persist(
                                &adapter,
                                &Engine3SubmitInput {
                                    idempotency_key: args.idempotency_key,
                                    session_id: args.session_id,
                                    parent_task_id: None,
                                    model: endpoint.model.clone(),
                                    route_id: endpoint.route_id.clone(),
                                    payload,
                                },
                                &keys,
                                cancel,
                            )
                            .await
                            .map_err(media_failure)?;
                        print_json_or_pretty(&rec, json_output)
                    } else {
                        let res = newapi_video_generate(
                            &runner,
                            &endpoint,
                            &cred,
                            &na_req,
                            args.idempotency_key,
                            args.session_id,
                            cancel,
                        )
                        .await
                        .map_err(media_failure)?;
                        print_json_or_pretty(&res, json_output)
                    }
                }
            }
        }
    }
}

async fn run_realtime(
    args: RealtimeArgs,
    artifact_store: Arc<WorkspaceMediaArtifactStore>,
    top_data_dir: Option<PathBuf>,
    store_cache: &mut Option<Arc<Store>>,
    json_output: bool,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    let (endpoint, cred) = resolve_endpoint_and_cred(
        args.vendor.as_str(),
        &args.model,
        &args.auth,
        top_data_dir,
        store_cache,
    )
    .await?;
    let input_audio_pcm16 = match args.audio_file {
        Some(path) => Some(
            std::fs::read(&path)
                .map_err(|e| Failure::custom("io_error", format!("读取输入音频失败: {e}")))?
                .into(),
        ),
        None => None,
    };
    let input_video_jpeg = match args.video_file {
        Some(path) => Some(
            std::fs::read(&path)
                .map_err(|e| Failure::custom("io_error", format!("读取输入视频帧失败: {e}")))?
                .into(),
        ),
        None => None,
    };
    let owner_id = format!("cli-{}", uuid::Uuid::now_v7());
    let req = RealtimeTurnRequest {
        text_prompt: args.prompt,
        input_audio_pcm16,
        input_video_jpeg,
        instructions: args.instructions,
        voice: args.voice,
        wrap_wav: args.wrap_wav,
        turn_timeout_ms: args.timeout_ms,
        ..Default::default()
    };

    let resp = match args.vendor {
        RealtimeVendor::Openai => openai_realtime_turn(
            artifact_store.as_ref(),
            &endpoint,
            &cred,
            &owner_id,
            &req,
            cancel,
        )
        .await
        .map_err(media_failure)?,
        RealtimeVendor::GeminiLive | RealtimeVendor::GeminiLiveVertex => gemini_live_turn(
            artifact_store.as_ref(),
            &endpoint,
            &cred,
            &owner_id,
            &req,
            cancel,
        )
        .await
        .map_err(media_failure)?,
    };
    print_json_or_pretty(&resp, json_output)
}

enum TaskAdapterDispatch {
    MiniMaxTts(MiniMaxAsyncTtsAdapter),
    VolcengineTts(VolcengineV3AsyncTtsAdapter),
    PartnerSeedAudio(PartnerSeedAudioAdapter),
    DashScopeParaformer(DashScopeParaformerAdapter),
    VolcengineAsr(VolcengineAsrAdapter),
    SunoMusic(SunoMusicAdapter),
    CodaMusicGateway(CodaMusicGatewayAdapter),
    SoraVideo(SoraVideoAdapter),
    VeoVideo(VeoVideoAdapter),
    ArkSeedance(ArkSeedanceAdapter),
    MiniMaxVideo(MiniMaxVideoAdapter),
    KlingVideo(KlingVideoAdapter),
    DashScopeWanx(DashScopeWanxAdapter),
    RunwayVideo(RunwayVideoAdapter),
    NewApiVideo(NewApiVideoAdapter),
}

impl Engine3Adapter for TaskAdapterDispatch {
    fn vendor(&self) -> &str {
        match self {
            Self::MiniMaxTts(a) => a.vendor(),
            Self::VolcengineTts(a) => a.vendor(),
            Self::PartnerSeedAudio(a) => a.vendor(),
            Self::DashScopeParaformer(a) => a.vendor(),
            Self::VolcengineAsr(a) => a.vendor(),
            Self::SunoMusic(a) => a.vendor(),
            Self::CodaMusicGateway(a) => a.vendor(),
            Self::SoraVideo(a) => a.vendor(),
            Self::VeoVideo(a) => a.vendor(),
            Self::ArkSeedance(a) => a.vendor(),
            Self::MiniMaxVideo(a) => a.vendor(),
            Self::KlingVideo(a) => a.vendor(),
            Self::DashScopeWanx(a) => a.vendor(),
            Self::RunwayVideo(a) => a.vendor(),
            Self::NewApiVideo(a) => a.vendor(),
        }
    }

    fn kind(&self) -> MediaKind {
        match self {
            Self::MiniMaxTts(a) => a.kind(),
            Self::VolcengineTts(a) => a.kind(),
            Self::PartnerSeedAudio(a) => a.kind(),
            Self::DashScopeParaformer(a) => a.kind(),
            Self::VolcengineAsr(a) => a.kind(),
            Self::SunoMusic(a) => a.kind(),
            Self::CodaMusicGateway(a) => a.kind(),
            Self::SoraVideo(a) => a.kind(),
            Self::VeoVideo(a) => a.kind(),
            Self::ArkSeedance(a) => a.kind(),
            Self::MiniMaxVideo(a) => a.kind(),
            Self::KlingVideo(a) => a.kind(),
            Self::DashScopeWanx(a) => a.kind(),
            Self::RunwayVideo(a) => a.kind(),
            Self::NewApiVideo(a) => a.kind(),
        }
    }

    fn build_submit(
        &self,
        input: &Engine3SubmitInput,
    ) -> Result<coda::media::engine2::Engine2Request, MediaError> {
        match self {
            Self::MiniMaxTts(a) => a.build_submit(input),
            Self::VolcengineTts(a) => a.build_submit(input),
            Self::PartnerSeedAudio(a) => a.build_submit(input),
            Self::DashScopeParaformer(a) => a.build_submit(input),
            Self::VolcengineAsr(a) => a.build_submit(input),
            Self::SunoMusic(a) => a.build_submit(input),
            Self::CodaMusicGateway(a) => a.build_submit(input),
            Self::SoraVideo(a) => a.build_submit(input),
            Self::VeoVideo(a) => a.build_submit(input),
            Self::ArkSeedance(a) => a.build_submit(input),
            Self::MiniMaxVideo(a) => a.build_submit(input),
            Self::KlingVideo(a) => a.build_submit(input),
            Self::DashScopeWanx(a) => a.build_submit(input),
            Self::RunwayVideo(a) => a.build_submit(input),
            Self::NewApiVideo(a) => a.build_submit(input),
        }
    }

    fn parse_submit_response(
        &self,
        status: u16,
        headers: &reqwest::header::HeaderMap,
        body: &serde_json::Value,
    ) -> Result<coda::media::engine3::SubmitOutcome, MediaError> {
        match self {
            Self::MiniMaxTts(a) => a.parse_submit_response(status, headers, body),
            Self::VolcengineTts(a) => a.parse_submit_response(status, headers, body),
            Self::PartnerSeedAudio(a) => a.parse_submit_response(status, headers, body),
            Self::DashScopeParaformer(a) => a.parse_submit_response(status, headers, body),
            Self::VolcengineAsr(a) => a.parse_submit_response(status, headers, body),
            Self::SunoMusic(a) => a.parse_submit_response(status, headers, body),
            Self::CodaMusicGateway(a) => a.parse_submit_response(status, headers, body),
            Self::SoraVideo(a) => a.parse_submit_response(status, headers, body),
            Self::VeoVideo(a) => a.parse_submit_response(status, headers, body),
            Self::ArkSeedance(a) => a.parse_submit_response(status, headers, body),
            Self::MiniMaxVideo(a) => a.parse_submit_response(status, headers, body),
            Self::KlingVideo(a) => a.parse_submit_response(status, headers, body),
            Self::DashScopeWanx(a) => a.parse_submit_response(status, headers, body),
            Self::RunwayVideo(a) => a.parse_submit_response(status, headers, body),
            Self::NewApiVideo(a) => a.parse_submit_response(status, headers, body),
        }
    }

    fn build_poll(
        &self,
        task: &MediaTaskRecord,
    ) -> Result<coda::media::engine2::Engine2Request, MediaError> {
        match self {
            Self::MiniMaxTts(a) => a.build_poll(task),
            Self::VolcengineTts(a) => a.build_poll(task),
            Self::PartnerSeedAudio(a) => a.build_poll(task),
            Self::DashScopeParaformer(a) => a.build_poll(task),
            Self::VolcengineAsr(a) => a.build_poll(task),
            Self::SunoMusic(a) => a.build_poll(task),
            Self::CodaMusicGateway(a) => a.build_poll(task),
            Self::SoraVideo(a) => a.build_poll(task),
            Self::VeoVideo(a) => a.build_poll(task),
            Self::ArkSeedance(a) => a.build_poll(task),
            Self::MiniMaxVideo(a) => a.build_poll(task),
            Self::KlingVideo(a) => a.build_poll(task),
            Self::DashScopeWanx(a) => a.build_poll(task),
            Self::RunwayVideo(a) => a.build_poll(task),
            Self::NewApiVideo(a) => a.build_poll(task),
        }
    }

    fn parse_poll_response(
        &self,
        status: u16,
        headers: &reqwest::header::HeaderMap,
        body: &serde_json::Value,
        task: &MediaTaskRecord,
    ) -> Result<coda::media::engine3::PollOutcome, MediaError> {
        match self {
            Self::MiniMaxTts(a) => a.parse_poll_response(status, headers, body, task),
            Self::VolcengineTts(a) => a.parse_poll_response(status, headers, body, task),
            Self::PartnerSeedAudio(a) => a.parse_poll_response(status, headers, body, task),
            Self::DashScopeParaformer(a) => a.parse_poll_response(status, headers, body, task),
            Self::VolcengineAsr(a) => a.parse_poll_response(status, headers, body, task),
            Self::SunoMusic(a) => a.parse_poll_response(status, headers, body, task),
            Self::CodaMusicGateway(a) => a.parse_poll_response(status, headers, body, task),
            Self::SoraVideo(a) => a.parse_poll_response(status, headers, body, task),
            Self::VeoVideo(a) => a.parse_poll_response(status, headers, body, task),
            Self::ArkSeedance(a) => a.parse_poll_response(status, headers, body, task),
            Self::MiniMaxVideo(a) => a.parse_poll_response(status, headers, body, task),
            Self::KlingVideo(a) => a.parse_poll_response(status, headers, body, task),
            Self::DashScopeWanx(a) => a.parse_poll_response(status, headers, body, task),
            Self::RunwayVideo(a) => a.parse_poll_response(status, headers, body, task),
            Self::NewApiVideo(a) => a.parse_poll_response(status, headers, body, task),
        }
    }

    fn build_cancel(
        &self,
        task: &MediaTaskRecord,
    ) -> Result<Option<coda::media::engine2::Engine2Request>, MediaError> {
        match self {
            Self::MiniMaxTts(a) => a.build_cancel(task),
            Self::VolcengineTts(a) => a.build_cancel(task),
            Self::PartnerSeedAudio(a) => a.build_cancel(task),
            Self::DashScopeParaformer(a) => a.build_cancel(task),
            Self::VolcengineAsr(a) => a.build_cancel(task),
            Self::SunoMusic(a) => a.build_cancel(task),
            Self::CodaMusicGateway(a) => a.build_cancel(task),
            Self::SoraVideo(a) => a.build_cancel(task),
            Self::VeoVideo(a) => a.build_cancel(task),
            Self::ArkSeedance(a) => a.build_cancel(task),
            Self::MiniMaxVideo(a) => a.build_cancel(task),
            Self::KlingVideo(a) => a.build_cancel(task),
            Self::DashScopeWanx(a) => a.build_cancel(task),
            Self::RunwayVideo(a) => a.build_cancel(task),
            Self::NewApiVideo(a) => a.build_cancel(task),
        }
    }

    fn parse_cancel_response(
        &self,
        status: u16,
        headers: &reqwest::header::HeaderMap,
        body: &serde_json::Value,
    ) -> Result<(), MediaError> {
        match self {
            Self::DashScopeWanx(a) => a.parse_cancel_response(status, headers, body),
            _ => Ok(()),
        }
    }

    fn parse_resolved_outputs(
        &self,
        status: u16,
        headers: &reqwest::header::HeaderMap,
        body: &serde_json::Value,
        context: &serde_json::Value,
        task: &MediaTaskRecord,
    ) -> Result<Vec<coda::media::engine3::MediaOutputSource>, MediaError> {
        match self {
            Self::CodaMusicGateway(a) => {
                a.parse_resolved_outputs(status, headers, body, context, task)
            }
            _ => Err(MediaError::new(
                coda::media::MediaErrorCode::DecodeError,
                "adapter does not support AdapterResolve outputs",
            )),
        }
    }
}

fn infer_tts_format_from_summary(summary: Option<&serde_json::Value>) -> TtsAudioFormat {
    summary
        .and_then(|s| {
            s.pointer("/audio_setting/format")
                .or_else(|| s.pointer("/req_params/audio_params/format"))
                .or_else(|| s.get("format"))
                .and_then(serde_json::Value::as_str)
                .and_then(TtsAudioFormat::parse)
        })
        .unwrap_or(TtsAudioFormat::Mp3)
}

fn build_task_adapter(
    kind: MediaKind,
    vendor: &str,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    resource_id: Option<&str>,
    request_summary: Option<&serde_json::Value>,
    allow_suno_proxy: bool,
) -> Result<(TaskAdapterDispatch, coda::redaction::CredentialSet), Failure> {
    let tts_format = infer_tts_format_from_summary(request_summary);
    match (kind, vendor.trim().to_ascii_lowercase().as_str()) {
        (MediaKind::Tts, "minimax" | "minimax-cn") => {
            let (a, keys) = MiniMaxAsyncTtsAdapter::from_endpoint(endpoint, cred, tts_format)
                .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::MiniMaxTts(a), keys))
        }
        (MediaKind::Tts, "volcengine") => {
            let (a, keys) =
                VolcengineV3AsyncTtsAdapter::from_endpoint(endpoint, cred, resource_id, tts_format)
                    .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::VolcengineTts(a), keys))
        }
        (MediaKind::Tts, "partner-seed-audio") => {
            let (a, keys) = PartnerSeedAudioAdapter::from_endpoint(endpoint, cred, tts_format)
                .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::PartnerSeedAudio(a), keys))
        }
        (MediaKind::Stt, "dashscope") => {
            let (a, keys) =
                DashScopeParaformerAdapter::from_endpoint(endpoint, cred).map_err(media_failure)?;
            Ok((TaskAdapterDispatch::DashScopeParaformer(a), keys))
        }
        (MediaKind::Stt, "volcengine") => {
            let (a, keys) = VolcengineAsrAdapter::from_endpoint(endpoint, cred, resource_id)
                .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::VolcengineAsr(a), keys))
        }
        (MediaKind::Music, "suno") => {
            let (a, keys) =
                SunoMusicAdapter::from_endpoint(endpoint, cred, false, allow_suno_proxy)
                    .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::SunoMusic(a), keys))
        }
        (MediaKind::Music, "coda-music-gateway" | "coda-music" | "treeapi") => {
            let (a, keys) = CodaMusicGatewayAdapter::from_endpoint(
                endpoint,
                cred,
                CodaMusicGatewayMode::CodaGateway,
            )
            .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::CodaMusicGateway(a), keys))
        }
        (MediaKind::Video, "openai" | "sora") => {
            let (a, keys) = SoraVideoAdapter::from_endpoint(
                endpoint,
                cred,
                SoraContentVariant::Video,
                None,
                None,
            )
            .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::SoraVideo(a), keys))
        }
        (MediaKind::Video, "veo") => {
            let (a, keys) = VeoVideoAdapter::from_endpoint(endpoint, cred, VeoPlatform::Gemini)
                .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::VeoVideo(a), keys))
        }
        (MediaKind::Video, "veo-vertex") => {
            let (a, keys) = VeoVideoAdapter::from_endpoint(endpoint, cred, VeoPlatform::Vertex)
                .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::VeoVideo(a), keys))
        }
        (MediaKind::Video, "seedance" | "ark") => {
            let (a, keys) =
                ArkSeedanceAdapter::from_endpoint(endpoint, cred).map_err(media_failure)?;
            Ok((TaskAdapterDispatch::ArkSeedance(a), keys))
        }
        (MediaKind::Video, "minimax" | "minimax-cn" | "hailuo" | "hailuo-cn") => {
            let (a, keys) =
                MiniMaxVideoAdapter::from_endpoint(endpoint, cred).map_err(media_failure)?;
            Ok((TaskAdapterDispatch::MiniMaxVideo(a), keys))
        }
        (MediaKind::Video, "kling" | "kling-intl" | "klingai") => {
            let mode = if request_summary.and_then(|s| s.get("image")).is_some() {
                KlingVideoMode::Image2Video
            } else {
                KlingVideoMode::Text2Video
            };
            let (a, keys) =
                KlingVideoAdapter::from_endpoint(endpoint, cred, mode).map_err(media_failure)?;
            Ok((TaskAdapterDispatch::KlingVideo(a), keys))
        }
        (MediaKind::Video, "wanx" | "dashscope") => {
            let (a, keys) =
                DashScopeWanxAdapter::from_endpoint(endpoint, cred).map_err(media_failure)?;
            Ok((TaskAdapterDispatch::DashScopeWanx(a), keys))
        }
        (MediaKind::Video, "runway") => {
            let (a, keys) =
                RunwayVideoAdapter::from_endpoint(endpoint, cred, RunwayVideoMode::ImageToVideo)
                    .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::RunwayVideo(a), keys))
        }
        (MediaKind::Video, "newapi-video" | "new-api" | "newapi") => {
            let (a, keys) =
                NewApiVideoAdapter::from_endpoint(endpoint, cred, NewApiVideoPathMode::Videos)
                    .map_err(media_failure)?;
            Ok((TaskAdapterDispatch::NewApiVideo(a), keys))
        }
        _ => Err(Failure::custom(
            "invalid_request",
            format!(
                "不支持的异步媒体任务组合: kind={}, vendor={vendor}",
                kind.as_str()
            ),
        )),
    }
}

async fn run_task(
    command: MediaTaskCommand,
    http: &MediaHttpClient,
    artifact_store: Arc<WorkspaceMediaArtifactStore>,
    top_data_dir: Option<PathBuf>,
    store_cache: &mut Option<Arc<Store>>,
    json_output: bool,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    match command {
        MediaTaskCommand::Status { id, data_dir } => {
            let store = ensure_store_opened(store_cache, data_dir, top_data_dir)?;
            let repo = SqliteMediaTaskRepository(store);
            let rec = repo
                .get_task(&id)
                .map_err(media_failure)?
                .ok_or_else(|| Failure::custom("not_found", format!("未找到媒体任务 '{id}'")))?;
            print_json_or_pretty(&rec, json_output)
        }
        MediaTaskCommand::Poll(poll_args) => {
            let TaskPollArgs {
                id,
                resource_id,
                allow_suno_proxy,
                allow_loopback_download,
                mut auth,
            } = *poll_args;
            let store =
                ensure_store_opened(store_cache, auth.data_dir.clone(), top_data_dir.clone())?;
            let repo = Arc::new(SqliteMediaTaskRepository(store));
            let task = repo
                .get_task(&id)
                .map_err(media_failure)?
                .ok_or_else(|| Failure::custom("not_found", format!("未找到媒体任务 '{id}'")))?;
            if auth.route.is_none() {
                auth.route = task.route_id.clone();
            }
            let (endpoint, cred) = resolve_endpoint_and_cred(
                &task.vendor,
                &task.model,
                &auth,
                top_data_dir,
                store_cache,
            )
            .await?;
            let allow_loopback =
                allow_loopback_download || is_loopback_base_url(&endpoint.base_url);
            let (adapter, keys) = build_task_adapter(
                task.media_kind,
                &task.vendor,
                &endpoint,
                &cred,
                resource_id.as_deref(),
                Some(&task.request_summary),
                allow_suno_proxy,
            )?;
            let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                .with_allow_http_loopback_download(allow_loopback);
            let updated = runner
                .poll_until_done(&adapter, task, &keys, cancel)
                .await
                .map_err(media_failure)?;
            print_json_or_pretty(&updated, json_output)
        }
        MediaTaskCommand::Resume(resume_args) => {
            let TaskResumeArgs {
                vendor,
                kind,
                session_id,
                resource_id,
                allow_suno_proxy,
                allow_loopback_download,
                auth,
            } = *resume_args;
            let parsed_kind = MediaKind::parse(&kind).ok_or_else(|| {
                Failure::custom("invalid_request", format!("无效的媒体任务类型 '{kind}'"))
            })?;
            let store =
                ensure_store_opened(store_cache, auth.data_dir.clone(), top_data_dir.clone())?;
            let repo = Arc::new(SqliteMediaTaskRepository(store));
            let (endpoint, cred) =
                resolve_endpoint_and_cred(&vendor, "", &auth, top_data_dir, store_cache).await?;
            let allow_loopback =
                allow_loopback_download || is_loopback_base_url(&endpoint.base_url);
            let (adapter, keys) = build_task_adapter(
                parsed_kind,
                &vendor,
                &endpoint,
                &cred,
                resource_id.as_deref(),
                None,
                allow_suno_proxy,
            )?;
            let runner = Engine3Runner::new(http.clone(), repo, artifact_store)
                .with_allow_http_loopback_download(allow_loopback);
            let results = runner
                .resume_pending_tasks(&adapter, session_id.as_deref(), &keys, cancel)
                .await
                .map_err(media_failure)?;
            let summary: Vec<serde_json::Value> = results
                .into_iter()
                .map(|r| match r {
                    Ok(rec) => json!({ "ok": true, "task": rec }),
                    Err(err) => json!({
                        "ok": false,
                        "code": err.code.as_str(),
                        "message": err.message,
                    }),
                })
                .collect();
            print_json_or_pretty(&summary, json_output)
        }
        MediaTaskCommand::Cancel(cancel_args) => {
            let TaskCancelArgs {
                id,
                reason,
                resource_id,
                allow_suno_proxy,
                mut auth,
            } = *cancel_args;
            let store =
                ensure_store_opened(store_cache, auth.data_dir.clone(), top_data_dir.clone())?;
            let repo = Arc::new(SqliteMediaTaskRepository(store));
            let task = repo
                .get_task(&id)
                .map_err(media_failure)?
                .ok_or_else(|| Failure::custom("not_found", format!("未找到媒体任务 '{id}'")))?;
            if auth.route.is_none() {
                auth.route = task.route_id.clone();
            }
            let (endpoint, cred) = resolve_endpoint_and_cred(
                &task.vendor,
                &task.model,
                &auth,
                top_data_dir,
                store_cache,
            )
            .await?;
            let (adapter, keys) = build_task_adapter(
                task.media_kind,
                &task.vendor,
                &endpoint,
                &cred,
                resource_id.as_deref(),
                Some(&task.request_summary),
                allow_suno_proxy,
            )?;
            let runner = Engine3Runner::new(http.clone(), repo, artifact_store);
            let cancelled = runner
                .cancel_task(&adapter, &id, reason.as_deref(), &keys, cancel)
                .await
                .map_err(media_failure)?;
            print_json_or_pretty(&cancelled, json_output)
        }
    }
}

fn infer_audio_mime(filename: &str) -> &'static str {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "aif" | "aiff" => "audio/aiff",
        "caf" => "audio/x-caf",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    }
}

fn read_image_file(path: &Path) -> Result<ImageInputBytes, Failure> {
    let bytes = std::fs::read(path).map_err(|e| {
        Failure::custom(
            "io_error",
            format!("无法读取图像文件 {}: {e}", path.display()),
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("image.png")
        .to_owned();
    Ok(ImageInputBytes {
        filename,
        bytes: bytes.into(),
    })
}
