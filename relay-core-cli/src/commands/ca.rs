use crate::args::CaAction;
use anyhow::Result;
use relay_core_lib::tls::CertificateAuthority;
use relay_core_runtime::CaPaths;
#[cfg(target_os = "macos")]
use std::process::Command;

/// Must match `Run` default in `args.rs`.
#[cfg(target_os = "macos")]
const DEFAULT_PROXY_LISTEN: &str = "127.0.0.1:8080";

#[allow(unused_variables)]
pub fn execute(action: CaAction) -> Result<()> {
    match action {
        CaAction::Generate {
            ca_cert,
            ca_key,
            force,
        } => {
            let ca = CaPaths::resolve(ca_cert, ca_key).map_err(anyhow::Error::msg)?;
            if let Some(parent) = ca.cert.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if let Some(parent) = ca.key.parent() {
                std::fs::create_dir_all(parent)?;
            }

            if ca.cert.exists() && ca.key.exists() && !force {
                println!(
                    "CA certificate already exists at {:?}. Use --force to overwrite.",
                    ca.cert
                );
                return Ok(());
            }

            if force {
                if ca.cert.exists() {
                    let _ = std::fs::remove_file(&ca.cert);
                }
                if ca.key.exists() {
                    let _ = std::fs::remove_file(&ca.key);
                }
                let meta = ca.cert.with_extension("json");
                if meta.exists() {
                    let _ = std::fs::remove_file(&meta);
                }
            }

            match CertificateAuthority::load_or_create(&ca.cert, &ca.key) {
                Ok(_) => println!("CA certificate generated at {:?}", ca.cert),
                Err(e) => {
                    eprintln!("Failed to generate CA: {}", e);
                    std::process::exit(1);
                }
            }
        }
        CaAction::Export {
            ca_cert,
            ca_key,
            output,
        } => {
            let ca = CaPaths::resolve(ca_cert, ca_key).map_err(anyhow::Error::msg)?;
            if !ca_files_exist(&ca) {
                eprintln!("{}", missing_ca_guidance(&ca));
                std::process::exit(1);
            }
            let content = std::fs::read_to_string(&ca.cert)?;
            if let Some(out_path) = output {
                std::fs::write(&out_path, content)?;
                println!("CA certificate exported to {:?}", out_path);
            } else {
                println!("{}", content);
            }
        }
        CaAction::Install { ca_cert, ca_key } => {
            let ca = CaPaths::resolve(ca_cert, ca_key).map_err(anyhow::Error::msg)?;
            if !ca_files_exist(&ca) {
                eprintln!("{}", missing_ca_guidance(&ca));
                std::process::exit(1);
            }

            #[cfg(target_os = "macos")]
            {
                println!("Adding RelayCraft CA to System Keychain (requires sudo)...");
                println!("This allows your browser to trust certificates signed by RelayCore.");

                if let Err(e) = macos_remove_relaycraft_ca_from_system_keychain() {
                    eprintln!("Warning: could not remove previous RelayCraft CA entries: {e}");
                }

                let status = Command::new("sudo")
                    .arg("security")
                    .arg("add-trusted-cert")
                    .arg("-d")
                    .arg("-r")
                    .arg("trustRoot")
                    .arg("-k")
                    .arg(macos_system_keychain())
                    .arg(&ca.cert)
                    .status()?;

                if status.success() {
                    match get_file_sha1(&ca.cert).and_then(macos_trust_status_for_local_cert) {
                        Ok(MacosTrustStatus::Trusted) => {
                            println!();
                            println!("CA certificate installed and trusted by macOS.");
                            print_proxy_setup_hint();
                        }
                        Ok(other) => {
                            println!();
                            println!(
                                "CA install command finished, but trust could not be verified:"
                            );
                            println!("  {}", other.summary_line());
                            print_proxy_setup_hint();
                        }
                        Err(e) => {
                            println!();
                            println!("CA install command finished (verify manually): {e}");
                            print_proxy_setup_hint();
                        }
                    }
                } else {
                    eprintln!(
                        "Failed to install CA certificate. Exit code: {:?}",
                        status.code()
                    );
                    eprintln!(
                        "Try: sudo security add-trusted-cert -d -r trustRoot -k {} {:?}",
                        macos_system_keychain(),
                        ca.cert
                    );
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                println!("Automatic installation is not supported on this platform yet.");
                println!(
                    "Please install {:?} manually to your system's trust store.",
                    ca.cert
                );
            }
        }
        CaAction::Uninstall { .. } => {
            #[cfg(target_os = "macos")]
            {
                println!("Uninstalling RelayCraft CA from System Keychain (requires sudo)...");
                match macos_remove_relaycraft_ca_from_system_keychain() {
                    Ok(removed) if removed > 0 => {
                        println!(
                            "Removed {removed} RelayCraft CA certificate(s) from the system keychain."
                        );
                    }
                    Ok(_) => println!("No RelayCraft CA certificate found in the system keychain."),
                    Err(e) => eprintln!("Failed to uninstall CA certificate: {e}"),
                }
            }
        }
        CaAction::Status { ca_cert, ca_key } => {
            let ca = CaPaths::resolve(ca_cert, ca_key).map_err(anyhow::Error::msg)?;
            if !ca_files_exist(&ca) {
                println!("CA Status: Not generated");
                println!("  cert: {}", ca.cert.display());
                println!("  key:  {}", ca.key.display());
                println!("  next: relay-core-cli ca generate");
                return Ok(());
            }

            println!("CA Status: Generated");
            println!("  cert: {}", ca.cert.display());
            println!("  key:  {}", ca.key.display());

            #[cfg(target_os = "macos")]
            {
                match get_file_sha1(&ca.cert).and_then(macos_trust_status_for_local_cert) {
                    Ok(status) => println!("  trust: {}", status.summary_line()),
                    Err(e) => println!("  trust: unknown ({e})"),
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                println!("  trust: not auto-checked on this platform");
            }
        }
    }
    Ok(())
}

fn ca_files_exist(ca: &CaPaths) -> bool {
    ca.cert.exists() && ca.key.exists()
}

fn missing_ca_guidance(ca: &CaPaths) -> String {
    format!(
        "CA files are missing.\n  cert: {}\n  key:  {}\nRun `relay-core-cli ca generate` first.",
        ca.cert.display(),
        ca.key.display()
    )
}

#[cfg(target_os = "macos")]
fn print_proxy_setup_hint() {
    println!();
    println!("Next:");
    println!("  1. relay run -l {DEFAULT_PROXY_LISTEN}");
    println!("  2. Point your browser or OS HTTP/HTTPS proxy at the same host:port");
    println!("     (change -l / --listen if you use a custom address)");
}

#[cfg(target_os = "macos")]
use sha1::{Digest, Sha1};

#[cfg(target_os = "macos")]
const RELAYCRAFT_CA_CN: &str = "RelayCraft CA";

#[cfg(target_os = "macos")]
fn macos_system_keychain() -> &'static str {
    "/Library/Keychains/System.keychain"
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
enum MacosTrustStatus {
    Trusted,
    NotInKeychain,
    StaleKeychain {
        local_sha1: String,
        keychain_sha1s: Vec<String>,
    },
    CheckFailed(String),
}

#[cfg(target_os = "macos")]
impl MacosTrustStatus {
    fn summary_line(&self) -> String {
        match self {
            Self::Trusted => format!("Installed and trusted (matches \"{RELAYCRAFT_CA_CN}\")"),
            Self::NotInKeychain => {
                "Not installed in the system keychain — run `ca install`".to_string()
            }
            Self::StaleKeychain {
                local_sha1,
                keychain_sha1s,
            } => {
                format!(
                    "Keychain has {} older \"{}\" entr{} (SHA1 {}); local file is {} — run `ca install` to refresh",
                    keychain_sha1s.len(),
                    RELAYCRAFT_CA_CN,
                    if keychain_sha1s.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    },
                    keychain_sha1s.join(", "),
                    local_sha1
                )
            }
            Self::CheckFailed(msg) => format!("Check failed ({msg})"),
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_trust_status_for_local_cert(local_sha1: String) -> Result<MacosTrustStatus> {
    let keychain_sha1s = macos_list_relaycraft_ca_sha1_in_system_keychain()?;
    if keychain_sha1s.is_empty() {
        return Ok(MacosTrustStatus::NotInKeychain);
    }
    if keychain_sha1s.iter().any(|h| h == &local_sha1) {
        return Ok(MacosTrustStatus::Trusted);
    }
    Ok(MacosTrustStatus::StaleKeychain {
        local_sha1,
        keychain_sha1s,
    })
}

#[cfg(target_os = "macos")]
fn macos_list_relaycraft_ca_sha1_in_system_keychain() -> Result<Vec<String>> {
    let output = Command::new("security")
        .arg("find-certificate")
        .arg("-c")
        .arg(RELAYCRAFT_CA_CN)
        .arg("-a")
        .arg("-Z")
        .arg(macos_system_keychain())
        .output()?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    Ok(parse_sha1_hashes_from_security_z_output(
        &String::from_utf8_lossy(&output.stdout),
    ))
}

#[cfg(target_os = "macos")]
fn macos_remove_relaycraft_ca_from_system_keychain() -> Result<usize> {
    let mut removed = 0usize;
    loop {
        let status = Command::new("sudo")
            .arg("security")
            .arg("delete-certificate")
            .arg("-c")
            .arg(RELAYCRAFT_CA_CN)
            .arg(macos_system_keychain())
            .status()?;
        if status.success() {
            removed += 1;
        } else {
            break;
        }
    }
    Ok(removed)
}

#[cfg(target_os = "macos")]
fn get_file_sha1(path: &std::path::Path) -> Result<String> {
    let pem_content = std::fs::read_to_string(path)?;
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(pem_content.as_bytes()));
    let cert_der = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No certificate found in PEM file"))?;

    let mut hasher = Sha1::new();
    hasher.update(cert_der.as_ref());
    Ok(hex::encode(hasher.finalize()).to_uppercase())
}

/// Parse `SHA-1 hash: <HEX>` lines from `security find-certificate -Z` output.
#[cfg(target_os = "macos")]
fn parse_sha1_hashes_from_security_z_output(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("SHA-1 hash:")?;
            let hash = rest.trim().replace(' ', "").to_uppercase();
            if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                Some(hash)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::parse_sha1_hashes_from_security_z_output;

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_security_z_output_collects_all_sha1_hashes() {
        let sample = r#"SHA-256 hash: AAAA
SHA-1 hash: 556DC870334A7E801C993FF1710815705A936B3D
keychain: "/Library/Keychains/System.keychain"
SHA-256 hash: BBBB
SHA-1 hash: 404139F8ABA4A86B0D15613CB6397DB1DBF894D7
"#;
        let hashes = parse_sha1_hashes_from_security_z_output(sample);
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0], "556DC870334A7E801C993FF1710815705A936B3D");
        assert_eq!(hashes[1], "404139F8ABA4A86B0D15613CB6397DB1DBF894D7");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_security_z_output_ignores_sha256_lines() {
        let sample =
            "SHA-256 hash: DEADBEEF\nSHA-1 hash: 134EDAF3BB29368DDCA26BE1858ABACE31A2485C\n";
        let hashes = parse_sha1_hashes_from_security_z_output(sample);
        assert_eq!(hashes, vec!["134EDAF3BB29368DDCA26BE1858ABACE31A2485C"]);
    }
}
