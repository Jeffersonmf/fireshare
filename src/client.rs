//! The CLI side of the API in [`crate::server`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::server::sha256_file;
use crate::{Entry, SHA256_HEADER, Stored, paths};

/// Characters escaped inside one path segment of the URL.
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

/// A file written locally by [`Client::download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    /// `base` is the server root, e.g. `http://192.168.0.10:7878`.
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    fn file_url(&self, remote: &str) -> String {
        let encoded: Vec<String> = remote
            .split('/')
            .map(|s| utf8_percent_encode(s, SEGMENT).to_string())
            .collect();
        format!("{}/api/files/{}", self.base, encoded.join("/"))
    }

    pub async fn list(&self) -> Result<Vec<Entry>> {
        let resp = self
            .http
            .get(format!("{}/api/files", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("connecting to the server")?;
        Ok(check(resp).await?.json().await?)
    }

    /// Uploads one local file as `remote`. The server checks the SHA-256.
    pub async fn upload(&self, local: &Path, remote: &str, overwrite: bool) -> Result<Stored> {
        paths::clean(remote).map_err(|e| anyhow::anyhow!("{remote}: {e}"))?;
        let sha = sha256_file(local)
            .await
            .with_context(|| format!("reading {}", local.display()))?;
        let size = fs::metadata(local).await?.len();
        let file = fs::File::open(local).await?;
        let resp = self
            .http
            .put(format!("{}?overwrite={overwrite}", self.file_url(remote)))
            .bearer_auth(&self.token)
            .header(SHA256_HEADER, &sha)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
            .send()
            .await
            .context("connecting to the server")?;
        let stored: Stored = check(resp).await?.json().await?;
        if stored.sha256 != sha {
            bail!(
                "{remote}: server stored sha256 {}, local is {sha}",
                stored.sha256
            );
        }
        Ok(stored)
    }

    /// Uploads files and directories (recursively). A directory keeps its
    /// own name on the server: `send ./docs` creates `docs/...`. `prefix`
    /// puts everything under one remote directory.
    pub async fn send(
        &self,
        locals: &[PathBuf],
        prefix: Option<&str>,
        overwrite: bool,
    ) -> Result<Vec<Stored>> {
        let mut plan = Vec::new();
        for local in locals {
            let meta = fs::metadata(local)
                .await
                .with_context(|| format!("{}", local.display()))?;
            let base = local
                .file_name()
                .with_context(|| format!("{} has no file name", local.display()))?;
            if meta.is_file() {
                plan.push((local.clone(), PathBuf::from(base)));
                continue;
            }
            for e in walkdir::WalkDir::new(local).follow_links(false) {
                let e = e?;
                if e.file_type().is_file() {
                    let rel = e.path().strip_prefix(local)?;
                    plan.push((e.path().to_path_buf(), Path::new(base).join(rel)));
                }
            }
        }
        let mut out = Vec::with_capacity(plan.len());
        for (local, rel) in plan {
            let mut remote = paths::to_remote(&rel);
            if let Some(p) = prefix {
                remote = format!("{}/{remote}", p.trim_matches('/'));
            }
            out.push(self.upload(&local, &remote, overwrite).await?);
        }
        Ok(out)
    }

    /// Downloads `remote` into `dest_dir`, keeping its relative path, and
    /// checks the SHA-256 the server announced.
    pub async fn download(&self, remote: &str, dest_dir: &Path) -> Result<Downloaded> {
        let rel = paths::clean(remote).map_err(|e| anyhow::anyhow!("{remote}: {e}"))?;
        let resp = self
            .http
            .get(self.file_url(remote))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("connecting to the server")?;
        let resp = check(resp).await?;
        let announced = resp
            .headers()
            .get(SHA256_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let dest = dest_dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).await?;
        }
        let tmp = dest.with_file_name(format!(
            ".{}.fireshare-part",
            dest.file_name().unwrap_or_default().to_string_lossy()
        ));
        let mut file = fs::File::create(&tmp).await?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = fs::remove_file(&tmp).await;
                    return Err(e.into());
                }
            };
            size += chunk.len() as u64;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        let sha = hex::encode(hasher.finalize());
        if let Some(want) = announced
            && want != sha
        {
            let _ = fs::remove_file(&tmp).await;
            bail!("{remote}: sha256 mismatch: server sent {want}, received {sha}");
        }
        fs::rename(&tmp, &dest).await?;
        Ok(Downloaded {
            path: dest,
            size,
            sha256: sha,
        })
    }
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or(body);
    bail!("server answered {status}: {msg}")
}
