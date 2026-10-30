//! Turns a parsed request into an action: an immediate response or a CGI job.

use crate::cgi::CgiJob;
use crate::config::{Route, ServerConfig};
use crate::http::{
    default_error_page, html_escape, find, mime_type, pct_encode, query_param, Request, Response,
};
use crate::router;
use crate::session::SessionStore;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_STATIC_FILE: u64 = 256 * 1024 * 1024;

pub enum Action {
    Respond(Response),
    Cgi(CgiJob),
}

// ------------------------------------------------------------ error pages

pub fn error_response(server: &ServerConfig, code: u16) -> Response {
    if let Some(path) = server.error_pages.get(&code) {
        if let Ok(body) = fs::read(path) {
            return Response::with_body(code, "text/html; charset=utf-8", body);
        }
    }
    Response::with_body(code, "text/html; charset=utf-8", default_error_page(code))
}

// ------------------------------------------------------------------ entry

pub fn handle(
    server: &ServerConfig,
    port: u16,
    req: &Request,
    sessions: &mut SessionStore,
) -> Action {
    if req.path == "/__session" || req.path.starts_with("/__session/") {
        return Action::Respond(session_endpoint(server, req, sessions));
    }

    let route = match router::match_route(server, &req.path) {
        Some(r) => r,
        None => return Action::Respond(error_response(server, 404)),
    };

    if let Some((code, url)) = &route.redirect {
        let body = format!(
            "<html><body>Redirecting to <a href=\"{0}\">{0}</a></body></html>\n",
            html_escape(url)
        );
        return Action::Respond(
            Response::with_body(*code, "text/html; charset=utf-8", body.into_bytes())
                .header("Location", url),
        );
    }

    let has = |m: &str| route.methods.iter().any(|x| x == m);
    let method_ok = has(&req.method) || (req.method == "HEAD" && has("GET"));
    if !method_ok {
        return Action::Respond(
            error_response(server, 405).header("Allow", &allow_header(route)),
        );
    }

    let mut fs_path = match router::resolve(route, &req.path) {
        Ok(p) => p,
        Err(c) => return Action::Respond(error_response(server, c)),
    };
    if let Some(root) = &route.root {
        if !within_root(root, &fs_path) {
            return Action::Respond(error_response(server, 403));
        }
    }

    // Directory URL: use the route's default file if it exists.
    let is_get = req.method == "GET" || req.method == "HEAD";
    if req.path.ends_with('/') && fs_path.is_dir() {
        if let Some(idx) = &route.index {
            let ip = fs_path.join(idx);
            if ip.is_file()
                && (is_get || (req.method == "POST" && cgi_for(route, &ip).is_some()))
            {
                fs_path = ip;
            }
        }
    }

    // CGI by file extension.
    if req.method != "DELETE" {
        if let Some(interp) = cgi_for(route, &fs_path) {
            if !fs_path.is_file() {
                return Action::Respond(error_response(server, 404));
            }
            return match build_cgi(server, port, req, interp, &fs_path) {
                Ok(job) => Action::Cgi(job),
                Err(c) => Action::Respond(error_response(server, c)),
            };
        }
    }

    let result = match req.method.as_str() {
        "GET" | "HEAD" => serve(route, req, &fs_path),
        "POST" => upload(route, req, &fs_path),
        "DELETE" => delete(&fs_path),
        _ => Err(405),
    };
    Action::Respond(match result {
        Ok(r) => r,
        Err(c) => error_response(server, c),
    })
}

fn allow_header(route: &Route) -> String {
    let mut v: Vec<&str> = route.methods.iter().map(|s| s.as_str()).collect();
    if v.contains(&"GET") && !v.contains(&"HEAD") {
        v.push("HEAD");
    }
    v.join(", ")
}

fn io_status(e: &io::Error) -> u16 {
    match e.kind() {
        io::ErrorKind::NotFound => 404,
        io::ErrorKind::PermissionDenied => 403,
        _ => match e.raw_os_error() {
            Some(c) if c == libc::ENOTDIR || c == libc::ENAMETOOLONG => 404,
            _ => 500,
        },
    }
}

