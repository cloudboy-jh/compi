//! A small HTTP/1.1 client for the OpenCode server Compi started: JSON requests, one per
//! connection, and the server's event stream (server-sent events). The server listens
//! on the loopback interface (a Linux one in WSL is reached through WSL's forwarding of
//! it), behind the password Compi gave it, so nothing here speaks TLS or follows
//! redirects.
use base64::Engine as _;
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// How long connecting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an answer may take.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);
/// The event stream says something, a heartbeat at least, more often than this.
const EVENT_SILENCE: Duration = Duration::from_secs(90);
/// The largest answer read: a long session's messages.
const MAX_ANSWER_BYTES: u64 = 256 * 1024 * 1024;
/// The largest status line and headers read.
const MAX_HEAD_BYTES: u64 = 64 * 1024;
/// The user name OpenCode's server takes with its password.
const USER: &str = "opencode";

/// Where the server listens, and the password it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Server {
    /// `host:port`.
    address: String,
    /// The `Authorization` header's value.
    authorization: String,
}

impl Server {
    /// The server a line of OpenCode's output says listens (`… listening on
    /// http://127.0.0.1:4096`), behind `password`.
    pub(super) fn listening(line: &str, password: &str) -> Option<Self> {
        let (_, rest) = line.split_once("listening on http://")?;
        let address = rest
            .split(|c: char| c == '/' || c.is_whitespace())
            .next()
            .filter(|address| address.contains(':'))?;
        let credentials =
            base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{password}"));
        Some(Self {
            address: address.to_owned(),
            authorization: format!("Basic {credentials}"),
        })
    }
}

/// `method` on `path` with `body`: the JSON answered (`Null` for none), else what went
/// wrong.
pub(super) fn call(
    server: &Server,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    call_within(server, method, path, body, ANSWER_TIMEOUT)
}

/// `call`, its answer awaited for `timeout`: one OpenCode gives only once something
/// else happened (a sign-in finished in the browser).
pub(super) fn call_within(
    server: &Server,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Value, String> {
    let stream = connect(server, timeout)?;
    send(&stream, server, method, path, body, "application/json")?;
    let mut reader = BufReader::new(stream);
    let head = read_head(&mut reader)?;
    let mut answer = Vec::new();
    body_reader(reader, &head)
        .take(MAX_ANSWER_BYTES)
        .read_to_end(&mut answer)
        .map_err(|error| format!("OpenCode's answer broke off: {error}"))?;
    if !(200..300).contains(&head.status) {
        return Err(error_text(head.status, &answer));
    }
    if answer.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&answer).map_err(|error| format!("OpenCode answered no JSON: {error}"))
}

/// Reads the server's events on `path`, each to `event`, until it returns false or the
/// stream ends; the error says why it ended otherwise.
pub(super) fn subscribe(
    server: &Server,
    path: &str,
    mut event: impl FnMut(Value) -> bool,
) -> Result<(), String> {
    let stream = connect(server, EVENT_SILENCE)?;
    send(&stream, server, "GET", path, None, "text/event-stream")?;
    let mut reader = BufReader::new(stream);
    let head = read_head(&mut reader)?;
    if !(200..300).contains(&head.status) {
        let mut answer = Vec::new();
        let _ = body_reader(reader, &head)
            .take(MAX_HEAD_BYTES)
            .read_to_end(&mut answer);
        return Err(error_text(head.status, &answer));
    }
    let mut data = String::new();
    for line in BufReader::new(body_reader(reader, &head)).lines() {
        let line = line.map_err(|error| format!("OpenCode's events broke off: {error}"))?;
        if line.is_empty() {
            if !data.is_empty() {
                let parsed = serde_json::from_str(&data);
                data.clear();
                if let Ok(value) = parsed
                    && !event(value)
                {
                    return Ok(());
                }
            }
            continue;
        }
        // `event:`, `id:` and comments (heartbeats) say nothing Compi uses.
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    Ok(())
}

/// `pairs` as a query string, `?` first; empty for none.
pub(super) fn query(pairs: &[(&str, &str)]) -> String {
    let mut text = String::new();
    for (key, value) in pairs {
        text.push(if text.is_empty() { '?' } else { '&' });
        encode(&mut text, key);
        text.push('=');
        encode(&mut text, value);
    }
    text
}

/// `value` percent-encoded onto `text`, all but unreserved characters.
fn encode(text: &mut String, value: &str) {
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            text.push(char::from(byte));
        } else {
            text.push_str(&format!("%{byte:02X}"));
        }
    }
}

fn connect(server: &Server, timeout: Duration) -> Result<TcpStream, String> {
    let unreachable = |error: &dyn std::fmt::Display| {
        format!(
            "Couldn't reach OpenCode's server at {}: {error}",
            server.address
        )
    };
    let addresses = server
        .address
        .to_socket_addrs()
        .map_err(|error| unreachable(&error))?;
    let mut failure = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(timeout))
                    .and_then(|()| stream.set_write_timeout(Some(ANSWER_TIMEOUT)))
                    .map_err(|error| unreachable(&error))?;
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) => failure = Some(error),
        }
    }
    Err(match failure {
        Some(error) => unreachable(&error),
        None => unreachable(&"no address"),
    })
}

