//! Configuration file parsing and validation.
//!
//! ```text
//! server {
//!     host 127.0.0.1
//!     ports 8080 8081
//!     server_name localhost
//!     client_max_body_size 1M
//!     error_page 404 www/errors/404.html
//!     route /path {
//!         methods GET POST DELETE
//!         root www
//!         index index.html
//!         autoindex off
//!         cgi .py /usr/bin/python3
//!         redirect 301 /elsewhere
//!         upload_dir www/uploads
//!     }
//! }
//! ```

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::Path;

pub const DEFAULT_MAX_BODY: usize = 1024 * 1024;
pub const MAX_BODY_LIMIT: usize = 1 << 30;

#[derive(Debug, Clone)]
pub struct Route {
    pub path: String,
    pub methods: Vec<String>,
    pub root: Option<String>,
    pub index: Option<String>,
    pub autoindex: bool,
    /// (extension with leading dot, interpreter path)
    pub cgi: Vec<(String, String)>,
    pub redirect: Option<(u16, String)>,
    pub upload_dir: Option<String>,
}

impl Route {
    pub fn new(path: &str) -> Self {
        Self {
            path: normalize_route_path(path),
            methods: vec!["GET".to_string()],
            root: None,
            index: None,
            autoindex: false,
            cgi: Vec::new(),
            redirect: None,
            upload_dir: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub ports: Vec<u16>,
    pub server_names: Vec<String>,
    pub max_body: usize,
    pub error_pages: HashMap<u16, String>,
    pub routes: Vec<Route>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            ports: Vec::new(),
            server_names: Vec::new(),
            max_body: DEFAULT_MAX_BODY,
            error_pages: HashMap::new(),
            routes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub servers: Vec<ServerConfig>,
}

fn normalize_route_path(p: &str) -> String {
    if p.len() > 1 {
        p.trim_end_matches('/').to_string()
    } else {
        p.to_string()
    }
}

fn parse_size(s: &str) -> Result<usize, String> {
    let bad = || format!("invalid size '{}'", s);
    let last = s.chars().last().ok_or_else(bad)?;
    let (digits, mult): (&str, u64) = match last {
        'k' | 'K' => (&s[..s.len() - 1], 1024),
        'm' | 'M' => (&s[..s.len() - 1], 1024 * 1024),
        'g' | 'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = digits.parse().map_err(|_| bad())?;
    let total = n.checked_mul(mult).ok_or_else(bad)?;
    usize::try_from(total).map_err(|_| bad())
}

fn need(args: &[&str], n: usize, key: &str) -> Result<(), String> {
    if args.len() != n {
        Err(format!("'{}' expects {} argument(s)", key, n))
    } else {
        Ok(())
    }
}

fn server_directive(s: &mut ServerConfig, key: &str, a: &[&str]) -> Result<(), String> {
    match key {
        "host" | "server_address" => {
            need(a, 1, key)?;
            s.host = a[0].to_string();
        }
        "ports" | "port" | "listen" => {
            if a.is_empty() {
                return Err(format!("'{}' expects at least one port", key));
            }
            for p in a {
                let n: u16 = p.parse().map_err(|_| format!("invalid port '{}'", p))?;
                s.ports.push(n);
            }
        }
        "server_name" => {
            if a.is_empty() {
                return Err("'server_name' expects at least one name".into());
            }
            for n in a {
                s.server_names.push(n.to_ascii_lowercase());
            }
        }
        "client_max_body_size" => {
            need(a, 1, key)?;
            s.max_body = parse_size(a[0])?;
        }
        "error_page" => {
            need(a, 2, key)?;
            let code: u16 = a[0]
                .parse()
                .map_err(|_| format!("invalid status code '{}'", a[0]))?;
            s.error_pages.insert(code, a[1].to_string());
        }
        _ => return Err(format!("unknown server directive '{}'", key)),
    }
    Ok(())
}

fn route_directive(r: &mut Route, key: &str, a: &[&str]) -> Result<(), String> {
    match key {
        "methods" => {
            if a.is_empty() {
                return Err("'methods' expects at least one method".into());
            }
            r.methods = a.iter().map(|m| m.to_ascii_uppercase()).collect();
        }
        "root" => {
            need(a, 1, key)?;
            r.root = Some(a[0].to_string());
        }
        "index" | "default_file" => {
            need(a, 1, key)?;
            r.index = Some(a[0].to_string());
        }
        "autoindex" | "directory_listing" => {
            need(a, 1, key)?;
            r.autoindex = match a[0] {
                "on" => true,
                "off" => false,
                other => return Err(format!("autoindex expects on/off, got '{}'", other)),
            };
        }
        "cgi" => {
            need(a, 2, key)?;
            if !a[0].starts_with('.') || a[0].len() < 2 {
                return Err(format!("cgi extension must look like '.py', got '{}'", a[0]));
            }
            r.cgi.push((a[0].to_string(), a[1].to_string()));
        }
        "redirect" => {
            need(a, 2, key)?;
            let code: u16 = a[0]
                .parse()
                .map_err(|_| format!("invalid redirect code '{}'", a[0]))?;
            r.redirect = Some((code, a[1].to_string()));
        }
        "upload_dir" => {
            need(a, 1, key)?;
            r.upload_dir = Some(a[0].to_string());
        }
        _ => return Err(format!("unknown route directive '{}'", key)),
    }
    Ok(())
}

pub fn parse(text: &str) -> Result<Config, String> {
    let mut servers: Vec<ServerConfig> = Vec::new();
    let mut cur: Option<ServerConfig> = None;
    let mut route: Option<Route> = None;

    for (i, raw) in text.lines().enumerate() {
        let ln = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let t: Vec<&str> = line.split_whitespace().collect();
        let err = |m: &str| format!("line {}: {}", ln, m);

        match t[0] {
            "server" => {
                if cur.is_some() {
                    return Err(err("nested 'server' block"));
                }
                if t.len() != 2 || t[1] != "{" {
                    return Err(err("expected 'server {'"));
                }
                cur = Some(ServerConfig::default());
            }
            "route" => {
                if cur.is_none() {
                    return Err(err("'route' outside of a server block"));
                }
                if route.is_some() {
                    return Err(err("nested 'route' block"));
                }
                if t.len() != 3 || t[2] != "{" {
                    return Err(err("expected 'route /path {'"));
                }
                if !t[1].starts_with('/') {
                    return Err(err("route path must start with '/'"));
                }
                route = Some(Route::new(t[1]));
            }
            "}" => {
                if t.len() != 1 {
                    return Err(err("unexpected tokens after '}'"));
                }
                if let Some(r) = route.take() {
                    match cur.as_mut() {
                        Some(s) => s.routes.push(r),
                        None => return Err(err("unexpected '}'")),
                    }
                } else if let Some(s) = cur.take() {
                    servers.push(s);
                } else {
                    return Err(err("unexpected '}'"));
                }
            }
            key => {
                if let Some(r) = route.as_mut() {
                    route_directive(r, key, &t[1..]).map_err(|m| err(&m))?;
                } else if let Some(s) = cur.as_mut() {
                    server_directive(s, key, &t[1..]).map_err(|m| err(&m))?;
                } else {
                    return Err(err("directive outside of a server block"));
                }
            }
        }
    }
    if cur.is_some() || route.is_some() {
        return Err("unexpected end of file: missing '}'".to_string());
    }
    Ok(Config { servers })
}

fn effective_host(h: &str) -> &str {
    if h == "localhost" {
        "127.0.0.1"
    } else {
        h
    }
}

/// Validates semantic constraints. With `check_fs` also checks that referenced
/// files and directories exist.
pub fn validate(cfg: &Config, check_fs: bool) -> Result<(), String> {
    if cfg.servers.is_empty() {
        return Err("no server blocks defined".into());
    }
    for (i, s) in cfg.servers.iter().enumerate() {
        let tag = format!("server #{}", i + 1);
        if s.ports.is_empty() {
            return Err(format!("{}: no ports defined", tag));
        }
        if s.host != "localhost" && s.host.parse::<IpAddr>().is_err() {
            return Err(format!("{}: invalid host '{}'", tag, s.host));
        }
        let mut seen = HashSet::new();
        for p in &s.ports {
            if *p == 0 {
                return Err(format!("{}: port 0 is not allowed", tag));
            }
            if !seen.insert(*p) {
                return Err(format!("{}: port {} listed twice", tag, p));
            }
        }
        if s.max_body == 0 || s.max_body > MAX_BODY_LIMIT {
            return Err(format!(
                "{}: client_max_body_size {} out of bounds (1..={})",
                tag, s.max_body, MAX_BODY_LIMIT
            ));
        }
        if s.routes.is_empty() {
            return Err(format!("{}: no routes defined", tag));
        }
        for (code, path) in &s.error_pages {
            if !(400..=599).contains(code) {
                return Err(format!("{}: error_page code {} is not 4xx/5xx", tag, code));
            }
            if check_fs && !Path::new(path).is_file() {
                return Err(format!("{}: error page '{}' not found", tag, path));
            }
        }
        let mut paths = HashSet::new();
        for r in &s.routes {
            let rt = format!("{} route '{}'", tag, r.path);
            if !paths.insert(r.path.clone()) {
                return Err(format!("{}: defined twice", rt));
            }
            for m in &r.methods {
                if !["GET", "POST", "DELETE", "HEAD"].contains(&m.as_str()) {
                    return Err(format!("{}: unsupported method '{}'", rt, m));
                }
            }
            if let Some((code, _)) = &r.redirect {
                if ![301, 302, 303, 307, 308].contains(code) {
                    return Err(format!("{}: invalid redirect code {}", rt, code));
                }
            } else if r.root.is_none() {
                return Err(format!("{}: needs 'root' or 'redirect'", rt));
            }
            if check_fs {
                if let Some(root) = &r.root {
                    if !Path::new(root).exists() {
                        return Err(format!("{}: root '{}' does not exist", rt, root));
                    }
                }
                if let Some(ud) = &r.upload_dir {
                    if !Path::new(ud).is_dir() {
                        return Err(format!("{}: upload_dir '{}' is not a directory", rt, ud));
                    }
                }
                for (_, interp) in &r.cgi {
                    if !Path::new(interp).exists() {
                        return Err(format!("{}: cgi interpreter '{}' not found", rt, interp));
                    }
                }
            }
        }
    }

    // Conflicting listeners.
    for i in 0..cfg.servers.len() {
        for j in (i + 1)..cfg.servers.len() {
            let (a, b) = (&cfg.servers[i], &cfg.servers[j]);
            let (ha, hb) = (effective_host(&a.host), effective_host(&b.host));
            for pa in &a.ports {
                if !b.ports.contains(pa) {
                    continue;
                }
                if ha == hb {
                    let overlap = (a.server_names.is_empty() && b.server_names.is_empty())
                        || a.server_names.iter().any(|n| b.server_names.contains(n));
                    if overlap {
                        return Err(format!(
                            "servers #{} and #{} conflict on {}:{} (same server_name)",
                            i + 1,
                            j + 1,
                            ha,
                            pa
                        ));
                    }
                } else if ha == "0.0.0.0" || hb == "0.0.0.0" {
                    return Err(format!(
                        "servers #{} and #{} conflict on port {} (0.0.0.0 overlaps {})",
                        i + 1,
                        j + 1,
                        pa,
                        if ha == "0.0.0.0" { hb } else { ha }
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "
server {
    host 127.0.0.1
    ports 8080 8081
    server_name localhost Example.COM
    client_max_body_size 2M
    error_page 404 /tmp/404.html
    route / {
        methods GET POST
        root /tmp
        index index.html
        autoindex on
    }
    route /cgi/ {
        root /tmp
        cgi .py /usr/bin/python3
    }
    route /old {
        redirect 301 /new
    }
}
";

    #[test]
    fn parses_valid_config() {
        let c = parse(GOOD).unwrap();
        assert_eq!(c.servers.len(), 1);
        let s = &c.servers[0];
        assert_eq!(s.ports, vec![8080, 8081]);
        assert_eq!(s.server_names, vec!["localhost", "example.com"]);
        assert_eq!(s.max_body, 2 * 1024 * 1024);
        assert_eq!(s.error_pages.get(&404).unwrap(), "/tmp/404.html");
        assert_eq!(s.routes.len(), 3);
        assert_eq!(s.routes[0].methods, vec!["GET", "POST"]);
        assert!(s.routes[0].autoindex);
        assert_eq!(s.routes[1].path, "/cgi"); // trailing slash trimmed
        assert_eq!(s.routes[1].cgi[0], (".py".to_string(), "/usr/bin/python3".to_string()));
        assert_eq!(s.routes[2].redirect, Some((301, "/new".to_string())));
        validate(&c, false).unwrap();
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert!(parse("server {\n ports 80\n").is_err()); // missing }
        assert!(parse("}\n").is_err());
        assert!(parse("ports 80\n").is_err());
        assert!(parse("server {\n bogus 1\n}\n").is_err());
        assert!(parse("server {\n ports abc\n}\n").is_err());
        assert!(parse("server {\n ports 99999\n}\n").is_err());
        assert!(parse("server {\n route x {\n }\n}\n").is_err());
        assert!(parse("server {\n server {\n }\n}\n").is_err());
        assert!(parse("server {\n route / {\n autoindex maybe\n }\n}\n").is_err());
    }

    fn one(extra_server: &str) -> Config {
        parse(&format!(
            "server {{\n ports 8080\n {}\n route / {{\n root /tmp\n }}\n}}\n",
            extra_server
        ))
        .unwrap()
    }

    #[test]
    fn body_size_bounds() {
        assert!(validate(&one("client_max_body_size 1"), false).is_ok());
        assert!(validate(&one("client_max_body_size 0"), false).is_err());
        assert!(validate(&one("client_max_body_size 1G"), false).is_ok());
        assert!(validate(&one("client_max_body_size 2G"), false).is_err());
        assert!(parse_size("10x").is_err());
        assert_eq!(parse_size("10k").unwrap(), 10 * 1024);
    }

    #[test]
    fn port_validation() {
        let c = parse("server {\n ports 0\n route / {\n root /tmp\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_err());
        let c = parse("server {\n ports 80 80\n route / {\n root /tmp\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_err());
        let c = parse("server {\n route / {\n root /tmp\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_err()); // no ports
    }

    #[test]
    fn conflicting_servers_are_rejected() {
        let block = |names: &str, host: &str| {
            format!(
                "server {{\n host {}\n ports 8080\n {}\n route / {{\n root /tmp\n }}\n}}\n",
                host, names
            )
        };
        // same host:port, same name -> conflict
        let c = parse(&format!("{}{}", block("server_name a", "127.0.0.1"), block("server_name a", "127.0.0.1"))).unwrap();
        assert!(validate(&c, false).is_err());
        // same host:port, different names -> virtual hosts, fine
        let c = parse(&format!("{}{}", block("server_name a", "127.0.0.1"), block("server_name b", "127.0.0.1"))).unwrap();
        assert!(validate(&c, false).is_ok());
        // 0.0.0.0 overlaps with a specific address on the same port
        let c = parse(&format!("{}{}", block("server_name a", "0.0.0.0"), block("server_name b", "127.0.0.1"))).unwrap();
        assert!(validate(&c, false).is_err());
    }

    #[test]
    fn invalid_paths_detected_with_fs_check() {
        let c = parse("server {\n ports 8080\n route / {\n root /definitely/not/here\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_ok());
        assert!(validate(&c, true).is_err());
        let c = parse("server {\n ports 8080\n error_page 404 /nope.html\n route / {\n root /tmp\n }\n}\n").unwrap();
        assert!(validate(&c, true).is_err());
    }

    #[test]
    fn route_validation() {
        let c = parse("server {\n ports 8080\n route / {\n methods PATCH\n root /tmp\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_err());
        let c = parse("server {\n ports 8080\n route / {\n methods GET\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_err()); // no root/redirect
        let c = parse("server {\n ports 8080\n route / {\n redirect 200 /x\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_err());
        let c = parse("server {\n ports 8080\n route /a {\n root /tmp\n }\n route /a/ {\n root /tmp\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_err()); // duplicate after normalisation
        let c = parse("server {\n host not-an-ip\n ports 8080\n route / {\n root /tmp\n }\n}\n").unwrap();
        assert!(validate(&c, false).is_err());
    }
}
