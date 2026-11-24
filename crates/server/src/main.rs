use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rust_chat_crypto::{PasswordVerifier, certificate_fingerprint, load_verifier, save_verifier};
use rust_chat_server::DEFAULT_CONFIG_PATH;
use rust_chat_server::config::{ServerConfig, TlsConfig};
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
    /// Run the server (implemented in M1).
    Serve {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
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
        Command::Init { config, password } => init(&config, password),
        Command::HashPassword { config, password } => hash_password(&config, password),
        Command::Fingerprint { config } => print_fingerprint(&config),
        Command::Serve { config } => serve(&config),
    }
}

fn init(config_path: &Path, password: Option<String>) -> anyhow::Result<()> {
    let config = if config_path.exists() {
        println!("kept existing {}", config_path.display());
        ServerConfig::load(config_path)?
    } else {
        let config = ServerConfig::default();
        config.save(config_path)?;
        println!("wrote {}", config_path.display());
        config
    };

    let verifier_path = Path::new(&config.verifier_file);
    if verifier_path.exists() {
        println!("kept existing {}", verifier_path.display());
    } else {
        let password = prompt_password(password, "Server password: ")?;
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

fn serve(config_path: &Path) -> anyhow::Result<()> {
    let config = ServerConfig::load(config_path)?;
    let verifier = load_verifier(Path::new(&config.verifier_file))?;
    let _ = &verifier.stored_key;

    println!(
        "rust-chat-server '{}' config loaded ({}, {} rooms); WebSocket runtime lands in M1",
        config.server_name,
        config.port,
        config.rooms.len()
    );
    bail!(
        "`serve` is not implemented yet (M1). Config and verifier at {} loaded successfully.",
        config_path.display()
    )
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
