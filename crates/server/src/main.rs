use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rust_chat_crypto::{PasswordVerifier, certificate_fingerprint, load_verifier, save_verifier};
use rust_chat_server::DEFAULT_CONFIG_PATH;
use rust_chat_server::config::{ServerConfig, TlsConfig};
use rust_chat_server::server;
use rust_chat_server::state::Shared;
use std::io::BufReader;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "rust-chat-server",
    version,
    about = "Self-hosted chat control plane: text, presence and WebRTC signaling"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create config, password verifier and the self-signed TLS key pair.
    Init {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
        /// Non-interactive password; otherwise prompts on the terminal.
        #[arg(long)]
        password: Option<String>,
        /// Read the password from a file (first line); avoids process arguments.
        #[arg(long, conflicts_with = "password")]
        password_file: Option<PathBuf>,
        /// Base directory for the verifier and TLS material (for an installed layout).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Directory for client-created rooms (defaults to the relative config path).
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Replace the shared password verifier.
    HashPassword {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
        #[arg(long)]
        password: Option<String>,
    },
    /// Print the certificate fingerprint clients pin on first connect.
    Fingerprint {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
    /// Update the server from the latest GitHub release (Linux).
    Update {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
        /// Only report the current and latest versions.
        #[arg(long)]
        check: bool,
        /// Do not ask for confirmation (for timers).
        #[arg(long)]
        yes: bool,
        /// Restore rust-chat-server.previous.
        #[arg(long)]
        rollback: bool,
        /// Install a specific tag instead of the latest release.
        #[arg(long)]
        release: Option<String>,
    },
    /// Run the WebSocket control plane.
    Serve {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve {
        config: PathBuf::from(DEFAULT_CONFIG_PATH),
    }) {
        Command::Init {
            config,
            password,
            password_file,
            dir,
            data_dir,
        } => init(&config, password, password_file, dir, data_dir),
        Command::HashPassword { config, password } => hash_password(&config, password),
        Command::Fingerprint { config } => print_fingerprint(&config),
        Command::Update {
            config,
            check,
            yes,
            rollback,
            release,
        } => update_command(&config, check, yes, rollback, release),
        Command::Serve { config } => serve(&config).await,
    }
}

fn init(
    config_path: &Path,
    password: Option<String>,
    password_file: Option<PathBuf>,
    dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let config = if config_path.exists() {
        println!("kept existing {}", config_path.display());
        ServerConfig::load(config_path)?
    } else {
        let mut config = ServerConfig::default();
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir)?;
            config.verifier_file = dir.join("verifier").to_string_lossy().into_owned();
            config.tls.cert_file = dir
                .join("tls")
                .join("cert.pem")
                .to_string_lossy()
                .into_owned();
            config.tls.key_file = dir
                .join("tls")
                .join("key.pem")
                .to_string_lossy()
                .into_owned();
        }
        if let Some(data_dir) = &data_dir {
            std::fs::create_dir_all(data_dir)?;
            config.client_rooms_file = data_dir.join("rooms.json").to_string_lossy().into_owned();
        }
        config.save(config_path)?;
        println!("wrote {}", config_path.display());
        config
    };

    let verifier_path = Path::new(&config.verifier_file);
    if verifier_path.exists() {
        println!("kept existing {}", verifier_path.display());
    } else {
        if let Some(parent) = verifier_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let password = match password_file {
            Some(path) => std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}", path.display()))?
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
            None => prompt_password(password, "Server password: ")?,
        };
        let verifier = PasswordVerifier::with_defaults(&password)?;
        save_verifier(verifier_path, &verifier)?;
        println!("wrote {}", verifier_path.display());
    }

    if config.tls.enabled {
        ensure_tls_material(&config.tls, &config.server_name)?;
    }

    let fingerprint = read_fingerprint(&config.tls)?;
    println!("server fingerprint: {fingerprint}");
    println!("Clients pin this fingerprint on first connect; verify it out of band.");
    println!();
    println!(
        "next: rust-chat-server serve --config {}",
        config_path.display()
    );
    Ok(())
}

