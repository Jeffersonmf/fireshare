//! The HTTP side: a small API plus a browser page, behind a shared token.
//!
//! | Route | Auth | What it does |
//! |---|---|---|
//! | `GET /` | no | Browser page (asks for the token itself) |
//! | `GET /health` | no | `ok` |
//! | `GET /api/files` | yes | Lists every shared file |
//! | `GET /api/files/{path}` | yes | Streams one file, with its SHA-256 |
//! | `PUT /api/files/{path}` | yes | Streams one file in; `?overwrite=true` to replace |
//!
//! The token goes in `Authorization: Bearer <token>`, or in `?token=` for
//! plain browser download links.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::StreamExt;
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use crate::paths::{self, TMP_DIR};
use crate::{Entry, SHA256_HEADER, Stored};

/// What a server shares and how.
#[derive(Debug, Clone)]
pub struct Config {
    /// The shared directory. Created if missing.
    pub root: PathBuf,
    pub token: String,
    /// Largest accepted upload, in bytes.
    pub max_bytes: u64,
}

/// Builds the router. Prepares the shared directory and its temp area.
pub async fn router(cfg: Config) -> anyhow::Result<Router> {
    fs::create_dir_all(cfg.root.join(TMP_DIR)).await?;
    let cfg = Arc::new(Config {
        root: fs::canonicalize(&cfg.root).await?,
        ..cfg
    });
    let api = Router::new()
        .route("/files", get(list))
        .route("/files/{*path}", get(download).put(upload))
        .route_layer(middleware::from_fn_with_state(cfg.clone(), auth));
    Ok(Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/health", get(|| async { "ok" }))
        .nest("/api", api)
        // Uploads are streamed to disk and bounded by max_bytes instead.
        .layer(DefaultBodyLimit::disable())
        .with_state(cfg))
}

const INDEX_HTML: &str = include_str!("index.html");

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl ApiError {
    fn new(code: StatusCode, msg: impl Into<String>) -> Self {
        Self(code, msg.into())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::new(StatusCode::NOT_FOUND, "not found"),
            _ => {
                tracing::error!(error = %e, "io failure");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "io failure")
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

async fn auth(State(cfg): State<Arc<Config>>, req: Request, next: Next) -> Response {
    if presented_token(&req).is_some_and(|t| constant_time_eq(t.as_bytes(), cfg.token.as_bytes())) {
        return next.run(req).await;
    }
    ApiError::new(StatusCode::UNAUTHORIZED, "missing or wrong token").into_response()
}

fn presented_token(req: &Request) -> Option<String> {
    if let Some(v) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        return v.strip_prefix("Bearer ").map(str::to_owned);
    }
    req.uri().query()?.split('&').find_map(|kv| {
        kv.strip_prefix("token=")
            .map(|v| percent_decode_str(v).decode_utf8_lossy().into_owned())
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn resolve(cfg: &Config, remote: &str) -> Result<PathBuf, ApiError> {
    paths::clean(remote)
        .map(|rel| cfg.root.join(rel))
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e))
}

async fn list(State(cfg): State<Arc<Config>>) -> Result<Json<Vec<Entry>>, ApiError> {
    let root = cfg.root.clone();
    let entries = tokio::task::spawn_blocking(move || walk(&root))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(entries))
}

fn walk(root: &FsPath) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() != 1 || e.file_name() != TMP_DIR);
    for e in walker {
        let e = e.map_err(std::io::Error::other)?;
        if !e.file_type().is_file() {
            continue;
        }
        let meta = e.metadata().map_err(std::io::Error::other)?;
        let rel = e.path().strip_prefix(root).map_err(std::io::Error::other)?;
        out.push(Entry {
            path: paths::to_remote(rel),
            size: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs()),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

async fn download(
    State(cfg): State<Arc<Config>>,
    Path(remote): Path<String>,
) -> Result<Response, ApiError> {
    // The real path must stay inside the shared directory: a symlinked file
    // or directory pointing elsewhere is treated as missing.
    let path = fs::canonicalize(resolve(&cfg, &remote)?).await?;
    let meta = fs::metadata(&path).await?;
    if !path.starts_with(&cfg.root) || path.starts_with(cfg.root.join(TMP_DIR)) || !meta.is_file() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not found"));
    }
    let sha = sha256_file(&path).await?;
    let file = fs::File::open(&path).await?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.len()));
    headers.insert(
        SHA256_HEADER,
        HeaderValue::from_str(&sha).expect("hex is a valid header"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename*=UTF-8''{}",
        percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC)
    )) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok((headers, Body::from_stream(ReaderStream::new(file))).into_response())
}

#[derive(Deserialize)]
struct UploadQuery {
    #[serde(default)]
    overwrite: bool,
}

async fn upload(
    State(cfg): State<Arc<Config>>,
    Path(remote): Path<String>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Stored>), ApiError> {
    let dest = resolve(&cfg, &remote)?;
    if !q.overwrite && fs::symlink_metadata(&dest).await.is_ok() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "file exists; use overwrite=true",
        ));
    }
    let expected = headers
        .get(SHA256_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase);

    let tmp = cfg.root.join(TMP_DIR).join(crate::random_token());
    let result = receive(&cfg, body, &tmp, expected.as_deref()).await;
    let (size, sha) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_file(&tmp).await;
            return Err(e);
        }
    };
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).await?;
    }
    // Same filesystem: the file appears complete or not at all.
    if let Err(e) = fs::rename(&tmp, &dest).await {
        let _ = fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    tracing::info!(path = %remote, size, sha256 = %sha, "stored");
    Ok((
        StatusCode::CREATED,
        Json(Stored {
            path: remote,
            size,
            sha256: sha,
        }),
    ))
}

/// Streams the body into `tmp`, hashing on the way, and checks size and hash.
async fn receive(
    cfg: &Config,
    body: Body,
    tmp: &FsPath,
    expected: Option<&str>,
) -> Result<(u64, String), ApiError> {
    let mut file = fs::File::create(tmp).await?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, format!("body: {e}")))?;
        size += chunk.len() as u64;
        if size > cfg.max_bytes {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("larger than {} bytes", cfg.max_bytes),
            ));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;
    let sha = hex::encode(hasher.finalize());
    if let Some(want) = expected
        && want != sha
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("sha256 mismatch: sent {want}, received {sha}"),
        ));
    }
    Ok((size, sha))
}

/// Hex SHA-256 of a file's content.
pub async fn sha256_file(path: &FsPath) -> std::io::Result<String> {
    let mut file = fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
