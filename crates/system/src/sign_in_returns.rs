//! Hands the browser's return from an MCP server's sign-in to Claude's own listener on this Mac.
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// How long Claude's listener may take to exchange the code before it answers the browser.
const EXCHANGE: Duration = Duration::from_secs(30);

/// Sends the return to Claude's listener and reads the status it answers with, once the exchange
/// is over.
pub fn hand_back(port: u16, target: &str) -> Result<u16, String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, EXCHANGE).map_err(|_| "Claude no longer waits for this sign-in".to_owned())?;
    stream.set_read_timeout(Some(EXCHANGE)).map_err(|error| error.to_string())?;
    stream.set_write_timeout(Some(EXCHANGE)).map_err(|error| error.to_string())?;
    let request = format!("GET {} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n", target, port);
    stream.write_all(request.as_bytes()).map_err(|error| error.to_string())?;
    let mut head = [0_u8; 64];
    let read = stream.read(&mut head).map_err(|_| "Claude did not answer the sign-in".to_owned())?;
    let line = String::from_utf8_lossy(&head[..read]);
    let status = line.split_whitespace().nth(1).and_then(|code| code.parse::<u16>().ok());
    status.ok_or_else(|| "Claude did not answer the sign-in".to_owned())
}

/// Whether something listens on `port` of this Mac, as a browser would reach it. An empty
/// connection is what a browser opens ahead of a request; nothing is sent on it.
pub fn listening(port: u16) -> bool {
    use std::net::ToSocketAddrs as _;
    let addresses: Vec<SocketAddr> = ("localhost", port).to_socket_addrs().map(Iterator::collect).unwrap_or_default();
    addresses.iter().any(|address| TcpStream::connect_timeout(address, Duration::from_millis(300)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead as _;

    #[test]
    fn the_return_reaches_claudes_listener_as_a_browser_sends_it_and_its_status_comes_back() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let claude = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            stream.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n").unwrap();
            line.trim_end().to_owned()
        });
        assert_eq!(hand_back(port, "/callback?code=c&state=s1"), Ok(302));
        assert_eq!(claude.join().unwrap(), "GET /callback?code=c&state=s1 HTTP/1.1");
    }

    #[test]
    fn a_listener_that_is_gone_is_told_apart() {
        let open = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(listening(open.local_addr().unwrap().port()));
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(!listening(port));
        assert_eq!(hand_back(port, "/callback?code=c&state=s1"), Err("Claude no longer waits for this sign-in".to_owned()));
    }
}
