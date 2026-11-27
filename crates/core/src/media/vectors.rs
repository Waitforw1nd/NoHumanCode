//! D.2 Vectors & Rerank adapters (Engine 2):
//! - OpenAI Embeddings (`/v1/embeddings`)
//! - Gemini Embeddings (`:embedContent` & `:batchEmbedContents` + client L2 norm `<3072`)
//! - Jina Rerank (`/v1/rerank`)

use super::engine2::{
    Engine2Request, MediaHttpClient, check_standard_business_status, current_unix_i64,
};
use super::{
    MediaCredential, MediaEndpointConfig, MediaError, MediaErrorCode, normalize_gemini_model_id,
    validate_media_url,
};
use crate::redaction::CredentialSet;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Single(String),
    Batch(Vec<String>),
}

impl EmbeddingInput {
    pub fn as_slice(&self) -> Vec<&str> {
        match self {
            Self::Single(s) => vec![s.as_str()],
            Self::Batch(items) => items.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingEncodingFormat {
    Float,
    Base64,
}

impl EmbeddingEncodingFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Float => "float",
            Self::Base64 => "base64",
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenAiEmbedRequest {
    pub input: EmbeddingInput,
    pub encoding_format: Option<EmbeddingEncodingFormat>,
    pub dimensions: Option<u32>,
    pub user: Option<String>,
}

#[derive(Clone, Debug)]
pub struct GeminiEmbedRequest {
    pub inputs: Vec<String>,
    pub task_type: Option<String>,
    pub title: Option<String>,
    pub output_dimensionality: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingVector {
    pub index: usize,
    pub values: Vec<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingUsage {
    pub prompt_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmbedResponse {
    pub model: String,
    pub embeddings: Vec<EmbeddingVector>,
    pub usage: Option<EmbeddingUsage>,
}

#[derive(Clone, Debug)]
pub struct JinaRerankRequest {
    pub query: String,
    pub documents: Vec<String>,
    pub top_n: Option<usize>,
    pub return_documents: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RerankResult {
    pub index: usize,
    pub relevance_score: f64,
    pub document: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RerankResponse {
    pub model: String,
    pub results: Vec<RerankResult>,
    pub total_tokens: Option<u64>,
}

pub fn build_openai_embeddings_url(
    base_url: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/v1")
    {
        Ok(format!("{trimmed}/embeddings"))
    } else {
        Ok(format!("{trimmed}/v1/embeddings"))
    }
}

pub fn build_gemini_v1beta_base(
    base_url: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let path_lower = url.path().trim_end_matches('/').to_ascii_lowercase();
    if embedded_proxy {
        if path_lower.ends_with("/gemini") {
            Ok(trimmed.to_owned())
        } else {
            Ok(format!("{trimmed}/gemini"))
        }
    } else if path_lower.ends_with("/v1beta") {
        Ok(trimmed.to_owned())
    } else {
        Ok(format!("{trimmed}/v1beta"))
    }
}

pub fn build_jina_rerank_url(base_url: &str, embedded_proxy: bool) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let path_lower = url.path().trim_end_matches('/').to_ascii_lowercase();
    if embedded_proxy {
        if path_lower.ends_with("/rerank") {
            Ok(trimmed.to_owned())
        } else {
            Ok(format!("{trimmed}/rerank"))
        }
    } else if path_lower.ends_with("/v1") {
        Ok(format!("{trimmed}/rerank"))
    } else {
        Ok(format!("{trimmed}/v1/rerank"))
    }
}

pub async fn openai_embed(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &OpenAiEmbedRequest,
    cancel: &CancellationToken,
) -> Result<EmbedResponse, MediaError> {
    if endpoint.model.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "embedding model must not be empty",
        ));
    }
    let texts = req.input.as_slice();
    if texts.is_empty() {
        return Err(MediaError::invalid_request(
            "embedding input must not be empty",
        ));
    }

    let url = build_openai_embeddings_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let mut body = json!({
        "model": endpoint.model,
        "input": req.input,
    });
    if let Some(fmt) = req.encoding_format {
        body["encoding_format"] = json!(fmt.as_str());
    }
    if let Some(dim) = req.dimensions {
        body["dimensions"] = json!(dim);
    }
    if let Some(user) = &req.user {
        body["user"] = json!(user);
    }

    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    parse_openai_embed_response(&endpoint.model, &resp.body, &keys)
}

fn parse_openai_embed_response(
    fallback_model: &str,
    body: &Value,
    keys: &CredentialSet,
) -> Result<EmbedResponse, MediaError> {
    let data = body.get("data").and_then(Value::as_array).ok_or_else(|| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            "missing data[] in embeddings response",
        )
    })?;

    let mut vectors = Vec::with_capacity(data.len());
    for (fallback_idx, item) in data.iter().enumerate() {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(fallback_idx);
        let raw_emb = item.get("embedding").ok_or_else(|| {
            MediaError::new(MediaErrorCode::DecodeError, "missing embedding field")
        })?;
        let values = match raw_emb {
            Value::Array(arr) => decode_float_array(arr)?,
            Value::String(b64) => decode_base64_le_f32(b64)?,
            _ => {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "unsupported embedding encoding type",
                ));
            }
        };
        vectors.push(EmbeddingVector { index, values });
    }
    vectors.sort_by_key(|v| v.index);

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model)
        .to_owned();
    let usage = body.get("usage").map(|u| EmbeddingUsage {
        prompt_tokens: u.get("prompt_tokens").and_then(Value::as_u64),
        total_tokens: u.get("total_tokens").and_then(Value::as_u64),
    });

    let _ = keys;
    Ok(EmbedResponse {
        model,
        embeddings: vectors,
        usage,
    })
}

