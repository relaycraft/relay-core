use crate::args::CaAction;
use anyhow::Result;
use relay_core_lib::tls::CertificateAuthority;
#[cfg(target_os = "macos")]
use std::process::Command;

#[allow(unused_variables)]
pub fn execute(action: CaAction) -> Result<()> {
    match action {
        CaAction::Init { cert, key, force } => {
            if cert.exists() && !force {
                println!(
                    "CA certificate already exists at {:?}. Use --force to overwrite.",
                    cert
                );
                return Ok(());
            }

            if force {
                // Remove existing files to allow regeneration
                if cert.exists() {
                    let _ = std::fs::remove_file(&cert);
                }
                if key.exists() {
                    let _ = std::fs::remove_file(&key);
                }
                let meta = cert.with_extension("json");
                if meta.exists() {
                    let _ = std::fs::remove_file(&meta);
                }
            }

            match CertificateAuthority::load_or_create(&cert, &key) {
                Ok(_) => println!("CA certificate initialized at {:?}", cert),
                Err(e) => {
                    eprintln!("Failed to initialize CA: {}", e);
                    std::process::exit(1);
                }
            }
        }
        CaAction::Export { cert, output } => {
            if !cert.exists() {
                eprintln!("CA certificate not found at {:?}. Run 'init' first.", cert);
                std::process::exit(1);
            }
            let content = std::fs::read_to_string(&cert)?;
            if let Some(out_path) = output {
                std::fs::write(&out_path, content)?;
                println!("CA certificate exported to {:?}", out_path);
            } else {
                println!("{}", content);
            }
        }
        CaAction::Install { cert } => {
            if !cert.exists() {
                eprintln!("CA certificate not found at {:?}. Run 'init' first.", cert);
                std::process::exit(1);
            }

            #[cfg(target_os = "macos")]
            {
                println!("Adding RelayCraft CA to System Keychain (requires sudo)...");
                println!("This allows your browser to trust certificates signed by RelayCore.");
                let status = Command::new("sudo")
                    .arg("security")
                    .arg("add-trusted-cert")
                    .arg("-d")
                    .arg("-r")
                    .arg("trustRoot")
                    .arg("-k")
                    .arg("/Library/Keychains/System.keychain")
                    .arg(&cert)
                    .status()?;

                if status.success() {
                    println!();
                    println!("CA certificate installed and trusted by macOS.");
                    println!();
                    println!("Next: configure your browser or system proxy to use relay-core.");
                    println!("  HTTP Proxy: 127.0.0.1, Port: 8080");
                    println!("  HTTPS Proxy: 127.0.0.1, Port: 8080");
                } else {
                    eprintln!(
                        "Failed to install CA certificate. Exit code: {:?}",
                        status.code()
                    );
                    eprintln!(
                        "Try running: sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {:?}",
                        cert
                    );
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                println!("Automatic installation is not supported on this platform yet.");
                println!(
                    "Please install {:?} manually to your system's trust store.",
                    cert
                );
            }
        }
        CaAction::Uninstall { cert } => {
            #[cfg(target_os = "macos")]
            {
                println!("Uninstalling CA certificate from System Keychain (requires sudo)...");
                let status = Command::new("sudo")
                    .arg("security")
                    .arg("remove-trusted-cert")
                    .arg("-d")
                    .arg(&cert)
                    .status()?;

                if status.success() {
                    println!("CA certificate uninstalled successfully.");
                } else {
                    eprintln!(
                        "Failed to uninstall CA certificate. Exit code: {:?}",
                        status.code()
                    );
                }
            }
        }
        CaAction::Status { cert } => {
            if !cert.exists() {
                println!("Status: Not Initialized (File missing at {:?})", cert);
                return Ok(());
            }

            println!("Status: Initialized (File exists at {:?})", cert);

            #[cfg(target_os = "macos")]
            {
                match get_file_sha1(&cert) {
                    Ok(hash) => match is_cert_installed_macos(&hash) {
                        Ok(true) => println!(
                            "System Trust: Installed and Trusted (Found in System Keychain)"
                        ),
                        Ok(false) => {
                            println!("System Trust: Not Installed (Not found in System Keychain)");
                            println!(
                                "Note: Local certificate (SHA1: {}) does not match System Keychain.",
                                hash
                            );
                            println!("      Run 'ca install' to update system trust.");
                        }
                        Err(e) => println!("System Trust: Unknown (Check failed: {})", e),
                    },
                    Err(_) => {
                        println!("System Trust: Unknown (Failed to compute local certificate hash)")
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
use sha1::{Digest, Sha1};

#[cfg(target_os = "macos")]
fn get_file_sha1(path: &std::path::Path) -> Result<String> {
    // Read PEM file content
    let pem_content = std::fs::read_to_string(path)?;

    // Parse PEM to get DER bytes
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(pem_content.as_bytes()));
    let cert_der = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No certificate found in PEM file"))?;

    // Compute SHA-1 of the DER bytes
    let mut hasher = Sha1::new();
    hasher.update(cert_der.as_ref());
    let result = hasher.finalize();

    Ok(hex::encode(result).to_uppercase())
}

#[cfg(target_os = "macos")]
fn is_cert_installed_macos(sha1: &str) -> Result<bool> {
    let output = Command::new("security")
        .arg("find-certificate")
        .arg("-c")
        .arg("RelayCraft CA")
        .arg("-Z") // Print SHA-1 hash
        .arg("/Library/Keychains/System.keychain")
        .output()?;

    // If command fails (e.g. cert not found), it might return non-zero
    if !output.status.success() {
        return Ok(false);
    }

    // SHA-1 hash from security -Z is usually in format "SHA-1 hash: <HASH>"
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    // Normalize input SHA1 (remove spaces if any)
    let target_sha1 = sha1.replace(" ", "").to_uppercase();
    // Normalize output (remove spaces/newlines for robust check)
    let clean_stdout = stdout.replace(" ", "").replace("\n", "");

    Ok(clean_stdout.contains(&target_sha1))
}