fn hash_password(config_path: &Path, password: Option<String>) -> anyhow::Result<()> {
    let config = ServerConfig::load(config_path)?;
    let password = prompt_password(password, "New server password: ")?;
    let verifier = PasswordVerifier::with_defaults(&password)?;
    let path = Path::new(&config.verifier_file);
    save_verifier(path, &verifier)?;
    println!(
        "wrote {} (all sessions will need the new password)",
        path.display()
    );
    Ok(())
}

fn print_fingerprint(config_path: &Path) -> anyhow::Result<()> {
    let config = ServerConfig::load(config_path)?;
    println!("{}", read_fingerprint(&config.tls)?);
    Ok(())
}

fn update_command(
    config_path: &Path,
    check: bool,
    yes: bool,
    rollback: bool,
    release: Option<String>,
) -> anyhow::Result<()> {
    let config = ServerConfig::load(config_path).unwrap_or_default();
    if rollback {
        let exe = rust_chat_server::update::rollback()?;
        println!("rolled back {}", exe.display());
        rust_chat_server::update::restart_and_wait(config.port)?;
        println!("server restarted on port {}", config.port);
        return Ok(());
    }

    let tag = release.map(|tag| {
        if tag.starts_with('v') {
            tag
        } else {
            format!("v{tag}")
        }
    });
    let release = rust_chat_server::update::release(tag.as_deref())?;
    let current = env!("CARGO_PKG_VERSION");
    if !rust_chat_server::update::is_newer(&release.version, current) {
        println!(
            "rust-chat-server {current} is up to date (latest {})",
            release.version
        );
        return Ok(());
    }
    if check {
        println!(
            "current {current}; latest {} at {}",
            release.version, release.html_url
        );
        return Ok(());
    }
    if !yes
        && !rust_chat_server::update::confirm(&format!(
            "Update rust-chat-server from {current} to {}?",
            release.version
        ))?
    {
        println!("cancelled");
        return Ok(());
    }

    let exe = rust_chat_server::update::install(&release)?;
    println!("installed {} at {}", release.version, exe.display());
    rust_chat_server::update::restart_and_wait(config.port)?;
    println!("server restarted and listening on port {}", config.port);
    Ok(())
}

async fn serve(config_path: &Path) -> anyhow::Result<()> {
    let config = ServerConfig::load(config_path)?;
    let verifier = load_verifier(Path::new(&config.verifier_file))?;
    let shared = Shared::new(config, verifier)?;
    server::run(shared).await
}

fn prompt_password(provided: Option<String>, prompt: &str) -> anyhow::Result<String> {
    let password = match provided {
        Some(password) => password,
        None => rpassword::prompt_password(prompt).context("cannot read password")?,
    };
    if password.is_empty() {
        bail!("password must not be empty");
    }
    Ok(password)
}

fn ensure_tls_material(tls: &TlsConfig, common_name: &str) -> anyhow::Result<()> {
    let cert_path = Path::new(&tls.cert_file);
    let key_path = Path::new(&tls.key_file);
    if cert_path.exists() && key_path.exists() {
        println!("kept existing {}", cert_path.display());
        return Ok(());
    }

    let mut params = CertificateParams::new(vec![common_name.to_owned(), "localhost".to_owned()])
        .context("cannot build certificate parameters")?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    params.distinguished_name = name;
    let key_pair = KeyPair::generate().context("cannot generate an ECDSA P-256 key pair")?;
    let certificate = params
        .self_signed(&key_pair)
        .context("cannot self-sign the certificate")?;

    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(cert_path, certificate.pem())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::write(key_path, key_pair.serialize_pem())?;
    println!("wrote {} and {}", cert_path.display(), key_path.display());
    Ok(())
}

fn read_fingerprint(tls: &TlsConfig) -> anyhow::Result<String> {
    anyhow::ensure!(
        tls.enabled,
        "TLS is disabled in the config, so there is no certificate to fingerprint"
    );
    let file = std::fs::File::open(&tls.cert_file)
        .with_context(|| format!("cannot open {}", tls.cert_file))?;
    let mut reader = BufReader::new(file);
    let certificates: Vec<_> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("cannot parse {}", tls.cert_file))?;
    let certificate = certificates
        .first()
        .with_context(|| format!("{} contains no certificate", tls.cert_file))?;
    Ok(certificate_fingerprint(certificate.as_ref()))
}