fn decode_float_array(arr: &[Value]) -> Result<Vec<f32>, MediaError> {
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let f = v.as_f64().ok_or_else(|| {
            MediaError::new(MediaErrorCode::DecodeError, "non-numeric embedding value")
        })?;
        let f32_val = f as f32;
        if !f32_val.is_finite() {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "non-finite embedding float value",
            ));
        }
        out.push(f32_val);
    }
    Ok(out)
}

pub fn decode_base64_le_f32(b64: &str) -> Result<Vec<f32>, MediaError> {
    let bytes = STANDARD.decode(b64.trim()).map_err(|e| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            format!("invalid base64 embedding payload: {e}"),
        )
    })?;
    if bytes.len() % 4 != 0 {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "base64 embedding byte length is not a multiple of 4",
        ));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.as_chunks::<4>().0 {
        let val = f32::from_le_bytes(*chunk);
        if !val.is_finite() {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "non-finite f32 value in base64 embedding",
            ));
        }
        out.push(val);
    }
    Ok(out)
}

pub async fn gemini_embed(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &GeminiEmbedRequest,
    cancel: &CancellationToken,
) -> Result<EmbedResponse, MediaError> {
    if req.inputs.is_empty() {
        return Err(MediaError::invalid_request(
            "Gemini embedding inputs must not be empty",
        ));
    }
    let model_id = normalize_gemini_model_id(&endpoint.model)?;
    let api_base = build_gemini_v1beta_base(&endpoint.base_url, endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let is_batch = req.inputs.len() > 1;
    let url = if is_batch {
        format!("{api_base}/models/{model_id}:batchEmbedContents")
    } else {
        format!("{api_base}/models/{model_id}:embedContent")
    };

    let build_item = |text: &str| {
        let mut item = json!({
            "model": format!("models/{model_id}"),
            "content": {
                "parts": [{ "text": text }]
            }
        });
        if let Some(task_type) = &req.task_type {
            item["taskType"] = json!(task_type);
        }
        if let Some(title) = &req.title {
            item["title"] = json!(title);
        }
        if let Some(dim) = req.output_dimensionality {
            item["outputDimensionality"] = json!(dim);
        }
        item
    };

    let body = if is_batch {
        let requests: Vec<Value> = req.inputs.iter().map(|s| build_item(s)).collect();
        json!({ "requests": requests })
    } else {
        build_item(&req.inputs[0])
    };

    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    let mut vectors = if is_batch {
        let arr = resp
            .body
            .get("embeddings")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing embeddings[] in Gemini batchEmbedContents response",
                )
            })?;
        let mut out = Vec::with_capacity(arr.len());
        for (idx, item) in arr.iter().enumerate() {
            let vals = item
                .get("values")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        "missing values[] in Gemini embedding item",
                    )
                })?;
            out.push(EmbeddingVector {
                index: idx,
                values: decode_float_array(vals)?,
            });
        }
        out
    } else {
        let vals = resp
            .body
            .pointer("/embedding/values")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing embedding.values in Gemini embedContent response",
                )
            })?;
        vec![EmbeddingVector {
            index: 0,
            values: decode_float_array(vals)?,
        }]
    };

    if needs_gemini_l2_normalization(model_id, req.output_dimensionality) {
        for vec_item in &mut vectors {
            l2_normalize_in_place(&mut vec_item.values);
        }
    }

    Ok(EmbedResponse {
        model: model_id.to_owned(),
        embeddings: vectors,
        usage: None,
    })
}

