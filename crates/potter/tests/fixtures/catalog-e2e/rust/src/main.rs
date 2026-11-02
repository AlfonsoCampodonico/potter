use std::io::{Read, Write};
use std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let listener = TcpListener::bind(format!("0.0.0.0:{port}"))?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 3\r\n\r\nok\n")?;
    }
    Ok(())
}