fn send(
    mut stream: &TcpStream,
    server: &Server,
    method: &str,
    path: &str,
    body: Option<&Value>,
    accept: &str,
) -> Result<(), String> {
    let body = body.map(Value::to_string);
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: {}\r\nAccept: {accept}\r\nConnection: close\r\n",
        server.address, server.authorization
    );
    match &body {
        Some(body) => request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )),
        None if method != "GET" => request.push_str("Content-Length: 0\r\n\r\n"),
        None => request.push_str("\r\n"),
    }
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|error| format!("Couldn't ask OpenCode's server: {error}"))
}

/// An answer's status and how its body comes.
#[derive(Debug, PartialEq)]
struct Head {
    status: u16,
    chunked: bool,
    length: Option<u64>,
}

fn read_head(reader: &mut impl BufRead) -> Result<Head, String> {
    let broke =
        |error: &dyn std::fmt::Display| format!("OpenCode's server answered badly: {error}");
    let mut reader = reader.take(MAX_HEAD_BYTES);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|error| broke(&error))?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| broke(&format!("no status in {:?}", line.trim())))?;
    let mut head = Head {
        status,
        chunked: false,
        length: None,
    };
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|error| broke(&error))? == 0 {
            return Err(broke(&"its headers broke off"));
        }
        let header = line.trim_end();
        if header.is_empty() {
            return Ok(head);
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            head.chunked = value.to_ascii_lowercase().contains("chunked");
        } else if name.eq_ignore_ascii_case("content-length") {
            head.length = value.parse().ok();
        }
    }
}

/// The body after `head`: chunked, of a known length, or to the end of the connection.
fn body_reader<R: BufRead + 'static>(reader: R, head: &Head) -> Box<dyn Read> {
    if head.chunked {
        Box::new(Chunked {
            inner: reader,
            left: 0,
            done: false,
        })
    } else if let Some(length) = head.length {
        Box::new(reader.take(length))
    } else {
        Box::new(reader)
    }
}

/// A chunked body as the bytes it carries.
struct Chunked<R> {
    inner: R,
    /// Bytes left in the chunk being read.
    left: u64,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.done || buffer.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut size = String::new();
            self.inner.read_line(&mut size)?;
            let size = size.trim();
            let size = size.split(';').next().unwrap_or_default().trim();
            self.left = u64::from_str_radix(size, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let wanted = buffer
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= read as u64;
        if self.left == 0 {
            // The chunk's closing line break.
            let mut end = String::new();
            self.inner.read_line(&mut end)?;
        }
        Ok(read)
    }
}

/// What a failed request's answer says: OpenCode's message (`message`, `data.message`,
/// `error`), else the status and the answer's start.
fn error_text(status: u16, answer: &[u8]) -> String {
    let parsed: Option<Value> = serde_json::from_slice(answer).ok();
    let message = parsed.as_ref().and_then(|value| {
        [
            &value["message"],
            &value["data"]["message"],
            &value["error"]["message"],
            &value["error"],
        ]
        .into_iter()
        .find_map(|text| text.as_str().map(str::trim).filter(|text| !text.is_empty()))
        .map(str::to_owned)
    });
    message.unwrap_or_else(|| {
        let text = String::from_utf8_lossy(answer);
        let text: String = text.trim().chars().take(200).collect();
        if text.is_empty() {
            format!("OpenCode's server answered {status}")
        } else {
            format!("OpenCode's server answered {status}: {text}")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_line_names_its_address_and_the_password_goes_with_every_request() {
        let server = Server::listening(
            "opencode server listening on http://127.0.0.1:4096",
            "secret",
        )
        .unwrap();
        assert_eq!(server.address, "127.0.0.1:4096");
        assert_eq!(server.authorization, "Basic b3BlbmNvZGU6c2VjcmV0");
        assert_eq!(
            Server::listening("server listening on http://127.0.0.1:45753/", "x")
                .unwrap()
                .address,
            "127.0.0.1:45753"
        );
        assert_eq!(Server::listening("server password abc", "x"), None);
    }

    #[test]
    fn queries_encode_everything_but_unreserved_characters() {
        assert_eq!(
            query(&[("location[directory]", r"C:\a b"), ("limit", "5")]),
            "?location%5Bdirectory%5D=C%3A%5Ca%20b&limit=5"
        );
        assert_eq!(query(&[]), "");
    }

    #[test]
    fn heads_and_chunked_bodies_read_as_sent() {
        let answer = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/event-stream\r\n\r\n6\r\ndata: \r\n6\r\n{\"a\":1\r\n3\r\n}\n\n\r\n0\r\n\r\n";
        let mut reader = BufReader::new(io::Cursor::new(answer.as_bytes().to_vec()));
        let head = read_head(&mut reader).unwrap();
        assert_eq!(
            head,
            Head {
                status: 200,
                chunked: true,
                length: None
            }
        );
        let mut body = String::new();
        body_reader(reader, &head)
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, "data: {\"a\":1}\n\n");
    }

    #[test]
    fn errors_say_opencode_s_own_message() {
        assert_eq!(
            error_text(
                400,
                br#"{"_tag":"InvalidRequestError","message":"Invalid session ID"}"#
            ),
            "Invalid session ID"
        );
        assert_eq!(
            error_text(500, br#"{"name":"UnknownError","data":{"message":"boom"}}"#),
            "boom"
        );
        assert_eq!(error_text(404, b""), "OpenCode's server answered 404");
    }
}