fn needs_gemini_l2_normalization(model_id: &str, output_dimensionality: Option<u32>) -> bool {
    let lower = model_id.to_ascii_lowercase();
    let is_truncatable_model = lower == "embedding-001" || lower == "gemini-embedding-001";
    is_truncatable_model && output_dimensionality.is_some_and(|dim| dim < 3072)
}

pub fn l2_normalize_in_place(values: &mut [f32]) {
    let sum_sq: f64 = values.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    let norm = sum_sq.sqrt();
    if norm > 0.0 && norm.is_finite() {
        for v in values.iter_mut() {
            *v = (f64::from(*v) / norm) as f32;
        }
    }
}

pub async fn jina_rerank(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &JinaRerankRequest,
    cancel: &CancellationToken,
) -> Result<RerankResponse, MediaError> {
    if endpoint.model.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "rerank model must not be empty",
        ));
    }
    if req.query.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "rerank query must not be empty",
        ));
    }
    if req.documents.is_empty() {
        return Err(MediaError::invalid_request(
            "rerank documents must not be empty",
        ));
    }

    let url = build_jina_rerank_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let mut body = json!({
        "model": endpoint.model,
        "query": req.query,
        "documents": req.documents,
        "return_documents": req.return_documents,
    });
    if let Some(top_n) = req.top_n {
        body["top_n"] = json!(top_n);
    }

    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    let raw_results = resp
        .body
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing results[] in Jina rerank response",
            )
        })?;

    let mut results = Vec::with_capacity(raw_results.len());
    for item in raw_results {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| MediaError::new(MediaErrorCode::DecodeError, "missing result.index"))?;
        let relevance_score = item
            .get("relevance_score")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing result.relevance_score",
                )
            })?;
        let document = match item.get("document") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Object(obj)) => obj
                .get("text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            _ => None,
        };
        results.push(RerankResult {
            index,
            relevance_score,
            document,
        });
    }

    let model = resp
        .body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&endpoint.model)
        .to_owned();
    let total_tokens = resp
        .body
        .pointer("/usage/total_tokens")
        .and_then(Value::as_u64);

    Ok(RerankResponse {
        model,
        results,
        total_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        extract::Query,
        http::{HeaderMap as AxumHeaders, StatusCode},
        routing::post,
    };
    use std::collections::HashMap;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn openai_embeddings_float_and_base64_decoding_and_index_sorting() {
        let app = Router::new().route(
            "/v1/embeddings",
            post(
                |headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(
                        headers.get("authorization").and_then(|v| v.to_str().ok()),
                        Some("Bearer sk-oai-test")
                    );
                    if body.get("encoding_format").and_then(Value::as_str) == Some("base64") {
                        let mut b0 = Vec::new();
                        b0.extend_from_slice(&1.5_f32.to_le_bytes());
                        b0.extend_from_slice(&(-2.25_f32).to_le_bytes());
                        let mut b1 = Vec::new();
                        b1.extend_from_slice(&0.5_f32.to_le_bytes());
                        b1.extend_from_slice(&3.0_f32.to_le_bytes());
                        return (
                            StatusCode::OK,
                            axum::Json(json!({
                                "model": "text-embedding-3-small",
                                "data": [
                                    {"index": 1, "embedding": STANDARD.encode(b1)},
                                    {"index": 0, "embedding": STANDARD.encode(b0)}
                                ],
                                "usage": {"prompt_tokens": 8, "total_tokens": 8}
                            })),
                        );
                    }
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "model": "text-embedding-3-small",
                            "data": [{"index": 0, "embedding": [0.25, -0.5]}],
                            "usage": {"prompt_tokens": 4, "total_tokens": 4}
                        })),
                    )
                },
            ),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let endpoint =
            MediaEndpointConfig::new("openai", format!("http://{addr}"), "text-embedding-3-small");
        let cred = MediaCredential::bearer("sk-oai-test").unwrap();
        let cancel = CancellationToken::new();

        // Single float
        let res_float = openai_embed(
            &http,
            &endpoint,
            &cred,
            &OpenAiEmbedRequest {
                input: EmbeddingInput::Single("hello".into()),
                encoding_format: Some(EmbeddingEncodingFormat::Float),
                dimensions: Some(2),
                user: None,
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(res_float.embeddings.len(), 1);
        assert_eq!(res_float.embeddings[0].values, vec![0.25, -0.5]);

        // Batch base64 out-of-order sorted by index
        let res_b64 = openai_embed(
            &http,
            &endpoint,
            &cred,
            &OpenAiEmbedRequest {
                input: EmbeddingInput::Batch(vec!["a".into(), "b".into()]),
                encoding_format: Some(EmbeddingEncodingFormat::Base64),
                dimensions: Some(2),
                user: None,
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(res_b64.embeddings[0].values, vec![1.5, -2.25]);
        assert_eq!(res_b64.embeddings[1].values, vec![0.5, 3.0]);
    }

    #[tokio::test]
    async fn gemini_embed_and_batch_embed_enforce_header_auth_model_field_and_l2_norm() {
        let app = Router::new()
            .route(
                "/v1beta/models/gemini-embedding-001:embedContent",
                post(
                    |headers: AxumHeaders,
                     Query(q): Query<HashMap<String, String>>,
                     axum::Json(_body): axum::Json<Value>| async move {
                        assert!(!q.contains_key("key"), "must never send ?key= query param");
                        assert_eq!(
                            headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                            Some("goog-emb-secret")
                        );
                        (
                            StatusCode::OK,
                            axum::Json(json!({"embedding": {"values": [3.0, 4.0]}})),
                        )
                    },
                ),
            )
            .route(
                "/v1beta/models/gemini-embedding-001:batchEmbedContents",
                post(
                    |headers: AxumHeaders,
                     Query(q): Query<HashMap<String, String>>,
                     axum::Json(body): axum::Json<Value>| async move {
                        assert!(!q.contains_key("key"));
                        assert_eq!(
                            headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                            Some("goog-emb-secret")
                        );
                        let reqs = body["requests"].as_array().unwrap();
                        assert_eq!(reqs.len(), 2);
                        for r in reqs {
                            assert_eq!(r["model"], "models/gemini-embedding-001");
                        }
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "embeddings": [
                                    {"values": [3.0, 4.0]},
                                    {"values": [0.0, 5.0]}
                                ]
                            })),
                        )
                    },
                ),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let endpoint =
            MediaEndpointConfig::new("gemini", format!("http://{addr}"), "gemini-embedding-001");
        let cred = MediaCredential::x_goog_api_key("goog-emb-secret").unwrap();
        let cancel = CancellationToken::new();

        // Single with < 3072 triggers L2 normalization ([3, 4] -> [0.6, 0.8])
        let single = gemini_embed(
            &http,
            &endpoint,
            &cred,
            &GeminiEmbedRequest {
                inputs: vec!["hello".into()],
                task_type: Some("RETRIEVAL_DOCUMENT".into()),
                title: None,
                output_dimensionality: Some(768),
            },
            &cancel,
        )
        .await
        .unwrap();
        assert!((single.embeddings[0].values[0] - 0.6).abs() < 1e-5);
        assert!((single.embeddings[0].values[1] - 0.8).abs() < 1e-5);

        // Batch with 3072 preserves raw un-normalized values
        let batch_raw = gemini_embed(
            &http,
            &endpoint,
            &cred,
            &GeminiEmbedRequest {
                inputs: vec!["a".into(), "b".into()],
                task_type: None,
                title: None,
                output_dimensionality: Some(3072),
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(batch_raw.embeddings[0].values, vec![3.0, 4.0]);
        assert_eq!(batch_raw.embeddings[1].values, vec![0.0, 5.0]);
    }

    #[tokio::test]
    async fn jina_rerank_parses_both_document_shapes_and_scrubs_errors() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_c = calls.clone();
        let app = Router::new()
            .route(
                "/v1/rerank",
                post(move |headers: AxumHeaders, axum::Json(body): axum::Json<Value>| {
                    let attempt = calls_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        if auth == "Bearer jina-bad-key" {
                            return (
                                StatusCode::UNAUTHORIZED,
                                axum::Json(json!({
                                    "error": {"message": "invalid key jina-bad-key"}
                                })),
                            );
                        }
                        if attempt == 0 {
                            return (
                                StatusCode::TOO_MANY_REQUESTS,
                                axum::Json(json!({"error": {"message": "slow down"}})),
                            );
                        }
                        assert_eq!(body["top_n"], 2);
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "model": "jina-reranker-v2-base-multilingual",
                                "results": [
                                    {"index": 1, "relevance_score": 0.92, "document": {"text": "rust async"}},
                                    {"index": 0, "relevance_score": 0.41, "document": "python script"}
                                ],
                                "usage": {"total_tokens": 42}
                            })),
                        )
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let endpoint = MediaEndpointConfig::new(
            "jina",
            format!("http://{addr}"),
            "jina-reranker-v2-base-multilingual",
        );
        let cred = MediaCredential::bearer("jina-ok-key").unwrap();
        let cancel = CancellationToken::new();

        let reranked = jina_rerank(
            &http,
            &endpoint,
            &cred,
            &JinaRerankRequest {
                query: "rust tokio".into(),
                documents: vec!["python script".into(), "rust async".into()],
                top_n: Some(2),
                return_documents: true,
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(reranked.results.len(), 2);
        assert_eq!(reranked.results[0].document.as_deref(), Some("rust async"));
        assert_eq!(
            reranked.results[1].document.as_deref(),
            Some("python script")
        );

        let bad_cred = MediaCredential::bearer("jina-bad-key").unwrap();
        let err = jina_rerank(
            &http,
            &endpoint,
            &bad_cred,
            &JinaRerankRequest {
                query: "rust".into(),
                documents: vec!["doc".into()],
                top_n: None,
                return_documents: false,
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::AuthRejected);
        assert!(!err.to_string().contains("jina-bad-key"));
    }

    #[test]
    fn embedded_proxy_url_builders_target_gemini_and_rerank_routes() {
        assert_eq!(
            build_gemini_v1beta_base("http://127.0.0.1:4100/model-proxy/run-1/media", true)
                .unwrap(),
            "http://127.0.0.1:4100/model-proxy/run-1/media/gemini"
        );
        assert_eq!(
            build_gemini_v1beta_base("http://127.0.0.1:4100/model-proxy/run-1/media/gemini", true)
                .unwrap(),
            "http://127.0.0.1:4100/model-proxy/run-1/media/gemini"
        );
        assert_eq!(
            build_gemini_v1beta_base("https://generativelanguage.googleapis.com", false).unwrap(),
            "https://generativelanguage.googleapis.com/v1beta"
        );

        assert_eq!(
            build_jina_rerank_url("http://127.0.0.1:4100/model-proxy/run-1/media", true).unwrap(),
            "http://127.0.0.1:4100/model-proxy/run-1/media/rerank"
        );
        assert_eq!(
            build_jina_rerank_url("http://127.0.0.1:4100/model-proxy/run-1/media/rerank", true)
                .unwrap(),
            "http://127.0.0.1:4100/model-proxy/run-1/media/rerank"
        );
        assert_eq!(
            build_jina_rerank_url("https://api.jina.ai", false).unwrap(),
            "https://api.jina.ai/v1/rerank"
        );
    }
}
