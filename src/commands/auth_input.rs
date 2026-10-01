use std::io::{self, Read};

use crate::errors::CliError;

const MAX_SECRET_BYTES: u64 = 65_536;

/// Read one authentication secret from stdin.
///
/// Input is bounded before allocation, outer whitespace is discarded, and
/// error messages never include any portion of the supplied secret.
pub fn read_secret() -> Result<String, CliError> {
    let stdin = io::stdin();
    read_secret_from(stdin.lock())
}

fn read_secret_from(reader: impl Read) -> Result<String, CliError> {
    // Read at most one byte past the limit so oversized input can be detected
    // without buffering an unbounded pipe in memory.
    let mut bytes = Vec::with_capacity(MAX_SECRET_BYTES as usize + 1);
    reader
        .take(MAX_SECRET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CliError::InvalidInput("could not read secret from stdin".into()))?;

    if bytes.len() as u64 > MAX_SECRET_BYTES {
        return Err(CliError::InvalidInput(format!(
            "secret input exceeds the {MAX_SECRET_BYTES}-byte limit"
        )));
    }

    let secret = String::from_utf8(bytes)
        .map_err(|_| CliError::InvalidInput("secret input must be valid UTF-8".into()))?;
    let secret = secret.trim().to_owned();
    if secret.is_empty() {
        return Err(CliError::InvalidInput("secret input is empty".into()));
    }

    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_outer_whitespace() {
        let secret = read_secret_from("  token-value\n".as_bytes()).unwrap();
        assert_eq!(secret, "token-value");
    }

    #[test]
    fn rejects_empty_input() {
        let error = read_secret_from(" \n\t".as_bytes()).unwrap_err();
        assert!(matches!(error, CliError::InvalidInput(_)));
        assert!(!error.to_string().contains("\n\t"));
    }

    #[test]
    fn accepts_exact_size_limit() {
        let input = vec![b'x'; MAX_SECRET_BYTES as usize];
        let secret = read_secret_from(input.as_slice()).unwrap();
        assert_eq!(secret.len(), MAX_SECRET_BYTES as usize);
    }

    #[test]
    fn rejects_oversized_input_without_echoing_it() {
        let input = vec![b's'; MAX_SECRET_BYTES as usize + 1];
        let error = read_secret_from(input.as_slice()).unwrap_err();
        assert!(matches!(error, CliError::InvalidInput(_)));
        assert!(!error.to_string().contains("ssssssss"));
    }

    #[test]
    fn rejects_non_utf8_without_echoing_it() {
        let input = [
            0xff, b's', b'e', b'n', b's', b'i', b't', b'i', b'v', b'e', b'-', b's', b'e', b'n',
            b't', b'i', b'n', b'e', b'l',
        ];
        let error = read_secret_from(input.as_slice()).unwrap_err();
        assert!(matches!(error, CliError::InvalidInput(_)));
        assert!(!error.to_string().contains("sensitive-sentinel"));
    }
}
