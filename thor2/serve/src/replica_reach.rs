//! Does THOR's copy on the other machine answer at all?
//!
//! THE DEFECT THIS CLOSES. On 2026-10-25 the machine that holds a user's
//! read-only copy rebooted after an automatic update and its encrypted data
//! folder stayed locked, so the copy's containers could not start. The phone
//! and laptop connection was dead for eleven hours and nothing said so in
//! plain words: the ship line only blamed the scheduled task.
//!
//! `sync ship` records the receiver URL it was given in a tiny sidecar beside
//! the store (`sync-ship-target.json`, same shape as the other sidecars: one
//! fixed name, fail-open on read). The session start and `doctor` then try one
//! TCP connect to that host and port under a hard time limit, so a machine
//! that is switched off never delays a start noticeably. Silent when no
//! target is known and when the receiver answers.

use serde::{Deserialize, Serialize};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The whole check, name lookup included, is abandoned after this.
pub const REACH_LIMIT: Duration = Duration::from_millis(800);
/// The connect itself gets a little less, so the answer arrives inside the limit.
const CONNECT_LIMIT: Duration = Duration::from_millis(700);

#[derive(Serialize, Deserialize)]
struct Target {
    url: String,
}

pub fn path_for(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("sync-ship-target.json")
}

/// Remember the receiver URL of the latest `sync ship`, success or failure.
pub fn record_target(db: &Path, url: &str) -> std::io::Result<()> {
    let json = serde_json::to_string(&Target { url: url.trim().to_string() }).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path_for(db), json)
}

/// The last recorded receiver URL, or `None` (never shipped, unreadable).
pub fn read_target(db: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path_for(db)).ok()?;
    let target: Target = serde_json::from_str(&text).ok()?;
    Some(target.url).filter(|u| !u.trim().is_empty())
}

/// Host and port of `http://host:port/...` (or https, or a bare `host:port`).
/// The port defaults to 80 / 443 by scheme. `None` when no host can be read.
pub fn host_port(url: &str) -> Option<(String, u16)> {
    let url = url.trim();
    let (default_port, rest) = if let Some(r) = url.strip_prefix("https://") {
        (443, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (80, r)
    } else {
        (80, url)
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default_port);
        return Some((host.to_string(), port));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, default_port),
    };
    (!host.is_empty()).then(|| (host.to_string(), port))
}

/// Whether a TCP connection to `host:port` succeeds within `limit` in total.
pub fn answers(host: &str, port: u16, limit: Duration) -> bool {
    let host = host.to_string();
    crate::work_folders::within(limit, move || {
        let Ok(addrs) = (host.as_str(), port).to_socket_addrs() else { return false };
        addrs.into_iter().any(|addr| TcpStream::connect_timeout(&addr, CONNECT_LIMIT).is_ok())
    })
    .unwrap_or(false)
}

/// The host the copy is on, when it is known and does NOT answer.
pub fn down_host(db: &Path, limit: Duration) -> Option<String> {
    let (host, port) = host_port(&read_target(db)?)?;
    (!answers(&host, port, limit)).then_some(host)
}

/// The plain sentence for someone who has never run a second machine.
pub fn down_words(host: &str) -> String {
    format!(
        "THOR's copy on {host} does not answer, so your other devices (phone, laptop) have no memory connection. That machine may have restarted: if its data folder is encrypted, unlock it, then start its THOR containers again."
    )
}

/// The session-start / doctor line, or `None` when no target is known or the
/// receiver answers.
pub fn down_line(db: &Path) -> Option<String> {
    down_host(db, REACH_LIMIT).map(|host| down_words(&host))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    fn store() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        (dir, db)
    }

    #[test]
    fn host_and_port_are_read_from_a_url() {
        assert_eq!(host_port("http://nas.local:5556"), Some(("nas.local".into(), 5556)));
        assert_eq!(host_port("http://10.0.0.5:5556/ship"), Some(("10.0.0.5".into(), 5556)));
        assert_eq!(host_port("https://example.org"), Some(("example.org".into(), 443)));
        assert_eq!(host_port("http://[::1]:7"), Some(("::1".into(), 7)));
        assert_eq!(host_port("box:9"), Some(("box".into(), 9)));
        assert_eq!(host_port("http://"), None);
    }

    #[test]
    fn no_known_target_gives_no_line() {
        let (_dir, db) = store();
        assert_eq!(down_line(&db), None);
    }

    #[test]
    fn a_closed_port_gives_the_line_within_the_time_box() {
        let (_dir, db) = store();
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        record_target(&db, &format!("http://127.0.0.1:{port}")).unwrap();
        let started = Instant::now();
        let line = down_line(&db).expect("a closed port must be reported");
        assert!(started.elapsed() < Duration::from_millis(1500), "waited {:?}", started.elapsed());
        assert!(line.contains("127.0.0.1") && line.contains("does not answer"), "{line}");
    }

    #[test]
    fn an_open_port_gives_no_line() {
        let (_dir, db) = store();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        record_target(&db, &format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port())).unwrap();
        assert_eq!(down_line(&db), None);
    }

    #[test]
    fn a_corrupt_sidecar_is_no_target() {
        let (_dir, db) = store();
        std::fs::write(path_for(&db), b"not json").unwrap();
        assert_eq!(read_target(&db), None);
    }
}
