//! Port forwards over SSH: parsing what the user types, and the `ssh`
//! options that carry them.
//!
//! Every forward listens on the loopback address (127.0.0.1) unless the
//! caller names a bind address, so nothing becomes reachable from the
//! network by default. The other end is always the loopback address of the
//! machine that is forwarded to.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

use crate::error::AppError;

/// The address a forward listens on when the caller names none.
pub const DEFAULT_BIND: &str = "127.0.0.1";

/// Which side listens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// A port on this machine reaches a port on the sandbox (`ssh -L`).
    Local,
    /// A port on the sandbox reaches a port on this machine (`ssh -R`).
    Reverse,
}

/// One forward: `listen` on `bind` (on the listening side) reaches `target`
/// on the other side's loopback address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    pub direction: Direction,
    pub bind: String,
    pub listen: u16,
    pub target: u16,
}

impl Forward {
    /// A local forward of `local` on this machine to sandbox port `remote`.
    pub fn local(bind: Option<String>, local: u16, remote: u16) -> Self {
        Self {
            direction: Direction::Local,
            bind: bind.unwrap_or_else(|| DEFAULT_BIND.to_string()),
            listen: local,
            target: remote,
        }
    }

    /// `ssh` arguments for this forward (`-L` or `-R` and its spec).
    pub fn ssh_args(&self) -> [String; 2] {
        let flag = match self.direction {
            Direction::Local => "-L",
            Direction::Reverse => "-R",
        };
        let bind = if self.bind.contains(':') {
            format!("[{}]", self.bind)
        } else {
            self.bind.clone()
        };
        [
            flag.to_string(),
            format!("{bind}:{}:127.0.0.1:{}", self.listen, self.target),
        ]
    }

    /// Whether this forward listens on a loopback address only.
    pub fn is_loopback(&self) -> bool {
        self.bind == "localhost"
            || self
                .bind
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    }

    /// Fail when a local forward's port cannot be bound on this machine (it
    /// is taken), before any connection is made.
    pub fn check_local_port(&self) -> Result<(), AppError> {
        if self.direction != Direction::Local {
            return Ok(());
        }
        self.bind_listener().map(drop)
    }

    /// Reserve a local listener until the dedicated SSH connection takes over.
    pub fn reserve_local(&mut self) -> Result<TcpListener, AppError> {
        let listener = self.bind_listener()?;
        self.listen = listener
            .local_addr()
            .map_err(|err| AppError::io("reading allocated port", err))?
            .port();
        Ok(listener)
    }

    pub fn url(&self) -> String {
        let host = match self.bind.as_str() {
            "*" | "0.0.0.0" => "127.0.0.1".into(),
            "::" => "[::1]".into(),
            other if other.contains(':') => format!("[{other}]"),
            other => other.to_string(),
        };
        format!("http://{host}:{}", self.listen)
    }

    fn bind_listener(&self) -> Result<TcpListener, AppError> {
        let address = match self.bind.as_str() {
            "localhost" => IpAddr::V4(Ipv4Addr::LOCALHOST),
            "*" => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            other => other
                .parse()
                .map_err(|_| AppError::config(format!("{other} is not a bind address")))?,
        };
        TcpListener::bind(SocketAddr::new(address, self.listen)).map_err(|error| {
            AppError::new(
                "port_in_use",
                format!(
                    "cannot listen on {}:{} on this machine: {error}",
                    self.bind, self.listen
                ),
            )
            .with_hint("stop what uses the port, or forward from another local port")
        })
    }
}

impl fmt::Display for Forward {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.direction {
            Direction::Local => write!(
                f,
                "{}:{} -> sandbox port {}",
                self.bind, self.listen, self.target
            ),
            Direction::Reverse => write!(
                f,
                "sandbox {}:{} -> local port {}",
                self.bind, self.listen, self.target
            ),
        }
    }
}

/// `serve --forward [BIND:]LOCAL:REMOTE`.
pub fn parse_local(value: &str) -> Result<Forward, String> {
    let (bind, listen, target) = parse_triple(value, "[BIND:]LOCAL:REMOTE", true)?;
    Ok(Forward {
        direction: Direction::Local,
        bind: bind.unwrap_or_else(|| DEFAULT_BIND.to_string()),
        listen,
        target,
    })
}

