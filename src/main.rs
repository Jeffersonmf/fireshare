use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use fireshare::client::Client;
use fireshare::server::{self, Config};

/// Send files between machines over HTTP on one port.
#[derive(Parser)]
#[command(name = "fireshare", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Share a directory: API for the CLI plus a browser page at /
    Serve {
        /// Directory to share (created if missing)
        #[arg(short, long, default_value = "./shared", env = "FIRESHARE_DIR")]
        dir: PathBuf,
        #[arg(short, long, default_value_t = 7878, env = "FIRESHARE_PORT")]
        port: u16,
        /// Address to listen on; 0.0.0.0 accepts other machines on the network
        #[arg(short, long, default_value = "0.0.0.0", env = "FIRESHARE_BIND")]
        bind: IpAddr,
        /// Shared token; a random one is generated and printed when omitted
        #[arg(short, long, env = "FIRESHARE_TOKEN")]
        token: Option<String>,
        /// Largest accepted upload, in MiB
        #[arg(long, default_value_t = 20 * 1024, env = "FIRESHARE_MAX_MIB")]
        max_mib: u64,
    },
    /// Upload files or directories (recursively)
    Send {
        /// Files or directories
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[command(flatten)]
        remote: Remote,
        /// Put everything under this remote directory
        #[arg(long = "as")]
        prefix: Option<String>,
        /// Replace files that already exist on the server
        #[arg(long)]
        overwrite: bool,
    },
    /// Download files (keeps their relative paths)
    Get {
        /// Remote paths, as listed by `ls`
        names: Vec<String>,
        /// Download every shared file
        #[arg(long, conflicts_with = "names")]
        all: bool,
        /// Local directory to write into
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        #[command(flatten)]
        remote: Remote,
    },
    /// List the shared files
    Ls {
        #[command(flatten)]
        remote: Remote,
    },
}

#[derive(clap::Args)]
struct Remote {
    /// Server URL, e.g. http://192.168.0.10:7878
    #[arg(short, long, visible_aliases = ["to", "from"], env = "FIRESHARE_SERVER")]
    server: String,
    #[arg(short, long, env = "FIRESHARE_TOKEN")]
    token: String,
}

impl Remote {
    fn client(&self) -> Client {
        Client::new(&self.server, &self.token)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fireshare=info".into()),
        )
        .init();

    match Cli::parse().cmd {
        Cmd::Serve {
            dir,
            port,
            bind,
            token,
            max_mib,
        } => serve(dir, SocketAddr::new(bind, port), token, max_mib).await,
        Cmd::Send {
            paths,
            remote,
            prefix,
            overwrite,
        } => {
            let started = Instant::now();
            let stored = remote
                .client()
                .send(&paths, prefix.as_deref(), overwrite)
                .await?;
            let total: u64 = stored.iter().map(|s| s.size).sum();
            for s in &stored {
                println!(
                    "✔ {}  {}  sha256 {}",
                    s.path,
                    human(s.size),
                    &s.sha256[..12]
                );
            }
            println!(
                "{} arquivo(s), {} em {}",
                stored.len(),
                human(total),
                rate(total, started)
            );
            Ok(())
        }
        Cmd::Get {
            names,
            all,
            out,
            remote,
        } => {
            let client = remote.client();
            let names = if all {
                client.list().await?.into_iter().map(|e| e.path).collect()
            } else if names.is_empty() {
                bail!("informe os arquivos ou use --all")
            } else {
                names
            };
            let started = Instant::now();
            let mut total = 0;
            for name in &names {
                let d = client.download(name, &out).await?;
                total += d.size;
                println!(
                    "✔ {}  {}  sha256 {}",
                    d.path.display(),
                    human(d.size),
                    &d.sha256[..12]
                );
            }
            println!(
                "{} arquivo(s), {} em {}",
                names.len(),
                human(total),
                rate(total, started)
            );
            Ok(())
        }
        Cmd::Ls { remote } => {
            for e in remote.client().list().await? {
                println!("{:>10}  {}", human(e.size), e.path);
            }
            Ok(())
        }
    }
}

async fn serve(dir: PathBuf, addr: SocketAddr, token: Option<String>, max_mib: u64) -> Result<()> {
    let generated = token.is_none();
    let token = token.unwrap_or_else(fireshare::random_token);
    let app = server::router(Config {
        root: dir.clone(),
        token: token.clone(),
        max_bytes: max_mib * 1024 * 1024,
    })
    .await
    .with_context(|| format!("preparing {}", dir.display()))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    let port = listener.local_addr()?.port();
    let host = if addr.ip().is_unspecified() {
        lan_ip().map_or_else(|| "127.0.0.1".to_owned(), |ip| ip.to_string())
    } else {
        addr.ip().to_string()
    };
    println!("🔥 fireshare compartilhando {}", dir.display());
    println!("   navegador: http://{host}:{port}/");
    println!(
        "   token:     {token}{}",
        if generated {
            "  (gerado; use --token para fixar)"
        } else {
            ""
        }
    );
    println!("   cliente:   fireshare send <arquivos> --to http://{host}:{port} --token {token}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// The address other machines on the LAN reach this one by. Connecting a
/// UDP socket sends nothing; it only selects the outgoing interface.
fn lan_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn rate(bytes: u64, started: Instant) -> String {
    let secs = started.elapsed().as_secs_f64().max(1e-3);
    format!("{:.2}s ({}/s)", secs, human((bytes as f64 / secs) as u64))
}
