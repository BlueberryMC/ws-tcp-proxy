use futures_util::{SinkExt, StreamExt};
use std::env;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_hdr_async, connect_async};
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_tungstenite::tungstenite::handshake::server::Request;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 || args.len() > 5 {
        eprintln!("Usage: {} <mode> <listen_addr> <target> [--haproxy-v2]", args[0]);
        eprintln!("Modes:");
        eprintln!("  tcp-ws  (TCP in -> WebSocket out)");
        eprintln!("  ws-tcp  (WebSocket in -> TCP out)");
        eprintln!("Examples:");
        eprintln!("  {} tcp-ws 127.0.0.1:9000 ws://127.0.0.1:8080", args[0]);
        eprintln!("  {} ws-tcp 127.0.0.1:8080 127.0.0.1:9000", args[0]);
        eprintln!("  {} ws-tcp 127.0.0.1:8080 127.0.0.1:9000 --haproxy-v2", args[0]);
        std::process::exit(2);
    }

    let mode = args[1].clone();
    let listen_addr = &args[2];
    let target = args[3].clone();
    let haproxy_v2 = args.get(4).map(|v| v.as_str()) == Some("--haproxy-v2")
        || args.get(4).map(|v| v.as_str()) == Some("--haproxy");

    if haproxy_v2 && mode != "ws-tcp" {
        eprintln!("--haproxy-v2 is only supported in ws-tcp mode");
        std::process::exit(2);
    }

    let listener = TcpListener::bind(listen_addr).await?;
    println!("Listening on {}", listen_addr);

    loop {
        let (tcp, peer) = listener.accept().await?;
        let target = target.clone();
        let mode = mode.clone();
        let haproxy_v2 = haproxy_v2;
        println!("Accepted {}", peer);
        tokio::spawn(async move {
            let result = match mode.as_str() {
                "tcp-ws" => proxy_tcp_to_ws(tcp, target).await,
                "ws-tcp" => proxy_ws_to_tcp(tcp, target, haproxy_v2).await,
                _ => {
                    eprintln!("Unknown mode: {}", mode);
                    return;
                }
            };

            if let Err(err) = result {
                eprintln!("Connection error: {}", err);
            }
        });
    }
}

async fn proxy_tcp_to_ws(mut tcp: TcpStream, ws_url: String) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ws_stream, _) = connect_async(ws_url).await?;
    let (mut tcp_read, mut tcp_write) = tcp.split();
    let mut buf = vec![0u8; 8192];

    loop {
        tokio::select! {
            read_res = tcp_read.read(&mut buf) => {
                let n = read_res?;
                if n == 0 {
                    let _ = ws_stream.send(Message::Close(None)).await;
                    break;
                }
                ws_stream.send(Message::Binary(Bytes::from(buf[..n].to_vec()))).await?;
            }
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        tcp_write.write_all(&data).await?;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        ws_stream.send(Message::Pong(payload)).await?;
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(err.into()),
                    None => break,
                }
            }
        }
    }

    Ok(())
}

async fn proxy_ws_to_tcp(
    tcp: TcpStream,
    target_addr: String,
    haproxy_v2: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let peer_addr = tcp.peer_addr()?;
    let header_ip = std::sync::Arc::new(std::sync::Mutex::new(None));
    let header_ip_capture = header_ip.clone();
    let mut ws_stream = accept_hdr_async(tcp, |req: &Request, resp| {
        if let Some(value) = req.headers().get("cf-connecting-ip") {
            if let Ok(value) = value.to_str() {
                if let Ok(ip) = value.parse() {
                    println!("Proxying from {}", ip);
                    if let Ok(mut guard) = header_ip_capture.lock() {
                        *guard = Some(ip);
                    }
                }
            }
        }
        Ok(resp)
    })
    .await?;
    let mut tcp_out = TcpStream::connect(target_addr).await?;
    if haproxy_v2 {
        let header_ip = header_ip.lock().ok().and_then(|g| *g);
        let src_ip = header_ip.unwrap_or_else(|| peer_addr.ip());
        let src_port = if header_ip.is_some() { 0 } else { peer_addr.port() };
        let dst_addr = tcp_out.peer_addr()?;
        let header = build_haproxy_v2_header(src_ip, src_port, dst_addr.ip(), dst_addr.port());
        tcp_out.write_all(&header).await?;
    }
    let (mut tcp_read, mut tcp_write) = tcp_out.split();
    let mut buf = vec![0u8; 8192];

    loop {
        tokio::select! {
            read_res = tcp_read.read(&mut buf) => {
                let n = read_res?;
                if n == 0 {
                    let _ = ws_stream.send(Message::Close(None)).await;
                    break;
                }
                ws_stream.send(Message::Binary(Bytes::from(buf[..n].to_vec()))).await?;
            }
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        tcp_write.write_all(&data).await?;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        ws_stream.send(Message::Pong(payload)).await?;
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(Box::from(err)),
                    None => break,
                }
            }
        }
    }

    Ok(())
}

fn build_haproxy_v2_header(
    src_ip: std::net::IpAddr,
    src_port: u16,
    dst_ip: std::net::IpAddr,
    dst_port: u16,
) -> Vec<u8> {
    const SIG: [u8; 12] = [0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a];
    let mut out = Vec::with_capacity(16 + 36);
    out.extend_from_slice(&SIG);
    out.push(0x21); // version 2, PROXY command

    let (src_ip, dst_ip) = normalize_ip_families(src_ip, dst_ip);

    match (src_ip, dst_ip) {
        (std::net::IpAddr::V4(src), std::net::IpAddr::V4(dst)) => {
            out.push(0x11); // TCP over IPv4
            out.extend_from_slice(&12u16.to_be_bytes());
            out.extend_from_slice(&src.octets());
            out.extend_from_slice(&dst.octets());
            out.extend_from_slice(&src_port.to_be_bytes());
            out.extend_from_slice(&dst_port.to_be_bytes());
        }
        (std::net::IpAddr::V6(src), std::net::IpAddr::V6(dst)) => {
            out.push(0x21); // TCP over IPv6
            out.extend_from_slice(&36u16.to_be_bytes());
            out.extend_from_slice(&src.octets());
            out.extend_from_slice(&dst.octets());
            out.extend_from_slice(&src_port.to_be_bytes());
            out.extend_from_slice(&dst_port.to_be_bytes());
        }
        _ => {
            out.push(0x00); // UNSPEC
            out.extend_from_slice(&0u16.to_be_bytes());
        }
    }

    out
}

fn normalize_ip_families(
    src_ip: std::net::IpAddr,
    dst_ip: std::net::IpAddr,
) -> (std::net::IpAddr, std::net::IpAddr) {
    match (src_ip, dst_ip) {
        (std::net::IpAddr::V6(src), std::net::IpAddr::V4(dst)) => {
            (std::net::IpAddr::V6(src), std::net::IpAddr::V6(v4_mapped_v6(dst)))
        }
        (std::net::IpAddr::V4(src), std::net::IpAddr::V6(dst)) => {
            (std::net::IpAddr::V6(v4_mapped_v6(src)), std::net::IpAddr::V6(dst))
        }
        _ => (src_ip, dst_ip),
    }
}

fn v4_mapped_v6(v4: std::net::Ipv4Addr) -> std::net::Ipv6Addr {
    let octets = v4.octets();
    std::net::Ipv6Addr::from([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, octets[0], octets[1], octets[2], octets[3],
    ])
}
