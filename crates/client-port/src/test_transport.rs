//! A transport for this crate's tests: plain HTTP/1.1 to a fake daemon, nothing else.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::client::Delivery;
use crate::client::availability::ResponseObserver;
use crate::transport::{DirectMac, Local, Remote, Request, Transport};

pub struct Fake;

struct Http {
    base: String,
    timeout: Duration,
}

impl Http {
    fn exchange(&self, request: Request<'_>, observer: Option<&ResponseObserver>) -> Result<(u16, Vec<u8>)> {
        let address = self.base.strip_prefix("http://").context("an http base")?;
        let mut stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(self.timeout))?;
        let mut head = format!("{} {} HTTP/1.1\r\nHost: {address}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n", request.method, request.path, request.content_type, request.body.len());
        if let Some(operation) = request.operation {
            head.push_str(&format!("x-alondra-mcp-operation: {operation}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(request.body)?;
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        if reader.read_line(&mut status)? == 0 {
            bail!("the daemon closed the connection without an answer");
        }
        let code: u16 = status.split_whitespace().nth(1).context("a status")?.parse()?;
        let mut length = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse::<usize>().ok();
            }
        }
        if let Some(observer) = observer {
            observer.received();
        }
        let mut body = Vec::new();
        match length {
            Some(length) => {
                body.resize(length, 0);
                reader.read_exact(&mut body)?;
            }
            None => {
                reader.read_to_end(&mut body)?;
            }
        }
        Ok((code, body))
    }
}

impl Local for Http {
    fn send(&self, request: Request<'_>, observer: Option<&ResponseObserver>) -> Result<(u16, Vec<u8>)> {
        self.exchange(request, observer)
    }

    fn domain_get(&self, path: &str, _bearer: &str) -> Result<(u16, Vec<u8>)> {
        self.exchange(Request { method: "GET", path, content_type: "application/json", body: &[], operation: None }, None)
    }

    fn domain_call(&self, request: Request<'_>, _bearer: &str) -> Result<(u16, Option<String>, Vec<u8>)> {
        let (status, body) = self.exchange(request, None)?;
        Ok((status, None, body))
    }

    fn bounded(&self, timeout: Duration) -> Arc<dyn Local> {
        Arc::new(Http { base: self.base.clone(), timeout })
    }
}

impl Transport for Fake {
    fn local(&self, timeout: Duration, _signed: bool) -> Arc<dyn Local> {
        Arc::new(Http { base: "http://127.0.0.1:9".into(), timeout })
    }

    fn at(&self, base: String) -> Arc<dyn Local> {
        Arc::new(Http { base, timeout: Duration::from_secs(5) })
    }

    fn relay(&self) -> Option<Arc<dyn Remote>> {
        None
    }

    fn relay_target(&self) -> Option<&'static str> {
        None
    }

    fn select_relay(&self, _mac: Option<&str>) -> Result<()> {
        bail!("no relay in tests")
    }

    fn relay_paired(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn relay_machines(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn connect_relay(&self, _mac: &str, _repair: bool) -> Result<()> {
        bail!("no relay in tests")
    }

    fn forget_relay(&self, _mac: &str) -> Result<bool> {
        Ok(false)
    }

    fn direct_macs(&self) -> Result<Vec<Arc<dyn DirectMac>>> {
        Ok(Vec::new())
    }

    fn connect_direct(&self, _host: Option<&str>, _repair: bool) -> Result<()> {
        bail!("no direct Macs in tests")
    }

    fn forget_direct(&self, _home: &Path, _mac: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn delivery(&self, cause: &(dyn std::error::Error + 'static)) -> Option<Delivery> {
        cause.downcast_ref::<std::io::Error>().map(|error| match error.kind() {
            std::io::ErrorKind::ConnectionRefused => Delivery::Unsent,
            _ => Delivery::Unanswered,
        })
    }
}
