//! Servidor HTTP minimal para recibir el callback OAuth en `localhost`.
//!
//! Atiende **una sola** conexión esperada (`/auth/callback?code=...&state=...`),
//! responde un HTML estático de éxito o error y libera el puerto. No es un
//! servidor de propósito general: parsea solo lo justo de HTTP/1.1.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

use crate::{Error, Result};

/// Resultado del callback parseado del query string.
#[derive(Debug, Clone)]
pub struct CallbackResult {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Servidor de un solo uso: bind, escucha una conexión, devuelve el resultado.
pub struct LocalCallbackServer;

impl LocalCallbackServer {
    /// Bind a `127.0.0.1:port` (0 = puerto libre asignado por el SO). Devuelve
    /// la URI de redirección (`http://127.0.0.1:<port>/auth/callback`) y un
    /// futuro que se resuelve cuando el navegador toca el endpoint o vence el
    /// timeout (5 minutos por defecto).
    pub async fn bind(port: u16) -> Result<BoundServer> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| Error::Oauth(format!("bind {port}: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| Error::Oauth(format!("local_addr: {e}")))?;
        let redirect_uri = format!("http://127.0.0.1:{}/auth/callback", addr.port());
        Ok(BoundServer {
            listener,
            addr,
            redirect_uri,
        })
    }
}

/// Servidor ya enlazado a un puerto. La `redirect_uri` puede inyectarse en la
/// URL de autorización antes de empezar a escuchar.
pub struct BoundServer {
    listener: TcpListener,
    addr: SocketAddr,
    pub redirect_uri: String,
}

impl BoundServer {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Espera una conexión, parsea el query del callback y responde el HTML
    /// correspondiente. `wait` acota el tiempo total que se mantiene escuchando.
    pub async fn await_callback(self, wait: Duration) -> Result<CallbackResult> {
        let accepted = timeout(wait, self.listener.accept()).await.map_err(|_| {
            Error::Oauth("se agotó el tiempo de espera del callback (5 min)".into())
        })?;
        let (mut socket, _peer) = accepted.map_err(|e| Error::Oauth(format!("accept: {e}")))?;

        // Leemos lo justo para parsear la primera línea de la request. Una
        // request de callback típica entra en pocos cientos de bytes; con 4 KiB
        // sobra.
        let mut buf = [0u8; 4096];
        let n = socket
            .read(&mut buf)
            .await
            .map_err(|e| Error::Oauth(format!("read: {e}")))?;
        let request = String::from_utf8_lossy(&buf[..n]);

        let parsed = parse_callback(&request);
        let body = if let Some(err) = parsed.error.as_deref() {
            html_error(parsed.error_description.as_deref().unwrap_or(err))
        } else if parsed.code.is_some() {
            HTML_SUCCESS.to_string()
        } else {
            html_error("falta el parámetro `code`")
        };

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
        Ok(parsed)
    }
}

/// Parsea la primera línea `GET /auth/callback?... HTTP/1.1` y extrae query.
fn parse_callback(request: &str) -> CallbackResult {
    let mut empty = CallbackResult {
        code: None,
        state: None,
        error: None,
        error_description: None,
    };
    let Some(first_line) = request.lines().next() else {
        return empty;
    };
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return empty;
    }
    let target = parts[1];
    let Some(query_idx) = target.find('?') else {
        return empty;
    };
    let query = &target[query_idx + 1..];
    let params = parse_query(query);
    empty.code = params.get("code").cloned();
    empty.state = params.get("state").cloned();
    empty.error = params.get("error").cloned();
    empty.error_description = params.get("error_description").cloned();
    empty
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            let key = url_decode(it.next()?);
            let value = url_decode(it.next().unwrap_or(""));
            Some((key, value))
        })
        .collect()
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) =
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
                {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

const HTML_SUCCESS: &str = r##"<!doctype html>
<html><head><title>xiě-code · Autorización completada</title>
<style>body{font-family:system-ui,-apple-system,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#131010;color:#f1ecec}.c{text-align:center;padding:2rem}h1{margin-bottom:.5rem}p{color:#b7b1b1}</style>
</head><body><div class="c"><h1>Autorización completada</h1><p>Ya puedes cerrar esta ventana y volver a xiě-code.</p></div>
<script>setTimeout(()=>window.close(),1500)</script></body></html>"##;

fn html_error(message: &str) -> String {
    format!(
        r##"<!doctype html><html><head><title>xiě-code · Error de autorización</title>
<style>body{{font-family:system-ui,-apple-system,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#131010;color:#f1ecec}}.c{{text-align:center;padding:2rem}}h1{{color:#fc533a}}.e{{color:#ff917b;font-family:monospace;margin-top:1rem;padding:1rem;background:#3c140d;border-radius:.5rem}}</style>
</head><body><div class="c"><h1>Autorización fallida</h1><div class="e">{}</div></div></body></html>"##,
        html_escape(message)
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_query_and_extracts_code_state() {
        let r = "GET /auth/callback?code=abc&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let p = parse_callback(r);
        assert_eq!(p.code.as_deref(), Some("abc"));
        assert_eq!(p.state.as_deref(), Some("xyz"));
        assert!(p.error.is_none());
    }

    #[test]
    fn parses_error_with_description() {
        let r = "GET /auth/callback?error=access_denied&error_description=user%20cancel HTTP/1.1\r\n\r\n";
        let p = parse_callback(r);
        assert_eq!(p.error.as_deref(), Some("access_denied"));
        assert_eq!(p.error_description.as_deref(), Some("user cancel"));
    }

    #[test]
    fn url_decode_handles_percent_and_plus() {
        assert_eq!(url_decode("hello+world"), "hello world");
        assert_eq!(url_decode("a%2Fb"), "a/b");
        assert_eq!(url_decode("a%2"), "a%2"); // malformado: pasa tal cual
    }
}