/// `serve --reverse [BIND:]REMOTE:LOCAL`.
pub fn parse_reverse(value: &str) -> Result<Forward, String> {
    let (bind, listen, target) = parse_triple(value, "[BIND:]REMOTE:LOCAL", false)?;
    Ok(Forward {
        direction: Direction::Reverse,
        bind: bind.unwrap_or_else(|| DEFAULT_BIND.to_string()),
        listen,
        target,
    })
}

/// `forward REMOTE[:LOCAL]`: the sandbox port and the local port (the same
/// number when no local port is given).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortPair {
    pub remote: u16,
    pub local: u16,
}

pub fn parse_pair(value: &str) -> Result<PortPair, String> {
    let shape = || format!("expected REMOTE[:LOCAL] (ports), got {value:?}");
    let mut parts = value.split(':');
    let remote = parse_port(parts.next().unwrap_or_default()).map_err(|_| shape())?;
    let local = match parts.next() {
        Some(local) => parse_port(local).map_err(|_| shape())?,
        None => remote,
    };
    if parts.next().is_some() {
        return Err(format!("{}; pass a bind address with --bind", shape()));
    }
    Ok(PortPair { remote, local })
}

/// A bind address: an IP address, `localhost`, or `*` (every address).
pub fn parse_bind(value: &str) -> Result<String, String> {
    let value = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(value);
    if value == "localhost" || value == "*" || value.parse::<IpAddr>().is_ok() {
        Ok(value.to_string())
    } else {
        Err(format!(
            "{value:?} is not a bind address (an IP address, localhost or *)"
        ))
    }
}

pub fn parse_local_port(value: &str) -> Result<u16, String> {
    if value == "auto" {
        Ok(0)
    } else {
        parse_port(value)
    }
}

/// A port number from 1 to 65535.
pub fn parse_port(value: &str) -> Result<u16, String> {
    match value.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(format!("{value:?} is not a port (1 to 65535)")),
    }
}

/// `[BIND:]A:B`, where BIND may be a bracketed IPv6 address.
fn parse_triple(
    value: &str,
    shape: &str,
    local: bool,
) -> Result<(Option<String>, u16, u16), String> {
    let wrong = || format!("expected {shape}, got {value:?}");
    let (bind, ports) = if let Some(rest) = value.strip_prefix('[') {
        let (address, ports) = rest.split_once("]:").ok_or_else(wrong)?;
        (Some(address), ports)
    } else {
        match value.matches(':').count() {
            1 => (None, value),
            2 => {
                let (address, ports) = value.split_once(':').ok_or_else(wrong)?;
                (Some(address), ports)
            }
            _ => return Err(wrong()),
        }
    };
    let (a, b) = ports.split_once(':').ok_or_else(wrong)?;
    let bind = bind.map(parse_bind).transpose()?;
    Ok((
        bind,
        (if local {
            parse_local_port(a)
        } else {
            parse_port(a)
        })
        .map_err(|error| format!("{}: {error}", wrong()))?,
        parse_port(b).map_err(|error| format!("{}: {error}", wrong()))?,
    ))
}

/// Ports the provider uses on every sandbox, which cannot be exposed.
pub const RESERVED_PORTS: [u16; 4] = [22, 6080, 8090, 8911];
/// A range of ports the provider uses on every sandbox (inclusive).
pub const RESERVED_RANGE: (u16, u16) = (47984, 48010);

pub fn is_reserved(port: u16) -> bool {
    RESERVED_PORTS.contains(&port) || (RESERVED_RANGE.0..=RESERVED_RANGE.1).contains(&port)
}

