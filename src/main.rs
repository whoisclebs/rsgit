use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_ADDR: &str = "127.0.0.1:8080";
const MAX_REQUEST_BYTES: usize = 8192;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_GIT_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CLONE_FILE_BYTES: usize = 128 * 1024 * 1024;
const MAX_SEARCH_QUERY_LEN: usize = 128;

fn main() -> std::io::Result<()> {
    let addr = env::var("RSGIT_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let repo_root = env::var("RSGIT_REPO_ROOT").unwrap_or_else(|_| ".".to_string());
    let repo_root = fs::canonicalize(repo_root)?;
    let state = AppState { repo_root };

    let listener = TcpListener::bind(&addr)?;
    eprintln!("rsgit listening on http://{addr}");
    eprintln!("repository root: {}", state.repo_root.display());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_connection(stream, &state) {
                    eprintln!("request error: {err}");
                }
            }
            Err(err) => eprintln!("connection error: {err}"),
        }
    }

    Ok(())
}

struct AppState {
    repo_root: PathBuf,
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    host: Option<String>,
}

#[derive(Clone, Debug)]
struct Repo {
    name: String,
    path: PathBuf,
    bare: bool,
}

fn handle_connection(mut stream: TcpStream, state: &AppState) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = [0_u8; MAX_REQUEST_BYTES];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }

    let raw = String::from_utf8_lossy(&buf[..n]);
    let (response, is_head) = match parse_request(&raw) {
        Some(req) if req.method == "GET" || req.method == "HEAD" => {
            let is_head = req.method == "HEAD";
            (route_response(&req, state), is_head)
        }
        Some(_) => (
            response(
                405,
                "Method Not Allowed",
                "text/plain; charset=utf-8",
                "method not allowed",
            )
            .into_bytes(),
            false,
        ),
        None => (
            response(
                400,
                "Bad Request",
                "text/plain; charset=utf-8",
                "bad request",
            )
            .into_bytes(),
            false,
        ),
    };

    if is_head {
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or(response.len());
        stream.write_all(&response[..split])?;
        stream.write_all(b"\r\n\r\n")?;
    } else {
        stream.write_all(&response)?;
    }
    stream.flush()
}

fn parse_request(raw: &str) -> Option<Request> {
    let first = raw.lines().next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let _version = parts.next()?;

    let (path_part, query_part) = target.split_once('?').unwrap_or((target, ""));
    Some(Request {
        method,
        path: url_decode(path_part),
        query: parse_query(query_part),
        host: parse_host(raw),
    })
}

fn parse_host(raw: &str) -> Option<String> {
    raw.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("host") {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

fn parse_query(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in raw.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(url_decode(k), url_decode(v));
    }
    out
}

fn route_response(req: &Request, state: &AppState) -> Vec<u8> {
    if let Some(response) = route_clone(req, state) {
        return response;
    }
    route(req, state).into_bytes()
}

fn route_clone(req: &Request, state: &AppState) -> Option<Vec<u8>> {
    let parts: Vec<&str> = req
        .path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() < 3 || parts[0] != "repo" {
        return None;
    }
    let repo = find_repo(state, parts[1])?;
    let clone_path = parts[2..].join("/");
    match clone_path.as_str() {
        "HEAD" => Some(serve_git_head(&repo)),
        "info/refs" => Some(serve_info_refs(&repo, req)),
        "objects/info/packs" => Some(serve_info_packs(&repo)),
        _ if clone_path.starts_with("objects/") => Some(serve_git_file(&repo, &clone_path)),
        _ => None,
    }
}

fn serve_git_head(repo: &Repo) -> Vec<u8> {
    let git_dir = repo_git_dir(repo);
    let path = git_dir.join("HEAD");
    match fs::read(path) {
        Ok(bytes) => response_bytes(200, "OK", "text/plain; charset=utf-8", bytes),
        Err(_) => response_bytes(
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        ),
    }
}

fn serve_info_refs(repo: &Repo, req: &Request) -> Vec<u8> {
    if req.query.contains_key("service") {
        // Deliberately serve dumb HTTP refs. Git clients fall back when this is
        // not advertised as smart protocol output.
    }
    let refs = git_output(
        repo,
        &["for-each-ref", "--format=%(objectname)%09%(refname)"],
    )
    .unwrap_or_default();
    response_bytes(200, "OK", "text/plain; charset=utf-8", refs.into_bytes())
}

fn serve_info_packs(repo: &Repo) -> Vec<u8> {
    let pack_dir = repo_git_dir(repo).join("objects").join("pack");
    let mut body = Vec::new();
    if let Ok(entries) = fs::read_dir(pack_dir) {
        let mut packs: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| name.starts_with("pack-") && name.ends_with(".pack"))
            .collect();
        packs.sort();
        for pack in packs {
            body.extend_from_slice(b"P ");
            body.extend_from_slice(pack.as_bytes());
            body.push(b'\n');
        }
    }
    response_bytes(200, "OK", "text/plain; charset=utf-8", body)
}

