use crate::args::TransparentAction;
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "macos")]
use anyhow::{Result, Context};
#[cfg(target_os = "macos")]
use std::io::Write;


#[cfg(target_os = "macos")]
pub fn handle_transparent_command(action: TransparentAction) -> Result<()> {
    match action {
        TransparentAction::Generate { port, output, interface } => {
            let config = generate_pf_config(port, &interface);
            if let Some(path) = output {
                std::fs::write(&path, &config)
                    .with_context(|| format!("Failed to write PF config to {:?}", path))?;
                println!("PF configuration written to {:?}", path);
            } else {
                println!("{}", config);
            }
        }
        TransparentAction::Load { port, interface } => {
            // Check if running as root
            if !is_root() {
                anyhow::bail!("This command requires root privileges. Please run with sudo.");
            }

            let config = generate_pf_config(port, &interface);
            
            // Write config to temporary file
            let mut temp = tempfile::Builder::new()
                .prefix("relay_pf_")
                .suffix(".conf")
                .tempfile()?;
            
            temp.write_all(config.as_bytes())?;
            let temp_path = temp.path().to_owned();
            
            // Load rules
            // pfctl -a relaycraft -f /tmp/relay_pf_XXXX.conf
            let status = Command::new("pfctl")
                .args(["-a", "relaycraft", "-f", temp_path.to_str().unwrap()])
                .status()
                .context("Failed to execute pfctl")?;
            
            if !status.success() {
                anyhow::bail!("pfctl failed to load rules");
            }
            
            // Enable PF if not enabled
            // pfctl -e
            let _ = Command::new("pfctl")
                .arg("-e")
                .output(); // Ignore error if already enabled

            println!("Transparent proxy rules loaded (redirecting TCP to port {} on {}).", port, interface);
        }
        TransparentAction::Unload => {
            if !is_root() {
                anyhow::bail!("This command requires root privileges. Please run with sudo.");
            }

            // Flush rules
            // pfctl -a relaycraft -F all
            let status = Command::new("pfctl")
                .args(["-a", "relaycraft", "-F", "all"])
                .status()
                .context("Failed to execute pfctl")?;
            
             if !status.success() {
                anyhow::bail!("pfctl failed to unload rules");
            }
            println!("Transparent proxy rules unloaded.");
        }
        TransparentAction::Status => {
            // Check if rules are loaded
            // pfctl -a relaycraft -s rules
            let output = Command::new("pfctl")
                .args(["-a", "relaycraft", "-s", "rules"])
                .output()
                .context("Failed to execute pfctl")?;
            
            if output.status.success() {
                let rules = String::from_utf8_lossy(&output.stdout);
                if rules.trim().is_empty() {
                    println!("No active rules for relaycraft anchor.");
                } else {
                    println!("Active rules for relaycraft anchor:");
                    println!("{}", rules);
                }
            } else {
                 println!("Failed to check status. Are you root?");
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn handle_transparent_command(_action: TransparentAction) -> anyhow::Result<()> {
    anyhow::bail!("Transparent proxy management is only supported on macOS.");
}

#[cfg(target_os = "macos")]
fn generate_pf_config(port: u16, interface: &str) -> String {
    format!(
        "# relaycraft transparent proxy rules for {0}
rdr pass on {0} inet proto tcp from any to any port 80 -> 127.0.0.1 port {1}
rdr pass on {0} inet proto tcp from any to any port 443 -> 127.0.0.1 port {1}
",
        interface, port
    )
}

#[cfg(target_os = "macos")]
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout).trim() == "0"
        })
        .unwrap_or(false)
}