/// `expose PORT`: a port that is not one the provider reserves.
pub fn parse_expose_port(value: &str) -> Result<u16, String> {
    let port = parse_port(value)?;
    if is_reserved(port) {
        return Err(format!(
            "port {port} is reserved by the sandbox provider (22, 6080, 8090, 8911 and \
             {} to {}); run the service on another port",
            RESERVED_RANGE.0, RESERVED_RANGE.1
        ));
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_forward_binds_loopback_unless_a_bind_address_is_given() {
        let plain = parse_local("8000:9000").unwrap();
        assert_eq!(plain.bind, "127.0.0.1");
        assert_eq!((plain.listen, plain.target), (8000, 9000));
        assert!(plain.is_loopback());
        assert_eq!(plain.ssh_args(), ["-L", "127.0.0.1:8000:127.0.0.1:9000"]);

        let open = parse_local("0.0.0.0:8000:8000").unwrap();
        assert_eq!(open.bind, "0.0.0.0");
        assert!(!open.is_loopback());
        assert_eq!(open.ssh_args(), ["-L", "0.0.0.0:8000:127.0.0.1:8000"]);

        let v6 = parse_local("[::1]:8000:80").unwrap();
        assert_eq!(v6.bind, "::1");
        assert!(v6.is_loopback());
        assert_eq!(v6.ssh_args(), ["-L", "[::1]:8000:127.0.0.1:80"]);

        let any = parse_local("*:8000:80").unwrap();
        assert_eq!(any.bind, "*");
        assert!(!any.is_loopback());

        assert_eq!(
            Forward::local(None, 3000, 3001).ssh_args(),
            ["-L", "127.0.0.1:3000:127.0.0.1:3001"]
        );
        assert_eq!(
            Forward::local(Some("localhost".into()), 3000, 3001).bind,
            "localhost"
        );
    }

    #[test]
    fn a_reverse_forward_listens_on_the_sandbox_loopback_by_default() {
        let reverse = parse_reverse("20000:7777").unwrap();
        assert_eq!(reverse.direction, Direction::Reverse);
        assert_eq!(reverse.bind, "127.0.0.1");
        assert_eq!(reverse.ssh_args(), ["-R", "127.0.0.1:20000:127.0.0.1:7777"]);
        assert_eq!(
            reverse.to_string(),
            "sandbox 127.0.0.1:20000 -> local port 7777"
        );
        // A reverse forward needs no free local port.
        assert!(reverse.check_local_port().is_ok());
    }

    #[test]
    fn malformed_forwards_are_refused() {
        for bad in [
            "",
            "8000",
            "8000:",
            ":8000",
            "0:8000",
            "8000:70000",
            "a:b",
            "host.example:1:2",
            "1:2:3:4",
            "[::1:8000:80",
            "[nonsense]:1:2",
        ] {
            assert!(parse_local(bad).is_err(), "accepted {bad:?}");
            assert!(parse_reverse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn forward_takes_the_remote_port_first_and_defaults_the_local_one() {
        assert_eq!(
            parse_pair("8000").unwrap(),
            PortPair {
                remote: 8000,
                local: 8000
            }
        );
        assert_eq!(
            parse_pair("5432:15432").unwrap(),
            PortPair {
                remote: 5432,
                local: 15432
            }
        );
        for bad in ["", "0", "8000:", "x", "1:2:3", "127.0.0.1:1:2"] {
            assert!(parse_pair(bad).is_err(), "accepted {bad:?}");
        }
        assert_eq!(parse_bind("0.0.0.0").unwrap(), "0.0.0.0");
        assert_eq!(parse_bind("[::]").unwrap(), "::");
        assert_eq!(parse_bind("localhost").unwrap(), "localhost");
        assert!(parse_bind("example.com").is_err());
    }

    #[test]
    fn reserved_ports_cannot_be_exposed() {
        for port in [22, 6080, 8090, 8911, 47984, 48000, 48010] {
            let error = parse_expose_port(&port.to_string()).unwrap_err();
            assert!(error.contains("reserved"), "{error}");
        }
        for port in [80, 3000, 8000, 8091, 47983, 48011, 65535] {
            assert_eq!(parse_expose_port(&port.to_string()), Ok(port));
        }
        assert!(parse_expose_port("0").is_err());
        assert!(parse_expose_port("65536").is_err());
    }

    #[test]
    fn a_taken_local_port_is_reported_before_connecting() {
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let error = Forward::local(None, port, 80)
            .check_local_port()
            .unwrap_err();
        assert_eq!(error.code(), "port_in_use");
        // Free again once released; tests running in parallel may take the
        // same ephemeral port meanwhile, so allow a few tries.
        drop(held);
        let free = (0..50).any(|_| {
            Forward::local(None, port, 80).check_local_port().is_ok() || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                false
            }
        });
        assert!(free, "port {port} stayed taken after it was released");
    }
}
