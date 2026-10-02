use crate::{DaemonClient, Result, probe};
use sha2::{Digest, Sha256};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionTarget {
    Local {
        instance: Option<String>,
    },
    Ssh {
        endpoint: SshEndpoint,
        instance: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshEndpoint {
    destination: String,
    port: Option<u16>,
}

impl ConnectionTarget {
    pub fn from_options(instance: Option<String>, connect: Option<String>) -> Result<Self> {
        compi_protocol::identity::instance_names(instance.as_deref())?;
        match connect {
            Some(connect) => Ok(Self::Ssh {
                endpoint: SshEndpoint::parse(&connect)?,
                instance,
            }),
            None => Ok(Self::Local { instance }),
        }
    }

    pub fn connect(&self) -> Result<DaemonClient> {
        match self {
            Self::Local { instance } => probe::connect_or_start(instance.as_deref()),
            Self::Ssh { endpoint, instance } => {
                let mut command = endpoint.command(instance.as_deref());
                DaemonClient::connect_command(&mut command)
            }
        }
    }

    pub fn restart_daemon(&self) -> Result<DaemonClient> {
        match self {
            Self::Local { instance } => probe::restart_daemon(instance.as_deref()),
            Self::Ssh { .. } => {
                if let Ok(mut client) = self.connect() {
                    client.shutdown_daemon()?;
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match self.connect() {
                        Ok(client) => return Ok(client),
                        Err(error) if Instant::now() < deadline => {
                            drop(error);
                            thread::sleep(Duration::from_millis(100));
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
    }

    pub fn state_instance(&self) -> Option<String> {
        match self {
            Self::Local { instance } => instance.clone(),
            Self::Ssh { endpoint, instance } => {
                let mut hash = Sha256::new();
                hash.update(endpoint.destination.as_bytes());
                hash.update([0]);
                hash.update(endpoint.port.unwrap_or(22).to_le_bytes());
                hash.update([0]);
                if let Some(instance) = instance {
                    hash.update(instance.as_bytes());
                }
                let digest = hash.finalize();
                Some(format!("ssh-{}", hex_prefix(&digest[..8])))
            }
        }
    }

    pub const fn is_remote(&self) -> bool {
        matches!(self, Self::Ssh { .. })
    }
}

impl SshEndpoint {
    fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.starts_with('-')
            || value.chars().any(|character| {
                character.is_whitespace()
                    || character.is_control()
                    || matches!(
                        character,
                        '\'' | '"' | '\\' | '$' | ';' | '&' | '|' | '<' | '>'
                    )
            })
        {
            return Err(
                "SSH target must be a nonempty [user@]host[:port] without shell metacharacters"
                    .into(),
            );
        }

        let (user, host_port) = match value.split_once('@') {
            Some((user, host)) if !user.is_empty() && !host.is_empty() && !host.contains('@') => {
                (Some(user), host)
            }
            Some(_) => return Err("SSH target has an invalid user or host".into()),
            None => (None, value),
        };
        let (host, port) = parse_host_port(host_port)?;
        if host.is_empty() || host.starts_with('-') {
            return Err("SSH target has an invalid host".into());
        }
        let destination = match user {
            Some(user) => format!("{user}@{host}"),
            None => host.to_owned(),
        };
        Ok(Self { destination, port })
    }

    fn command(&self, instance: Option<&str>) -> Command {
        let mut command = Command::new("ssh");
        command
            .arg("-T")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=10");
        if let Some(port) = self.port {
            command.arg("-p").arg(port.to_string());
        }
        command.arg("--").arg(&self.destination);
        let mut remote = String::from("compi-daemon --server-stdio");
        if let Some(instance) = instance {
            remote.push_str(" --instance ");
            remote.push_str(instance);
        }
        command.arg(remote);
        command
    }
}

fn parse_host_port(value: &str) -> Result<(&str, Option<u16>)> {
    if let Some(bracketed) = value.strip_prefix('[') {
        let close = bracketed
            .find(']')
            .ok_or("bracketed SSH host is missing ']'")?;
        let host = &value[..close + 2];
        let suffix = &bracketed[close + 1..];
        let port = match suffix.strip_prefix(':') {
            Some(port) => Some(parse_port(port)?),
            None if suffix.is_empty() => None,
            None => return Err("bracketed SSH host has invalid text after ']'".into()),
        };
        return Ok((host, port));
    }
    if value.matches(':').count() > 1 {
        return Err("IPv6 SSH hosts must use [address] brackets".into());
    }
    match value.rsplit_once(':') {
        Some((host, port)) => Ok((host, Some(parse_port(port)?))),
        None => Ok((value, None)),
    }
}

fn parse_port(value: &str) -> Result<u16> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| "SSH port must be an integer from 1 through 65535".into())
}

fn hex_prefix(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_targets_without_option_or_shell_injection() {
        let target = ConnectionTarget::from_options(
            Some("work".into()),
            Some("dev@example.com:2222".into()),
        )
        .unwrap();
        let ConnectionTarget::Ssh { endpoint, instance } = target else {
            panic!("expected SSH target");
        };
        assert_eq!(endpoint.destination, "dev@example.com");
        assert_eq!(endpoint.port, Some(2222));
        let arguments: Vec<_> = endpoint
            .command(instance.as_deref())
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            arguments,
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
                "--",
                "dev@example.com",
                "compi-daemon --server-stdio --instance work",
            ]
        );
        assert!(ConnectionTarget::from_options(None, Some("-oProxyCommand=bad".into())).is_err());
        assert!(ConnectionTarget::from_options(None, Some("host;bad".into())).is_err());
        assert!(ConnectionTarget::from_options(None, Some("host:0".into())).is_err());
    }

    #[test]
    fn remote_state_namespace_includes_host_port_and_instance() {
        let first = ConnectionTarget::from_options(None, Some("host:22".into())).unwrap();
        let second = ConnectionTarget::from_options(None, Some("host:2222".into())).unwrap();
        let third =
            ConnectionTarget::from_options(Some("work".into()), Some("host:22".into())).unwrap();
        assert_ne!(first.state_instance(), second.state_instance());
        assert_ne!(first.state_instance(), third.state_instance());
        assert_eq!(first.state_instance(), first.state_instance());
    }
}