fn serve_git_file(repo: &Repo, clone_path: &str) -> Vec<u8> {
    if !safe_clone_file_path(clone_path) {
        return response_bytes(
            400,
            "Bad Request",
            "text/plain; charset=utf-8",
            b"bad path".to_vec(),
        );
    }
    let git_dir = repo_git_dir(repo);
    let path = git_dir.join(clone_path);
    let Ok(path) = fs::canonicalize(path) else {
        return response_bytes(
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        );
    };
    if !path.starts_with(&git_dir) {
        return response_bytes(
            403,
            "Forbidden",
            "text/plain; charset=utf-8",
            b"forbidden".to_vec(),
        );
    }
    let Ok(meta) = fs::metadata(&path) else {
        return response_bytes(
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        );
    };
    if meta.len() > MAX_CLONE_FILE_BYTES as u64 {
        return response_bytes(
            413,
            "Payload Too Large",
            "text/plain; charset=utf-8",
            b"too large".to_vec(),
        );
    }
    match fs::read(path) {
        Ok(bytes) => {
            let content_type = if clone_path.ends_with(".pack") || clone_path.ends_with(".idx") {
                "application/octet-stream"
            } else {
                "text/plain; charset=utf-8"
            };
            response_bytes(200, "OK", content_type, bytes)
        }
        Err(_) => response_bytes(
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        ),
    }
}

fn route(req: &Request, state: &AppState) -> String {
    let parts: Vec<&str> = req
        .path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return render_index(state, req);
    }

    if parts[0] != "repo" {
        return response_html(404, "Not Found", page("not found", "<h1>not found</h1>"));
    }

    let Some(repo_name) = parts.get(1) else {
        return response_html(
            400,
            "Bad Request",
            page("missing repository", "<h1>missing repository</h1>"),
        );
    };

    let Some(repo) = find_repo(state, repo_name) else {
        return response_html(
            404,
            "Not Found",
            page("repository not found", "<h1>repository not found</h1>"),
        );
    };

    let view = parts.get(2).copied().unwrap_or("summary");
    match view {
        "summary" => render_summary(&repo, req),
        "log" => render_log(&repo),
        "tree" => render_tree(&repo, req),
        "blob" => render_blob(&repo, req),
        "commit" => render_commit(&repo, req),
        "diff" => render_diff(&repo, req),
        "search" => render_search(&repo, req),
        _ => response_html(404, "Not Found", page("not found", "<h1>not found</h1>")),
    }
}

fn render_index(state: &AppState, req: &Request) -> String {
    let mut repos = list_repos(state);
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    let q_raw = query_or(req, "q", "").trim();
    let q = q_raw.to_ascii_lowercase();

    let mut body = String::from("<h1>rsgit</h1><p>A tiny Git browser.</p><form class=\"index-search\" method=\"get\" action=\"/\"><input name=\"q\" type=\"search\" placeholder=\"search repositories\" value=\"");
    body.push_str(&html_attr(q_raw));
    body.push_str(
        "\"><button>Search</button></form><table><tr><th>Repository</th><th>Description</th></tr>",
    );
    for repo in repos {
        let desc = git_output(&repo, &["description"]).unwrap_or_default();
        let desc_line = first_line(desc.trim()).unwrap_or("");
        if !q.is_empty()
            && !repo.name.to_ascii_lowercase().contains(&q)
            && !desc_line.to_ascii_lowercase().contains(&q)
        {
            continue;
        }
        body.push_str("<tr><td><a href=\"");
        body.push_str(&html_attr(&format!(
            "/repo/{}/summary",
            url_encode_path(&repo.name)
        )));
        body.push_str("\">");
        body.push_str(&html(&repo.name));
        body.push_str("</a></td><td>");
        body.push_str(&html(desc_line));
        body.push_str("</td></tr>");
    }
    body.push_str("</table>");
    response_html(200, "OK", page("rsgit", &body))
}

