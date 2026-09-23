// DAP's fixed wire framing: `Content-Length: <n>\r\n\r\n<n bytes of JSON>`,
// repeated for every message in both directions - not something specific to
// this adapter, just the protocol's own transport layer (see the DAP spec's
// "Base Protocol" section).

use std::io::{self, BufRead, Write};

pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<serde_json::Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return Ok(None); // EOF - the client closed stdin
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break; // blank line ends the header block
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length header")
            })?);
        }
    }
    let content_length = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body) //
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(value))
}

pub fn write_message<W: Write>(writer: &mut W, value: &serde_json::Value) -> io::Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}
