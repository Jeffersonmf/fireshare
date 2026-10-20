//! End to end: a real server on a loopback port, the real client.

use std::path::Path;

use fireshare::client::Client;
use fireshare::server::{Config, router};

const TOKEN: &str = "s3cret";

struct Server {
    url: String,
    root: tempfile::TempDir,
}

async fn start(max_bytes: u64) -> Server {
    let root = tempfile::tempdir().unwrap();
    let app = router(Config {
        root: root.path().to_path_buf(),
        token: TOKEN.into(),
        max_bytes,
    })
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { url, root }
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Deterministic, non-repeating-ish content of `n` bytes.
fn content(n: usize) -> Vec<u8> {
    let mut x: u32 = 2_463_534_242;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn tmp_is_empty(root: &Path) -> bool {
    std::fs::read_dir(root.join(".fireshare-tmp"))
        .unwrap()
        .next()
        .is_none()
}

#[tokio::test]
async fn upload_then_download_round_trips_byte_for_byte() {
    let srv = start(u64::MAX).await;
    let local = tempfile::tempdir().unwrap();
    let data = content(5 * 1024 * 1024 + 7);
    write(&local.path().join("big.bin"), &data);
    let c = Client::new(&srv.url, TOKEN);

    let stored = c
        .upload(&local.path().join("big.bin"), "in/big.bin", false)
        .await
        .unwrap();
    assert_eq!(stored.size, data.len() as u64);
    assert_eq!(
        std::fs::read(srv.root.path().join("in/big.bin")).unwrap(),
        data
    );

    let out = tempfile::tempdir().unwrap();
    let got = c.download("in/big.bin", out.path()).await.unwrap();
    assert_eq!(got.sha256, stored.sha256);
    assert_eq!(std::fs::read(out.path().join("in/big.bin")).unwrap(), data);
    assert!(tmp_is_empty(srv.root.path()));
}

#[tokio::test]
async fn send_walks_directories_and_list_shows_nested_paths() {
    let srv = start(u64::MAX).await;
    let local = tempfile::tempdir().unwrap();
    let proj = local.path().join("proj");
    write(&proj.join("README.md"), b"# hi");
    write(&proj.join("src/main.rs"), b"fn main() {}");
    write(&local.path().join("notes.txt"), b"n");
    let c = Client::new(&srv.url, TOKEN);

    let stored = c
        .send(
            &[proj, local.path().join("notes.txt")],
            Some("handoff"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(stored.len(), 3);

    let names: Vec<String> = c
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert_eq!(
        names,
        [
            "handoff/notes.txt",
            "handoff/proj/README.md",
            "handoff/proj/src/main.rs"
        ]
    );
}

#[tokio::test]
async fn a_wrong_or_missing_token_is_refused() {
    let srv = start(u64::MAX).await;
    let err = Client::new(&srv.url, "nope").list().await.unwrap_err();
    assert!(err.to_string().contains("401"), "{err}");

    let status = reqwest::get(format!("{}/api/files", srv.url))
        .await
        .unwrap()
        .status();
    assert_eq!(status, 401);
}

#[tokio::test]
async fn the_token_also_works_as_a_query_parameter_for_browser_links() {
    let srv = start(u64::MAX).await;
    write(&srv.root.path().join("a.txt"), b"hello");
    let r = reqwest::get(format!("{}/api/files/a.txt?token={TOKEN}", srv.url))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "hello");
}

#[tokio::test]
async fn paths_cannot_escape_the_shared_directory() {
    let srv = start(u64::MAX).await;
    let http = reqwest::Client::new();
    for bad in [
        "..%2Fsecret",
        "a/..%2F..%2Fsecret",
        "%2Fetc%2Fpasswd",
        ".fireshare-tmp/x",
    ] {
        let r = http
            .put(format!("{}/api/files/{bad}", srv.url))
            .bearer_auth(TOKEN)
            .body("x")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{bad}");
    }
    assert!(!srv.root.path().parent().unwrap().join("secret").exists());
}

#[tokio::test]
async fn a_symlink_out_of_the_share_is_not_served() {
    let srv = start(u64::MAX).await;
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("secret.txt"), b"secret");
    std::os::unix::fs::symlink(outside.path(), srv.root.path().join("link")).unwrap();

    let r = reqwest::Client::new()
        .get(format!("{}/api/files/link/secret.txt", srv.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn a_corrupted_upload_is_rejected_and_leaves_nothing_behind() {
    let srv = start(u64::MAX).await;
    let r = reqwest::Client::new()
        .put(format!("{}/api/files/x.bin", srv.url))
        .bearer_auth(TOKEN)
        .header(fireshare::SHA256_HEADER, "00".repeat(32))
        .body("payload")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
    assert!(!srv.root.path().join("x.bin").exists());
    assert!(tmp_is_empty(srv.root.path()));
}

#[tokio::test]
async fn existing_files_are_kept_unless_overwrite_is_asked() {
    let srv = start(u64::MAX).await;
    let local = tempfile::tempdir().unwrap();
    write(&local.path().join("f.txt"), b"v1");
    let c = Client::new(&srv.url, TOKEN);
    c.upload(&local.path().join("f.txt"), "f.txt", false)
        .await
        .unwrap();

    write(&local.path().join("f.txt"), b"v2");
    let err = c
        .upload(&local.path().join("f.txt"), "f.txt", false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("409"), "{err}");
    assert_eq!(std::fs::read(srv.root.path().join("f.txt")).unwrap(), b"v1");

    c.upload(&local.path().join("f.txt"), "f.txt", true)
        .await
        .unwrap();
    assert_eq!(std::fs::read(srv.root.path().join("f.txt")).unwrap(), b"v2");
}

#[tokio::test]
async fn uploads_above_the_limit_are_refused_and_discarded() {
    let srv = start(1024).await;
    let local = tempfile::tempdir().unwrap();
    write(&local.path().join("big"), &content(4096));
    let err = Client::new(&srv.url, TOKEN)
        .upload(&local.path().join("big"), "big", false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("413"), "{err}");
    assert!(!srv.root.path().join("big").exists());
    assert!(tmp_is_empty(srv.root.path()));
}

#[tokio::test]
async fn the_browser_page_is_served_without_a_token() {
    let srv = start(u64::MAX).await;
    let r = reqwest::get(format!("{}/", srv.url)).await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.text().await.unwrap().contains("fireshare"));
}