fn render_summary(repo: &Repo, req: &Request) -> String {
    let branches = git_output(
        repo,
        &[
            "for-each-ref",
            "--count=8",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(objectname:short)%09%(subject)%09%(authorname)%09%(committerdate:relative)",
            "refs/heads",
        ],
    )
    .unwrap_or_default();
    let commits = git_output(
        repo,
        &[
            "log",
            "--decorate=short",
            "--date=short",
            "--pretty=format:%ad%x09%h%x09%D%x09%s%x09%an",
            "-10",
        ],
    )
    .unwrap_or_default();
    let clone_command = public_clone_command(repo, req);

    let mut body = repo_nav(repo, "summary");
    body.push_str("<section class=\"summary-block\"><table><tr><th>Branch</th><th>Commit message</th><th>Author</th><th>Age</th></tr>");
    for line in branches.lines() {
        let cols: Vec<&str> = line.splitn(5, '\t').collect();
        if cols.len() == 5 {
            body.push_str("<tr><td><a href=\"");
            body.push_str(&html_attr(&format!(
                "/repo/{}/log?rev={}",
                url_encode_path(&repo.name),
                url_encode_query(cols[0])
            )));
            body.push_str("\">");
            body.push_str(&html(cols[0]));
            body.push_str("</a></td><td>");
            body.push_str(&html(cols[2]));
            body.push_str("</td><td>");
            body.push_str(&html(cols[3]));
            body.push_str("</td><td class=\"muted\">");
            body.push_str(&html(cols[4]));
            body.push_str("</td></tr>");
        }
    }
    body.push_str("</table></section>");

    body.push_str("<section class=\"summary-block\"><table><tr><th>Age</th><th>Commit message</th><th>Author</th></tr>");
    for line in commits.lines() {
        let cols: Vec<&str> = line.splitn(5, '\t').collect();
        if cols.len() == 5 {
            body.push_str("<tr><td>");
            body.push_str(&html(cols[0]));
            body.push_str("</td><td><a href=\"");
            body.push_str(&html_attr(&format!(
                "/repo/{}/commit?id={}",
                url_encode_path(&repo.name),
                url_encode_query(cols[1])
            )));
            body.push_str("\">");
            body.push_str(&html(cols[3]));
            body.push_str("</a>");
            body.push_str(&render_refs(cols[2]));
            body.push_str("</td><td>");
            body.push_str(&html(cols[4]));
            body.push_str("</td></tr>");
        }
    }
    body.push_str("</table><p><a href=\"");
    body.push_str(&html_attr(&format!(
        "/repo/{}/log",
        url_encode_path(&repo.name)
    )));
    body.push_str("\">[...]</a></p></section>");

    body.push_str("<section class=\"summary-block\"><h2>Clone</h2><div class=\"clone-url\">");
    body.push_str(&html(&clone_command));
    body.push_str("</div></section>");

    response_html(200, "OK", page(&repo.name, &body))
}