/// Guards against symlinks that point outside the route root.
fn within_root(root: &str, p: &Path) -> bool {
    let r = match fs::canonicalize(root) {
        Ok(r) => r,
        Err(_) => return true, // missing root surfaces as 404 later
    };
    if r.is_file() {
        return true;
    }
    match fs::canonicalize(p) {
        Ok(c) => c.starts_with(&r),
        Err(_) => true, // non-existent target: lexical check already done
    }
}

fn cgi_for<'a>(route: &'a Route, path: &Path) -> Option<&'a str> {
    let ext = path.extension()?.to_str()?;
    route
        .cgi
        .iter()
        .find(|(e, _)| e.trim_start_matches('.') == ext)
        .map(|(_, i)| i.as_str())
}

// -------------------------------------------------------------------- CGI

fn build_cgi(
    server: &ServerConfig,
    port: u16,
    req: &Request,
    interpreter: &str,
    fs_path: &Path,
) -> Result<CgiJob, u16> {
    let script = fs::canonicalize(fs_path).map_err(|e| io_status(&e))?;
    let workdir = script.parent().map(|p| p.to_path_buf()).ok_or(500u16)?;
    let script_str = script.to_string_lossy().to_string();
    let name = server
        .server_names
        .first()
        .cloned()
        .unwrap_or_else(|| "localhost".to_string());

    let mut env: Vec<(String, String)> = vec![
        ("GATEWAY_INTERFACE".into(), "CGI/1.1".into()),
        ("SERVER_PROTOCOL".into(), "HTTP/1.1".into()),
        ("SERVER_SOFTWARE".into(), "localhost-rs/1.0".into()),
        ("SERVER_NAME".into(), name),
        ("SERVER_PORT".into(), port.to_string()),
        ("REQUEST_METHOD".into(), req.method.clone()),
        ("QUERY_STRING".into(), req.query.clone()),
        ("PATH_INFO".into(), script_str.clone()),
        ("PATH_TRANSLATED".into(), script_str.clone()),
        ("SCRIPT_FILENAME".into(), script_str),
        ("SCRIPT_NAME".into(), req.path.clone()),
        ("REDIRECT_STATUS".into(), "200".into()),
        ("CONTENT_LENGTH".into(), req.body.len().to_string()),
        (
            "CONTENT_TYPE".into(),
            req.header("content-type").unwrap_or("").to_string(),
        ),
        ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
    ];
    for (k, v) in &req.headers {
        if k == "content-length" || k == "content-type" {
            continue;
        }
        env.push((format!("HTTP_{}", k.to_ascii_uppercase().replace('-', "_")), v.clone()));
    }
    Ok(CgiJob {
        interpreter: interpreter.to_string(),
        script,
        workdir,
        env,
        body: req.body.clone(),
    })
}

// ----------------------------------------------------------------- static

fn serve(route: &Route, req: &Request, fs_path: &Path) -> Result<Response, u16> {
    let meta = fs::metadata(fs_path).map_err(|e| io_status(&e))?;
    if meta.is_dir() {
        if !req.path.ends_with('/') {
            let mut loc = pct_encode(&req.path, true);
            loc.push('/');
            if !req.query.is_empty() {
                loc.push('?');
                loc.push_str(&req.query);
            }
            return Ok(Response::new(301).header("Location", &loc));
        }
        if route.autoindex {
            return autoindex(fs_path, &req.path);
        }
        return Err(403);
    }
    if !meta.is_file() {
        return Err(403);
    }
    if meta.len() > MAX_STATIC_FILE {
        return Err(500);
    }
    let data = fs::read(fs_path).map_err(|e| io_status(&e))?;
    Ok(Response::with_body(200, mime_type(fs_path), data))
}

