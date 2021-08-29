use std::{collections::HashMap, convert::TryInto};

use anyhow::Result;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}};

type ServerProxyMap = HashMap<String, (String, u16)>;

#[tokio::main]
async fn main() -> Result<()> {
    let config = tokio::fs::read_to_string("config.json").await?;
    let config: ServerProxyMap = serde_json::from_str(config.as_str())?;
    let config = Box::leak(Box::new(config)) as &_; // lives till it dies even without Box::leak so whatever
    let listener = TcpListener::bind("0.0.0.0:25565").await?;
    //println!("{:?}", config);

    loop {
        match listener.accept().await {
            Ok((socket, _)) => {tokio::spawn(async move {
                if let Err(err) = proxy_socket(socket, config).await {
                    eprintln!("Error while handling connection: {}", err);
                }
            });},
            Err(err) => eprintln!("Conenction failed: {}", err),
        }
    }
}

async fn read_bigendian_u16<T: tokio::io::AsyncRead + Unpin>(buf: &mut T) -> Result<u16> {
    Ok(((buf.read_u8().await? as u16) << 8) + buf.read_u8().await? as u16)
}

async fn write_bigendian_u16<T: tokio::io::AsyncWrite + Unpin>(buf: &mut T, n: u16) -> Result<()> {
    buf.write_u8((n >> 8) as u8).await?;
    buf.write_u8((n & 0xFF) as u8).await?;
    Ok(())
}

async fn proxy_socket(mut socket: TcpStream, config: &ServerProxyMap) -> Result<()> {
    //println!("Successfully opened connection");
    // assume that the client handshakes before encryption or compression
    let packet_len = read_varint(&mut socket).await?;
    if packet_len == 510_i32 { // Legacy ServerListPing packet; 510(0x01FE) when interpreted as VarInt
        return Err(anyhow::anyhow!("Recieved Legacy ServerListPing")); // can't deal with <1.6 protocols; abort connection
    }
    let packet_id = read_varint(&mut socket).await?;
    assert_eq!(packet_id, 0_i32); // Packet ID of handshake; abort prematurely when client dosn't send handshake for the first packet
    let protocol_version = read_varint(&mut socket).await?;
    let addr = read_string(&mut socket).await?;
    let port = read_bigendian_u16(&mut socket).await?;
    let hostname = format!("{}:{}", addr, port);
    let is_serverlistping = match read_varint(&mut socket).await? { // actually varint but indistinguishible from a byte because it's either 1 or 2
        1 => true,
        2 => false,
        _ => panic!("client sent invalid data for desired state"),
    };
    println!("{}",hostname);
    let target = config.get(&hostname).ok_or_else(|| anyhow::anyhow!("Host not in proxy table"))?;
    let mut server_socket = TcpStream::connect(target).await?;
    let mut handshake_buf = vec![];
    write_varint(&mut handshake_buf, 0_i32).await?;
    write_varint(&mut handshake_buf, protocol_version).await?;
    write_string(&mut handshake_buf, target.0.as_str()).await?;
    write_bigendian_u16(&mut handshake_buf, target.1).await?;
    write_varint(&mut handshake_buf, if is_serverlistping { 1 } else { 2 }).await?;
    write_varint(&mut server_socket, handshake_buf.len().try_into()?).await?;
    server_socket.write_all(handshake_buf.as_slice()).await?;
    tokio::io::copy_bidirectional(&mut socket, &mut server_socket).await?;
    //println!("Successfully closed connection");
    Ok(())
}

async fn read_varint<T: tokio::io::AsyncRead + Unpin>(buf: &mut T) -> Result<i32> {
    let mut res = 0i32;
    for i in 0..5 {
        let part = buf.read_u8().await?;
        res |= (part as i32 & 0x7F) << (7 * i);
        if part & 0x80 == 0 {
            return Ok(res);
        }
    }
    Err(anyhow::anyhow!("VarInt too big!"))
}

async fn read_string<T: tokio::io::AsyncRead + Unpin>(buf: &mut T) -> Result<String> {
    let len = read_varint(buf).await? as usize;
    let mut strbuf = vec![0; len as usize];
    buf.read_exact(&mut strbuf).await?;
    Ok(String::from_utf8(strbuf)?)
}

async fn write_varint<T: tokio::io::AsyncWrite + Unpin>(buf: &mut T, mut val: i32) -> Result<()> {
    for _ in 0..5 {
        if val & !0x7F == 0 {
            buf.write_u8(val as u8).await?;
            return Ok(());
        }
        buf.write_u8((val & 0x7F | 0x80) as u8).await?;
        val >>= 7;
    }
    Err(anyhow::anyhow!("VarInt too big!"))
}

async fn write_string<T: tokio::io::AsyncWrite + Unpin>(buf: &mut T, s: &str) -> Result<()> {
    write_varint(buf, s.len() as i32).await?;
    buf.write_all(s.as_bytes()).await?;
    Ok(())
}