fn render_search(repo: &Repo, req: &Request) -> String {
    let q = query_or(req, "q", "").trim();
    let mut body = repo_nav(repo, "search");
    body.push_str("<h1>search</h1>");
    if q.is_empty() {
        body.push_str("<p class=\"muted\">Type a commit-message search above.</p>");
        return response_html(200, "OK", page(&format!("{} search", repo.name), &body));
    }
    if q.len() > MAX_SEARCH_QUERY_LEN || has_control_chars(q) {
        return response_html(
            400,
            "Bad Request",
            page("bad search", "<h1>bad search query</h1>"),
        );
    }

    let args = [
        "log",
        "--all",
        "--fixed-strings",
        "--regexp-ignore-case",
        "--date=short",
        "--pretty=format:%ad%x09%h%x09%s%x09%an",
        "--grep",
        q,
        "-50",
    ];
    let out = git_output(repo, &args).unwrap_or_else(|err| err);
    body.push_str("<table><tr><th>Age</th><th>Commit message</th><th>Author</th></tr>");
    for line in out.lines() {
        let cols: Vec<&str> = line.splitn(4, '\t').collect();
        if cols.len() == 4 {
            body.push_str("<tr><td>");
            body.push_str(&html(cols[0]));
            body.push_str("</td><td><a href=\"");
            body.push_str(&html_attr(&format!(
                "/repo/{}/commit?id={}",
                url_encode_path(&repo.name),
                url_encode_query(cols[1])
            )));
            body.push_str("\">");
            body.push_str(&html(cols[2]));
            body.push_str("</a></td><td>");
            body.push_str(&html(cols[3]));
            body.push_str("</td></tr>");
        }
    }
    body.push_str("</table>");
    response_html(200, "OK", page(&format!("{} search", repo.name), &body))
}

fn render_log(repo: &Repo) -> String {
    let out = git_output(
        repo,
        &[
            "log",
            "--date=short",
            "--pretty=format:%h%x09%ad%x09%an%x09%s",
            "-50",
        ],
    )
    .unwrap_or_default();
    let mut body = repo_nav(repo, "log");
    body.push_str(
        "<h1>log</h1><table><tr><th>Commit</th><th>Date</th><th>Author</th><th>Subject</th></tr>",
    );
    for line in out.lines() {
        let cols: Vec<&str> = line.splitn(4, '\t').collect();
        if cols.len() == 4 {
            body.push_str("<tr><td><a href=\"");
            body.push_str(&html_attr(&format!(
                "/repo/{}/commit?id={}",
                url_encode_path(&repo.name),
                cols[0]
            )));
            body.push_str("\">");
            body.push_str(&html(cols[0]));
            body.push_str("</a></td><td>");
            body.push_str(&html(cols[1]));
            body.push_str("</td><td>");
            body.push_str(&html(cols[2]));
            body.push_str("</td><td>");
            body.push_str(&html(cols[3]));
            body.push_str("</td></tr>");
        }
    }
    body.push_str("</table>");
    response_html(200, "OK", page(&format!("{} log", repo.name), &body))
}

fn render_tree(repo: &Repo, req: &Request) -> String {
    let rev = query_or(req, "rev", "HEAD");
    let path = query_or(req, "path", "");
    if !safe_git_path(path) || !safe_git_rev(rev) {
        return response_html(400, "Bad Request", page("bad path", "<h1>bad path</h1>"));
    }

    let spec = if path.is_empty() {
        rev.to_string()
    } else {
        format!("{rev}:{path}")
    };
    let out = git_output(repo, &["ls-tree", "--end-of-options", &spec]).unwrap_or_default();
    let mut body = repo_nav(repo, "tree");
    body.push_str("<h1>tree</h1><table><tr><th>Mode</th><th>Type</th><th>Name</th></tr>");
    for line in out.lines() {
        if let Some((meta, name)) = line.split_once('\t') {
            let meta_cols: Vec<&str> = meta.split_whitespace().collect();
            if meta_cols.len() >= 3 {
                let child_path = join_repo_path(path, name);
                let href = if meta_cols[1] == "tree" {
                    format!(
                        "/repo/{}/tree?rev={}&path={}",
                        url_encode_path(&repo.name),
                        url_encode_query(rev),
                        url_encode_query(&child_path)
                    )
                } else {
                    format!(
                        "/repo/{}/blob?rev={}&path={}",
                        url_encode_path(&repo.name),
                        url_encode_query(rev),
                        url_encode_query(&child_path)
                    )
                };
                body.push_str("<tr><td>");
                body.push_str(&html(meta_cols[0]));
                body.push_str("</td><td>");
                body.push_str(&html(meta_cols[1]));
                body.push_str("</td><td><a href=\"");
                body.push_str(&html_attr(&href));
                body.push_str("\">");
                body.push_str(&html(name));
                body.push_str("</a></td></tr>");
            }
        }
    }
    body.push_str("</table>");
    response_html(200, "OK", page(&format!("{} tree", repo.name), &body))
}