fn autoindex(dir: &Path, req_path: &str) -> Result<Response, u16> {
    let rd = fs::read_dir(dir).map_err(|e| io_status(&e))?;
    let mut entries: Vec<(String, bool)> = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let is_dir = ent.metadata().map(|m| m.is_dir()).unwrap_or(false);
        entries.push((name, is_dir));
    }
    entries.sort();

    let mut html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Index of {0}</title></head>\
         <body><h1>Index of {0}</h1><hr><ul>",
        html_escape(req_path)
    );
    if req_path != "/" {
        html.push_str("<li><a href=\"../\">../</a></li>");
    }
    for (name, is_dir) in entries {
        let mut href = pct_encode(&name, false);
        let mut label = html_escape(&name);
        if is_dir {
            href.push('/');
            label.push('/');
        }
        html.push_str(&format!("<li><a href=\"{}\">{}</a></li>", href, label));
    }
    html.push_str("</ul><hr></body></html>\n");
    Ok(Response::with_body(200, "text/html; charset=utf-8", html.into_bytes()))
}

// ----------------------------------------------------------------- upload
#[allow(dead_code)]
pub struct Part<'a> {
    pub name: Option<String>,
    pub filename: Option<String>,
    pub data: &'a [u8],
}

fn disposition_param(h: &str, key: &str) -> Option<String> {
    let pat = format!("{}=\"", key);
    for (i, _) in h.match_indices(&pat) {
        let prev_ok = i == 0 || matches!(h.as_bytes()[i - 1], b' ' | b';');
        if !prev_ok {
            continue;
        }
        let rest = &h[i + pat.len()..];
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    None
}

/// Minimal multipart/form-data parser.
pub fn parse_multipart<'a>(body: &'a [u8], boundary: &str) -> Option<Vec<Part<'a>>> {
    let delim = format!("--{}", boundary).into_bytes();
    let mut next_delim = b"\r\n".to_vec();
    next_delim.extend_from_slice(&delim);

    let mut parts = Vec::new();
    let mut pos = find(body, &delim)? + delim.len();
    loop {
        if body[pos..].starts_with(b"--") {
            break;
        }
        if !body[pos..].starts_with(b"\r\n") {
            return None;
        }
        pos += 2;
        let hend = find(&body[pos..], b"\r\n\r\n")? + pos;
        let head = std::str::from_utf8(&body[pos..hend]).ok()?;
        let data_start = hend + 4;
        let dend = find(&body[data_start..], &next_delim)? + data_start;

        let mut name = None;
        let mut filename = None;
        for line in head.split("\r\n") {
            if line.to_ascii_lowercase().starts_with("content-disposition:") {
                name = disposition_param(line, "name");
                filename = disposition_param(line, "filename");
            }
        }
        parts.push(Part {
            name,
            filename,
            data: &body[data_start..dend],
        });
        pos = dend + next_delim.len();
    }
    Some(parts)
}

fn boundary_of(ct: &str) -> Option<String> {
    let lower = ct.to_ascii_lowercase();
    let i = lower.find("boundary=")?;
    let v = ct[i + 9..].split(';').next()?.trim().trim_matches('"');
    if v.is_empty() || v.len() > 200 {
        None
    } else {
        Some(v.to_string())
    }
}

/// Reduces a client-supplied filename to a safe basename.
pub fn sanitize_filename(raw: &str) -> Option<String> {
    let base = raw.rsplit(|c| c == '/' || c == '\\').next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.').to_string();
    let cleaned: String = cleaned.chars().take(100).collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn unique_stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn save_unique(dir: &Path, name: &str, data: &[u8]) -> io::Result<String> {
    for i in 0..1000u32 {
        let candidate = if i == 0 {
            name.to_string()
        } else {
            match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{}_{}.{}", stem, i, ext),
                _ => format!("{}_{}", name, i),
            }
        };
        let full = dir.join(&candidate);
        match fs::OpenOptions::new().write(true).create_new(true).open(&full) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(data) {
                    drop(f);
                    let _ = fs::remove_file(&full);
                    return Err(e);
                }
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "too many name collisions"))
}

