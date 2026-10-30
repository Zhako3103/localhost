//! Virtual-host selection, route matching and safe path resolution.

use crate::config::{Config, Route, ServerConfig};
use std::path::{Path, PathBuf};

/// Picks the server among `candidates` (indices into `cfg.servers` that share
/// one host:port) by the Host header. Falls back to the first candidate.
pub fn select_server(cfg: &Config, candidates: &[usize], host: Option<&str>) -> usize {
    let first = candidates.first().copied().unwrap_or(0);
    let host = match host {
        Some(h) => h.trim().to_ascii_lowercase(),
        None => return first,
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("").to_string()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    for &i in candidates {
        if let Some(s) = cfg.servers.get(i) {
            if s.server_names.iter().any(|n| *n == name) {
                return i;
            }
        }
    }
    first
}

/// Longest-prefix route match on path-segment boundaries.
pub fn match_route<'a>(server: &'a ServerConfig, path: &str) -> Option<&'a Route> {
    let mut best: Option<&Route> = None;
    for r in &server.routes {
        let hit = r.path == "/"
            || path == r.path
            || (path.starts_with(&r.path) && path.as_bytes().get(r.path.len()) == Some(&b'/'));
        if hit && best.map_or(true, |b| r.path.len() > b.path.len()) {
            best = Some(r);
        }
    }
    best
}

/// Maps a decoded URL path to a filesystem path below the route's root.
/// Lexically rejects any `..` that would climb above the root (403).
pub fn resolve(route: &Route, path: &str) -> Result<PathBuf, u16> {
    let root = route.root.as_ref().ok_or(500u16)?;
    if Path::new(root).is_file() {
        return Ok(PathBuf::from(root));
    }
    let rest = if route.path == "/" {
        path
    } else {
        path.get(route.path.len()..).unwrap_or("")
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(403);
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = PathBuf::from(root);
    for p in parts {
        out.push(p);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    fn cfg() -> Config {
        config::parse(
            "
server {
    ports 8080
    server_name alpha
    route / {
        root /srv/a
    }
    route /files {
        root /srv/files
    }
    route /files/deep {
        root /srv/deep
    }
}
server {
    ports 8080
    server_name beta
    route / {
        root /srv/b
    }
}
",
        )
        .unwrap()
    }

    #[test]
    fn selects_by_host_header_with_default_fallback() {
        let c = cfg();
        let cands = [0usize, 1];
        assert_eq!(select_server(&c, &cands, Some("beta")), 1);
        assert_eq!(select_server(&c, &cands, Some("BETA:8080")), 1);
        assert_eq!(select_server(&c, &cands, Some("alpha")), 0);
        assert_eq!(select_server(&c, &cands, Some("unknown")), 0);
        assert_eq!(select_server(&c, &cands, None), 0);
    }

    #[test]
    fn longest_prefix_wins_on_segment_boundary() {
        let c = cfg();
        let s = &c.servers[0];
        assert_eq!(match_route(s, "/").unwrap().path, "/");
        assert_eq!(match_route(s, "/files").unwrap().path, "/files");
        assert_eq!(match_route(s, "/files/x.txt").unwrap().path, "/files");
        assert_eq!(match_route(s, "/files/deep/x").unwrap().path, "/files/deep");
        // "/filesystem" must not match "/files"
        assert_eq!(match_route(s, "/filesystem").unwrap().path, "/");
    }

    #[test]
    fn resolves_below_root() {
        let c = cfg();
        let s = &c.servers[0];
        let r = match_route(s, "/files/a/b.txt").unwrap();
        assert_eq!(resolve(r, "/files/a/b.txt").unwrap(), PathBuf::from("/srv/files/a/b.txt"));
        let r = match_route(s, "/x").unwrap();
        assert_eq!(resolve(r, "/x/./y//z").unwrap(), PathBuf::from("/srv/a/x/y/z"));
        assert_eq!(resolve(r, "/").unwrap(), PathBuf::from("/srv/a"));
        assert_eq!(resolve(r, "/a/../b").unwrap(), PathBuf::from("/srv/a/b"));
    }

    #[test]
    fn traversal_is_forbidden() {
        let c = cfg();
        let r = &c.servers[0].routes[0];
        assert_eq!(resolve(r, "/../etc/passwd"), Err(403));
        assert_eq!(resolve(r, "/a/../../etc"), Err(403));
    }
}