fn render_blob(repo: &Repo, req: &Request) -> String {
    let rev = query_or(req, "rev", "HEAD");
    let path = query_or(req, "path", "");
    if path.is_empty() || !safe_git_path(path) || !safe_git_rev(rev) {
        return response_html(400, "Bad Request", page("bad path", "<h1>bad path</h1>"));
    }
    let spec = format!("{rev}:{path}");
    match git_output(repo, &["show", "--end-of-options", &spec]) {
        Ok(out) => response_html(
            200,
            "OK",
            page(
                path,
                &format!(
                    "{}<h1>{}</h1><pre>{}</pre>",
                    repo_nav(repo, "tree"),
                    html(path),
                    html(&out)
                ),
            ),
        ),
        Err(_) => response_html(
            404,
            "Not Found",
            page("not found", "<h1>blob not found</h1>"),
        ),
    }
}

fn render_commit(repo: &Repo, req: &Request) -> String {
    let id = query_or(req, "id", "HEAD");
    if !safe_git_rev(id) {
        return response_html(
            400,
            "Bad Request",
            page("bad revision", "<h1>bad revision</h1>"),
        );
    }
    let out = git_output(
        repo,
        &[
            "show",
            "--stat",
            "--patch",
            "--find-renames",
            "--end-of-options",
            id,
        ],
    )
    .unwrap_or_else(|_| "commit not found".to_string());
    let body = format!(
        "{}<h1>commit {}</h1><p><a href=\"/repo/{}/diff?id={}\">diff only</a></p><pre>{}</pre>",
        repo_nav(repo, "log"),
        html(id),
        html_attr(&url_encode_path(&repo.name)),
        html_attr(&url_encode_query(id)),
        html(&out)
    );
    response_html(200, "OK", page(&format!("commit {id}"), &body))
}

fn render_diff(repo: &Repo, req: &Request) -> String {
    let id = query_or(req, "id", "HEAD");
    if !safe_git_rev(id) {
        return response_html(
            400,
            "Bad Request",
            page("bad revision", "<h1>bad revision</h1>"),
        );
    }
    let out = git_output(
        repo,
        &[
            "show",
            "--format=",
            "--patch",
            "--find-renames",
            "--end-of-options",
            id,
        ],
    )
    .unwrap_or_else(|_| "diff not found".to_string());
    let body = format!(
        "{}<h1>diff {}</h1><pre>{}</pre>",
        repo_nav(repo, "log"),
        html(id),
        html(&out)
    );
    response_html(200, "OK", page(&format!("diff {id}"), &body))
}

fn repo_nav(repo: &Repo, active: &str) -> String {
    let base = format!("/repo/{}", url_encode_path(&repo.name));
    let items = [("summary", "summary"), ("log", "log"), ("tree", "tree")];
    let mut nav = String::from("<nav class=\"topbar\"><div><a href=\"/\">index</a>");
    for (label, path) in items {
        nav.push_str(" | ");
        if active == label {
            nav.push_str("<strong>");
        }
        nav.push_str("<a href=\"");
        nav.push_str(&html_attr(&format!("{base}/{path}")));
        nav.push_str("\">");
        nav.push_str(label);
        nav.push_str("</a>");
        if active == label {
            nav.push_str("</strong>");
        }
    }
    nav.push_str("</div><form class=\"search\" method=\"get\" action=\"");
    nav.push_str(&html_attr(&format!("{base}/search")));
    nav.push_str("\"><input name=\"q\" type=\"search\" placeholder=\"search commits\"><button>Search</button></form></nav>");
    nav
}

fn render_refs(raw: &str) -> String {
    let mut out = String::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let class = if item == "HEAD" { "ref head" } else { "ref" };
        out.push(' ');
        out.push_str("<span class=\"");
        out.push_str(class);
        out.push_str("\">");
        out.push_str(&html(item));
        out.push_str("</span>");
    }
    out
}

fn list_repos(state: &AppState) -> Vec<Repo> {
    let mut repos = Vec::new();
    let Ok(entries) = fs::read_dir(&state.repo_root) else {
        return repos;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if let Some(repo) = repo_from_path(state, name, path) {
            repos.push(repo);
        }
    }

    repos
}

