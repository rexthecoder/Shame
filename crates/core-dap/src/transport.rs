use std::io::{self, BufRead, Write};

/// Write one DAP message: a `Content-Length` header, a blank line, then
/// the raw JSON body. The Debug Adapter Protocol spec reuses LSP's
/// "Base Protocol" framing verbatim, so this is byte-for-byte the same
/// as `core-lsp::transport` — duplicated rather than shared so this
/// crate stays independent of `core-lsp` the same way `core-flutter`
/// doesn't depend on either, even though all three speak
/// `Content-Length`/line-framed JSON over stdio.
pub fn write_message<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

/// Read one DAP message's body (the bytes after the blank line),
/// skipping whatever headers precede it. `Content-Type` (when present)
/// is ignored; only `Content-Length` is required to know how much to
/// read.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut content_length = None;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream closed while reading DAP message headers",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            let value = value.trim();
            content_length = Some(value.parse::<usize>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid Content-Length header: {value:?}"),
                )
            })?);
        }
    }

    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "DAP message had no Content-Length header")
    })?;

    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn write_then_read_round_trips() {
        let mut buf = Vec::new();
        write_message(&mut buf, br#"{"hello":"world"}"#).unwrap();

        let mut reader = BufReader::new(Cursor::new(buf));
        let body = read_message(&mut reader).unwrap();
        assert_eq!(body, br#"{"hello":"world"}"#);
    }

    #[test]
    fn ignores_unrelated_headers() {
        let body = br#"{"a":1}"#;
        let raw = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut bytes = raw.into_bytes();
        bytes.extend_from_slice(body);

        let mut reader = BufReader::new(Cursor::new(bytes));
        assert_eq!(read_message(&mut reader).unwrap(), body);
    }

    #[test]
    fn two_messages_back_to_back() {
        let mut buf = Vec::new();
        write_message(&mut buf, b"one").unwrap();
        write_message(&mut buf, b"two").unwrap();

        let mut reader = BufReader::new(Cursor::new(buf));
        assert_eq!(read_message(&mut reader).unwrap(), b"one");
        assert_eq!(read_message(&mut reader).unwrap(), b"two");
    }

    #[test]
    fn missing_content_length_is_an_error() {
        let mut reader = BufReader::new(Cursor::new(b"\r\n".to_vec()));
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let mut reader = BufReader::new(Cursor::new(b"Content-Length: 10\r\n\r\nshort".to_vec()));
        assert!(read_message(&mut reader).is_err());
    }
}
