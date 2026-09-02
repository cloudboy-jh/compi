use crate::Result;
use std::process::Command;

pub fn ensure_default_wsl2() -> Result<()> {
    let output = Command::new(r"C:\Windows\System32\wsl.exe")
        .args(["--list", "--verbose"])
        .output()?;
    if !output.status.success() {
        let error = decode_wsl_output(&output.stderr);
        return Err(format!("could not inspect WSL distributions: {}", error.trim()).into());
    }

    match parse_default_wsl_version(&output.stdout) {
        Some(2) => Ok(()),
        Some(version) => Err(format!(
            "the default WSL distribution uses WSL{version}; Compi requires WSL2"
        )
        .into()),
        None => Err("no default WSL distribution was found; Compi requires WSL2".into()),
    }
}

fn parse_default_wsl_version(output: &[u8]) -> Option<u32> {
    let output = decode_wsl_output(output);
    output.lines().find_map(|line| {
        let line = line.trim_start();
        let default = line.strip_prefix('*')?.trim_start();
        default.split_whitespace().last()?.parse().ok()
    })
}

fn decode_wsl_output(output: &[u8]) -> String {
    if output.len() >= 2 && output.chunks_exact(2).any(|pair| pair[1] == 0) {
        let words: Vec<u16> = output
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(output).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utf8_default_wsl_version() {
        let output =
            b"  NAME            STATE           VERSION\r\n* Ubuntu          Running         2\r\n";
        assert_eq!(parse_default_wsl_version(output), Some(2));
    }

    #[test]
    fn parses_utf16_default_wsl_version() {
        let text = "  NAME      STATE      VERSION\r\n* Ubuntu    Stopped    2\r\n";
        let output: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(parse_default_wsl_version(&output), Some(2));
    }
}