fn find_repo(state: &AppState, name: &str) -> Option<Repo> {
    if !safe_repo_name(name) {
        return None;
    }
    let path = state.repo_root.join(name);
    repo_from_path(state, name.to_string(), path)
}

fn repo_from_path(state: &AppState, name: String, path: PathBuf) -> Option<Repo> {
    let meta = fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    let path = fs::canonicalize(path).ok()?;
    if !path.starts_with(&state.repo_root) {
        return None;
    }
    let normal = path.join(".git");
    if normal.is_dir()
        && fs::symlink_metadata(&normal)
            .map(|m| !m.file_type().is_symlink())
            .unwrap_or(false)
    {
        return Some(Repo {
            name,
            path,
            bare: false,
        });
    }
    if path.join("HEAD").is_file() && path.join("objects").is_dir() {
        return Some(Repo {
            name,
            path,
            bare: true,
        });
    }
    None
}

fn repo_git_dir(repo: &Repo) -> PathBuf {
    if repo.bare {
        repo.path.clone()
    } else {
        repo.path.join(".git")
    }
}

fn git_output(repo: &Repo, args: &[&str]) -> Result<String, String> {
    let git = env::var("RSGIT_GIT").unwrap_or_else(|_| "git".to_string());
    let mut cmd = Command::new(git);
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/local/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if repo.bare {
        cmd.arg("--git-dir").arg(&repo.path);
    } else {
        cmd.arg("-C").arg(&repo.path);
    }
    cmd.args(args);

    let mut child = cmd
        .spawn()
        .map_err(|err| format!("failed to execute git: {err}"))?;
    let stdout = child.stdout.take().ok_or("missing git stdout")?;
    let stderr = child.stderr.take().ok_or("missing git stderr")?;
    let out_reader = thread::spawn(move || read_limited(stdout, MAX_GIT_OUTPUT_BYTES));
    let err_reader = thread::spawn(move || read_limited(stderr, 64 * 1024));

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| "git wait failed")? {
            break status;
        }
        if start.elapsed() > GIT_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("git command timed out".to_string());
        }
        thread::sleep(Duration::from_millis(10));
    };

    let (stdout, stdout_truncated) = out_reader
        .join()
        .map_err(|_| "git stdout reader failed")?
        .map_err(|_| "git stdout read failed")?;
    let (stderr, _stderr_truncated) = err_reader
        .join()
        .map_err(|_| "git stderr reader failed")?
        .map_err(|_| "git stderr read failed")?;

    if stdout_truncated {
        return Err("git output too large".to_string());
    }
    if status.success() {
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&stderr);
        eprintln!(
            "git command failed for repo {}: {}",
            repo.name,
            stderr.trim()
        );
        Err("git command failed".to_string())
    }
}

fn read_limited<R: Read>(mut reader: R, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    let mut buf = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let remaining = limit.saturating_sub(out.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = n.min(remaining);
        out.extend_from_slice(&buf[..take]);
        if take < n {
            truncated = true;
        }
    }
    Ok((out, truncated))
}

fn response_html(status: u16, reason: &str, body: String) -> String {
    response(status, reason, "text/html; charset=utf-8", &body)
}

fn response(status: u16, reason: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\n\r\n{body}",
        body.len()
    )
}

fn response_bytes(status: u16, reason: &str, content_type: &str, body: Vec<u8>) -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\n\r\n",
        body.len()
    );
    let mut out = headers.into_bytes();
    out.extend_from_slice(&body);
    out
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{}</style></head><body>{}</body></html>",
        html(title),
        CSS,
        body
    )
}