fn upload(route: &Route, req: &Request, fs_path: &Path) -> Result<Response, u16> {
    let dir: PathBuf = match &route.upload_dir {
        Some(d) => PathBuf::from(d),
        None if fs_path.is_dir() => fs_path.to_path_buf(),
        None => return Err(405),
    };
    if !dir.is_dir() {
        return Err(500);
    }
    if req.body.is_empty() {
        return Err(400);
    }

    let ct = req.header("content-type").unwrap_or("");
    let mut saved: Vec<String> = Vec::new();
    if ct.to_ascii_lowercase().starts_with("multipart/form-data") {
        let boundary = boundary_of(ct).ok_or(400u16)?;
        let parts = parse_multipart(&req.body, &boundary).ok_or(400u16)?;
        for part in parts {
            if let Some(fname) = part.filename.as_deref().and_then(sanitize_filename) {
                let name = save_unique(&dir, &fname, part.data).map_err(|e| io_status(&e))?;
                saved.push(name);
            }
        }
        if saved.is_empty() {
            return Err(400);
        }
    } else {
        let wanted = query_param(&req.query, "name")
            .and_then(|n| sanitize_filename(&n))
            .unwrap_or_else(|| format!("upload_{}", unique_stamp()));
        let name = save_unique(&dir, &wanted, &req.body).map_err(|e| io_status(&e))?;
        saved.push(name);
    }
    let body = format!("Uploaded: {}\n", saved.join(", "));
    Ok(Response::with_body(201, "text/plain; charset=utf-8", body.into_bytes()))
}

// ----------------------------------------------------------------- delete

fn delete(fs_path: &Path) -> Result<Response, u16> {
    let meta = fs::symlink_metadata(fs_path).map_err(|e| io_status(&e))?;
    if meta.is_dir() {
        return Err(403);
    }
    fs::remove_file(fs_path).map_err(|e| io_status(&e))?;
    Ok(Response::new(204))
}

// --------------------------------------------------------------- sessions

