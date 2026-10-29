use anyhow::Result;
use nom::{
    bytes::{self, complete::tag},
    combinator::{flat_map, map, map_res},
    number,
    sequence::{preceded, tuple},
    IResult,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

/// Discover LMS servers via SqueezeNetwork discovery (UDP 3483).
/// Returns host + JSON port for the first server that answers within 5s.
/// Prefer localhost when a local server is also answering.
pub async fn discover() -> Result<HashMap<String, String>> {
    let servers = discover_all(Duration::from_secs(3)).await.unwrap_or_default();

    // Prefer 127.0.0.1 / localhost if present
    if let Some(local) = servers.iter().find(|s| {
        let h = s.get("host").map(|x| x.as_str()).unwrap_or("");
        h == "127.0.0.1" || h == "::1" || h.eq_ignore_ascii_case("localhost")
    }) {
        return Ok(local.clone());
    }
    if let Some(first) = servers.into_iter().next() {
        return Ok(first);
    }
    Ok(HashMap::new())
}

/// Collect discovery replies for up to `budget`.
pub async fn discover_all(budget: Duration) -> Result<Vec<HashMap<String, String>>> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.set_broadcast(true)?;
    let message = b"eNAME\0JSON\0UUID\0VERS\0IPAD\0";
    // 255.255.255.255 is dropped on some Catalina interfaces. Also send each
    // subnet broadcast (ifconfig "broadcast …").
    for target in broadcast_targets() {
        let _ = sock.send_to(message, target.as_str()).await;
    }

    let mut found: Vec<HashMap<String, String>> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let deadline = Instant::now() + budget;
    let mut buf = vec![0u8; 2048];

    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left.min(Duration::from_millis(400)), sock.recv_from(&mut buf)).await {
            Ok(Ok((n, addr))) => {
                let ip = addr.ip().to_string();
                let port = parse_reply(&buf[..n]).map(|(_, p)| p).unwrap_or(9000);
                let name = parse_name(&buf[..n]).unwrap_or_default();
                let key = format!("{}:{}", ip, port);
                if seen.insert(key) {
                    let mut server: HashMap<String, String> = HashMap::new();
                    server.insert("host".to_string(), ip);
                    server.insert("port".to_string(), port.to_string());
                    if !name.is_empty() {
                        server.insert("name".to_string(), name);
                    }
                    found.push(server);
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue, // timeout slice — keep listening
        }
    }

    // A local server does not always answer a broadcast aimed at other interfaces.
    if !seen.contains("127.0.0.1:9000") {
        let open = tokio::time::timeout(
            Duration::from_millis(400),
            tokio::net::TcpStream::connect("127.0.0.1:9000"),
        )
        .await;
        if matches!(open, Ok(Ok(_))) {
            let mut server: HashMap<String, String> = HashMap::new();
            server.insert("host".to_string(), "127.0.0.1".to_string());
            server.insert("port".to_string(), "9000".to_string());
            server.insert("name".to_string(), "This computer".to_string());
            found.push(server);
        }
    }

    Ok(found)
}

/// Limited broadcast plus each IPv4 interface broadcast address.
fn broadcast_targets() -> Vec<String> {
    let mut out = vec!["255.255.255.255:3483".to_string()];
    let Ok(output) = std::process::Command::new("ifconfig").output() else {
        return out;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let t = line.trim();
        let Some(rest) = t.split("broadcast ").nth(1) else {
            continue;
        };
        let ip = rest.split_whitespace().next().unwrap_or("");
        if ip.is_empty() || ip == "0.0.0.0" {
            continue;
        }
        let dest = format!("{ip}:3483");
        if !out.iter().any(|x| x == &dest) {
            out.push(dest);
        }
    }
    out
}

fn parse_tag<'a>(input: &'a [u8], start_tag: &str) -> IResult<&'a [u8], String> {
    map_res(
        preceded(
            tag(start_tag),
            flat_map(number::complete::be_u8, bytes::complete::take),
        ),
        |bytes: &[u8]| String::from_utf8(bytes.to_vec()),
    )(input)
}

fn parse_hostname(input: &[u8]) -> IResult<&[u8], String> {
    parse_tag(input, "ENAME")
}

fn parse_port(input: &[u8]) -> IResult<&[u8], u16> {
    map_res(
        |input| parse_tag(input, "JSON"),
        |s| s.parse::<u16>(),
    )(input)
}

fn parse_reply(input: &[u8]) -> IResult<&[u8], u16> {
    map(tuple((parse_hostname, parse_port)), |(_, port)| port)(input)
}

fn parse_name(input: &[u8]) -> Option<String> {
    parse_hostname(input).ok().map(|(_, n)| n)
}