const CSS: &str = "body{font:16px ui-monospace,SFMono-Regular,Consolas,'Liberation Mono',monospace;max-width:1180px;margin:1.5rem auto;padding:0 1rem;color:#f5f5f5;background:#111}a{color:#f5f5f5;text-decoration:none}a:hover{text-decoration:underline}.topbar{display:flex;gap:1rem;align-items:center;justify-content:space-between;margin-bottom:2rem}.search,.index-search{display:flex;gap:.4rem}.index-search{margin:1.5rem 0 2rem}.search input,.index-search input{background:#1d1d1d;border:1px solid #444;color:#f5f5f5;padding:.35rem .5rem}.search button,.index-search button{background:#2b2b2b;border:1px solid #555;color:#f5f5f5;padding:.35rem .6rem}table{border-collapse:collapse;width:100%;margin-bottom:.5rem}th,td{padding:.25rem .6rem;text-align:left;vertical-align:top}th{font-weight:700}tr:nth-child(even) td{background:#1d1d1d}.summary-block{margin-bottom:3rem}.muted{color:#9b9b9b}.ref{display:inline-block;background:#118611;border:1px solid #31b731;color:#fff;padding:0 .25rem;margin-left:.25rem}.ref.head{background:#9d1732;border-color:#d33}.clone-url{background:#1d1d1d;padding:.35rem .6rem}pre{background:#1d1d1d;border:1px solid #333;overflow:auto;padding:1rem}code{background:#1d1d1d;padding:.1rem .2rem}";

fn html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html_attr(input: &str) -> String {
    html(input).replace('"', "&quot;").replace('\'', "&#x27;")
}

fn first_line(input: &str) -> Option<&str> {
    input.lines().next()
}

fn query_or<'a>(req: &'a Request, key: &str, default: &'a str) -> &'a str {
    req.query.get(key).map(String::as_str).unwrap_or(default)
}

fn join_repo_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}/{child}")
    }
}

fn safe_repo_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
        && !name.contains("..")
        && !has_control_chars(name)
}

fn safe_git_path(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    if path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains(':')
        || has_control_chars(path)
    {
        return false;
    }
    path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn safe_clone_file_path(path: &str) -> bool {
    if has_control_chars(path) || path.contains('\\') || path.contains('\0') || path.contains(':') {
        return false;
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|part| matches!(*part, "" | "." | "..")) {
        return false;
    }
    match parts.as_slice() {
        ["objects", a, b]
            if a.len() == 2
                && b.len() == 38
                && a.bytes().all(|c| c.is_ascii_hexdigit())
                && b.bytes().all(|c| c.is_ascii_hexdigit()) =>
        {
            true
        }
        ["objects", "pack", file]
            if file.starts_with("pack-")
                && (file.ends_with(".pack") || file.ends_with(".idx"))
                && file
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.')) =>
        {
            true
        }
        _ => false,
    }
}

fn safe_git_rev(rev: &str) -> bool {
    if rev.is_empty()
        || rev.len() > 128
        || rev.starts_with('-')
        || rev.contains(':')
        || rev.contains('\\')
        || rev.contains(' ')
        || rev.contains('\0')
        || rev.contains("..")
        || has_control_chars(rev)
    {
        return false;
    }
    rev == "HEAD"
        || rev
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
}

fn has_control_chars(input: &str) -> bool {
    input.chars().any(char::is_control)
}

fn public_clone_command(repo: &Repo, req: &Request) -> String {
    match env::var("RSGIT_PUBLIC_BASE") {
        Ok(base) if !base.trim().is_empty() => {
            let base = base.trim_end_matches('/');
            let url = format!("{base}/repo/{}", url_encode_path(&repo.name));
            if safe_http_clone_url(&url) {
                format!("git clone {url}")
            } else {
                "clone URL not configured".to_string()
            }
        }
        _ => req
            .host
            .as_deref()
            .filter(|host| safe_host(host))
            .map(|host| {
                format!(
                    "git clone http://{host}/repo/{}",
                    url_encode_path(&repo.name)
                )
            })
            .unwrap_or_else(|| "clone URL not configured".to_string()),
    }
}

fn safe_http_clone_url(url: &str) -> bool {
    !url.is_empty()
        && url.len() <= 512
        && !has_control_chars(url)
        && !url.contains(' ')
        && (url.starts_with("https://") || url.starts_with("http://"))
}

fn safe_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && !has_control_chars(host)
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push((a << 4) | b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn url_encode_path(input: &str) -> String {
    url_encode(input, false)
}

fn url_encode_query(input: &str) -> String {
    url_encode(input, true)
}

fn url_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        let ok = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (!encode_slash && b == b'/');
        if ok {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[allow(dead_code)]
fn is_repo_dir(path: &Path) -> bool {
    path.join(".git").is_dir() || (path.join("HEAD").is_file() && path.join("objects").is_dir())
}