fn json_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Built-in demo endpoint: `/__session[?name=X]` and `/__session/logout`.
fn session_endpoint(server: &ServerConfig, req: &Request, sessions: &mut SessionStore) -> Response {
    if req.method != "GET" && req.method != "HEAD" {
        return error_response(server, 405).header("Allow", "GET, HEAD");
    }
    if req.path == "/__session/logout" {
        if let Some(sid) = req.cookie("SID") {
            sessions.remove(&sid);
        }
        return Response::with_body(200, "application/json", b"{\"logged_out\":true}\n".to_vec())
            .header("Set-Cookie", "SID=; Path=/; Max-Age=0; HttpOnly");
    }
    if req.path != "/__session" {
        return error_response(server, 404);
    }

    let existing = req.cookie("SID").filter(|s| sessions.is_valid(s));
    let (id, is_new) = match existing {
        Some(s) => (s, false),
        None => match sessions.create() {
            Some(id) => (id, true),
            None => return error_response(server, 503),
        },
    };
    let session = match sessions.get_mut(&id) {
        Some(s) => s,
        None => return error_response(server, 500),
    };
    session.visits += 1;
    if let Some(name) = query_param(&req.query, "name") {
        let clean: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
        session.data.insert("name".to_string(), clean);
    }
    let name = session.data.get("name").cloned().unwrap_or_default();
    let body = format!(
        "{{\"session\":\"{}\",\"new\":{},\"visits\":{},\"name\":\"{}\"}}\n",
        id,
        is_new,
        session.visits,
        json_escape(&name)
    );
    let mut resp = Response::with_body(200, "application/json", body.into_bytes());
    if is_new {
        resp = resp.header(
            "Set-Cookie",
            &format!("SID={}; Path=/; Max-Age=1800; HttpOnly; SameSite=Lax", id),
        );
    }
    resp
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use std::time::Duration;

    /// Builds a temp site and a server config pointing at it.
    fn setup(name: &str) -> (ServerConfig, PathBuf) {
        let dir = std::env::temp_dir().join(format!("localhost_rs_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("site/empty")).unwrap();
        fs::create_dir_all(dir.join("site/listing")).unwrap();
        fs::create_dir_all(dir.join("site/subdir")).unwrap();
        fs::create_dir_all(dir.join("uploads")).unwrap();
        fs::write(dir.join("site/index.html"), "<h1>hi</h1>").unwrap();
        fs::write(dir.join("site/a.txt"), "aaa").unwrap();
        fs::write(dir.join("site/listing/x.txt"), "x").unwrap();
        let d = dir.to_string_lossy();
        let text = format!(
            "server {{\n ports 8080\n server_name t\n route / {{\n methods GET DELETE\n root {d}/site\n index index.html\n }}\n \
             route /empty {{\n root {d}/site/empty\n }}\n \
             route /listing {{\n root {d}/site/listing\n autoindex on\n }}\n \
             route /up {{\n methods GET POST\n root {d}/uploads\n upload_dir {d}/uploads\n }}\n \
             route /go {{\n redirect 302 /a.txt\n }}\n}}\n"
        );
        let cfg = config::parse(&text).unwrap();
        (cfg.servers[0].clone(), dir)
    }

    fn request(method: &str, path: &str) -> Request {
        Request {
            method: method.to_string(),
            target: path.to_string(),
            path: path.to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("host".to_string(), "t".to_string())],
            ..Default::default()
        }
    }

    fn run(server: &ServerConfig, req: &Request) -> Response {
        let mut sessions = SessionStore::new(Duration::from_secs(60), 10);
        match handle(server, 8080, req, &mut sessions) {
            Action::Respond(r) => r,
            Action::Cgi(_) => panic!("unexpected CGI"),
        }
    }

    #[test]
    fn static_file_and_index() {
        let (s, _d) = setup("static");
        let r = run(&s, &request("GET", "/a.txt"));
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"aaa");
        let r = run(&s, &request("GET", "/"));
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"<h1>hi</h1>");
        assert_eq!(run(&s, &request("HEAD", "/a.txt")).status, 200);
    }

    #[test]
    fn missing_file_is_404() {
        let (s, _d) = setup("missing");
        assert_eq!(run(&s, &request("GET", "/nope")).status, 404);
        assert_eq!(run(&s, &request("GET", "/a.txt/extra")).status, 404);
    }

    #[test]
    fn disallowed_method_is_405_with_allow() {
        let (s, _d) = setup("method");
        let r = run(&s, &request("POST", "/a.txt"));
        assert_eq!(r.status, 405);
        assert!(r.headers.iter().any(|(k, _)| k == "Allow"));
        assert_eq!(run(&s, &request("PUT", "/a.txt")).status, 405);
    }

    #[test]
    fn traversal_is_403() {
        let (s, _d) = setup("trav");
        assert_eq!(run(&s, &request("GET", "/../../etc/passwd")).status, 403);
    }

    #[test]
    fn directory_rules() {
        let (s, _d) = setup("dirs");
        // no index, autoindex off -> 403
        assert_eq!(run(&s, &request("GET", "/empty/")).status, 403);
        // directory without trailing slash -> redirect
        let r = run(&s, &request("GET", "/listing"));
        assert_eq!(r.status, 301);
        assert!(r.headers.iter().any(|(k, v)| k == "Location" && v == "/listing/"));
        // autoindex on
        let r = run(&s, &request("GET", "/listing/"));
        assert_eq!(r.status, 200);
        assert!(String::from_utf8_lossy(&r.body).contains("x.txt"));
    }

    #[test]
    fn redirect_route() {
        let (s, _d) = setup("redir");
        let r = run(&s, &request("GET", "/go"));
        assert_eq!(r.status, 302);
        assert!(r.headers.iter().any(|(k, v)| k == "Location" && v == "/a.txt"));
    }

    #[test]
    fn delete_behaviour() {
        let (s, d) = setup("delete");
        assert_eq!(run(&s, &request("DELETE", "/nope.txt")).status, 404);
        assert_eq!(run(&s, &request("DELETE", "/subdir")).status, 403);
        assert_eq!(run(&s, &request("DELETE", "/a.txt")).status, 204);
        assert!(!d.join("site/a.txt").exists());
    }

    #[test]
    fn raw_upload_and_collision() {
        let (s, d) = setup("upload");
        let mut r = request("POST", "/up");
        r.query = "name=file.txt".to_string();
        r.body = b"data".to_vec();
        assert_eq!(run(&s, &r).status, 201);
        assert_eq!(run(&s, &r).status, 201);
        assert_eq!(fs::read(d.join("uploads/file.txt")).unwrap(), b"data");
        assert!(d.join("uploads/file_1.txt").exists());
        // empty body -> 400
        r.body.clear();
        assert_eq!(run(&s, &r).status, 400);
    }

    #[test]
    fn multipart_upload() {
        let (s, d) = setup("multipart");
        let body = "--XX\r\nContent-Disposition: form-data; name=\"f\"; filename=\"../evil name.txt\"\r\n\
                    Content-Type: text/plain\r\n\r\nhello\r\n--XX\r\n\
                    Content-Disposition: form-data; name=\"note\"\r\n\r\nignored\r\n--XX--\r\n";
        let mut r = request("POST", "/up");
        r.headers.push(("content-type".into(), "multipart/form-data; boundary=XX".into()));
        r.body = body.as_bytes().to_vec();
        assert_eq!(run(&s, &r).status, 201);
        assert_eq!(fs::read(d.join("uploads/evil_name.txt")).unwrap(), b"hello");
    }

    #[test]
    fn multipart_parser_edge_cases() {
        assert!(parse_multipart(b"garbage", "XX").is_none());
        let p = parse_multipart(b"--XX\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nv\r\n--XX--", "XX").unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name.as_deref(), Some("a"));
        assert!(p[0].filename.is_none());
        assert_eq!(p[0].data, b"v");
    }

    #[test]
    fn filename_sanitising() {
        assert_eq!(sanitize_filename("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(sanitize_filename("C:\\dir\\a b.txt").as_deref(), Some("a_b.txt"));
        assert_eq!(sanitize_filename(".htaccess").as_deref(), Some("htaccess"));
        assert_eq!(sanitize_filename("/"), None);
        assert_eq!(sanitize_filename(""), None);
    }

    #[test]
    fn custom_error_page_is_used_and_default_otherwise() {
        let (mut s, d) = setup("errpage");
        let p = d.join("e404.html");
        fs::write(&p, "custom 404").unwrap();
        s.error_pages.insert(404, p.to_string_lossy().to_string());
        assert_eq!(run(&s, &request("GET", "/nope")).body, b"custom 404");
        let r = error_response(&s, 500);
        assert!(String::from_utf8_lossy(&r.body).contains("500 Internal Server Error"));
    }

    #[test]
    fn session_flow() {
        let (s, _d) = setup("session");
        let mut store = SessionStore::new(Duration::from_secs(60), 10);
        let mut req = request("GET", "/__session");
        req.query = "name=Bob".to_string();
        let first = match handle(&s, 8080, &req, &mut store) {
            Action::Respond(r) => r,
            _ => panic!(),
        };
        assert_eq!(first.status, 200);
        let cookie = first
            .headers
            .iter()
            .find(|(k, _)| k == "Set-Cookie")
            .map(|(_, v)| v.clone())
            .unwrap();
        let sid = cookie.split(';').next().unwrap().trim_start_matches("SID=").to_string();

        let mut req2 = request("GET", "/__session");
        req2.headers.push(("cookie".into(), format!("SID={}", sid)));
        let second = match handle(&s, 8080, &req2, &mut store) {
            Action::Respond(r) => r,
            _ => panic!(),
        };
        let text = String::from_utf8_lossy(&second.body).to_string();
        assert!(text.contains("\"visits\":2"), "{}", text);
        assert!(text.contains("\"name\":\"Bob\""), "{}", text);
        assert!(!second.headers.iter().any(|(k, _)| k == "Set-Cookie"));
    }
}
