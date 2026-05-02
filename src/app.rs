//! Application routing and page rendering.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::config::Config;
use crate::error::Result;
use crate::git::Git;
use crate::html;
use crate::http::{self, Request};
use crate::repo::{self, Repo};
use crate::security::{
    safe_clone_file_path, safe_git_path, safe_git_rev, safe_host, safe_http_clone_url,
};

const MAX_REQUEST_BYTES: usize = 8192;
const MAX_SEARCH_QUERY_LEN: usize = 128;

/// The rsgit application state.
pub struct App {
    config: Config,
    git: Git,
}

impl App {
    /// Create an application with immutable configuration.
    pub fn new(config: Config) -> Self {
        let git = Git::new(&config);
        Self { config, git }
    }

    /// Serve one TCP connection.
    pub fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        let mut buf = [0_u8; MAX_REQUEST_BYTES];
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }

        let raw = String::from_utf8_lossy(&buf[..n]);
        let (response, is_head) = match http::parse(&raw) {
            Some(req) if req.method() == "GET" || req.method() == "HEAD" => {
                let is_head = req.method() == "HEAD";
                (self.route_response(&req), is_head)
            }
            Some(_) => (
                http::response(
                    405,
                    "Method Not Allowed",
                    "text/plain; charset=utf-8",
                    "method not allowed",
                ),
                false,
            ),
            None => (
                http::response(
                    400,
                    "Bad Request",
                    "text/plain; charset=utf-8",
                    "bad request",
                ),
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
        stream.flush()?;
        Ok(())
    }

    fn route_response(&self, req: &Request) -> Vec<u8> {
        if let Some(response) = self.route_clone(req) {
            return response;
        }
        self.route_html(req).into_bytes()
    }

    fn route_clone(&self, req: &Request) -> Option<Vec<u8>> {
        let parts = path_parts(req.path());
        if parts.len() < 3 || parts[0] != "repo" {
            return None;
        }
        let repo = repo::find(&self.config, parts[1])?;
        let clone_path = parts[2..].join("/");
        match clone_path.as_str() {
            "HEAD" => Some(self.serve_git_head(&repo)),
            "info/refs" => Some(self.serve_info_refs(&repo, req)),
            "objects/info/packs" => Some(self.serve_info_packs(&repo)),
            _ if clone_path.starts_with("objects/") => {
                Some(self.serve_git_file(&repo, &clone_path))
            }
            _ => None,
        }
    }

    fn route_html(&self, req: &Request) -> String {
        let parts = path_parts(req.path());
        if parts.is_empty() {
            return self.render_index(req);
        }
        if parts[0] != "repo" {
            return response_html(
                404,
                "Not Found",
                html::page("not found", "<h1>not found</h1>"),
            );
        }
        let Some(repo_name) = parts.get(1) else {
            return response_html(
                400,
                "Bad Request",
                html::page("missing repository", "<h1>missing repository</h1>"),
            );
        };
        let Some(repo) = repo::find(&self.config, repo_name) else {
            return response_html(
                404,
                "Not Found",
                html::page("repository not found", "<h1>repository not found</h1>"),
            );
        };
        match parts.get(2).copied().unwrap_or("summary") {
            "summary" => self.render_summary(&repo, req),
            "log" => self.render_log(&repo),
            "tree" => self.render_tree(&repo, req),
            "blob" => self.render_blob(&repo, req),
            "commit" => self.render_commit(&repo, req),
            "diff" => self.render_diff(&repo, req),
            "search" => self.render_search(&repo, req),
            _ => response_html(
                404,
                "Not Found",
                html::page("not found", "<h1>not found</h1>"),
            ),
        }
    }

    fn render_index(&self, req: &Request) -> String {
        let mut repos = repo::list(&self.config);
        repos.sort_by(|a, b| a.name().cmp(b.name()));
        let q_raw = req.query("q").unwrap_or("").trim();
        let q = q_raw.to_ascii_lowercase();

        let mut body = String::from("<h1>rsgit</h1><p>A tiny Git browser.</p><form class=\"index-search\" method=\"get\" action=\"/\"><input name=\"q\" type=\"search\" placeholder=\"search repositories\" value=\"");
        body.push_str(&html::attr(q_raw));
        body.push_str("\"><button>Search</button></form><table><tr><th>Repository</th><th>Description</th></tr>");
        for repo in repos {
            let desc = self.git.output(&repo, &["description"]).unwrap_or_default();
            let desc_line = desc.lines().next().unwrap_or("").trim();
            if !q.is_empty()
                && !repo.name().to_ascii_lowercase().contains(&q)
                && !desc_line.to_ascii_lowercase().contains(&q)
            {
                continue;
            }
            body.push_str("<tr><td><a href=\"");
            body.push_str(&html::attr(&format!(
                "/repo/{}/summary",
                html::url_path(repo.name())
            )));
            body.push_str("\">");
            body.push_str(&html::text(repo.name()));
            body.push_str("</a></td><td>");
            body.push_str(&html::text(desc_line));
            body.push_str("</td></tr>");
        }
        body.push_str("</table>");
        response_html(200, "OK", html::page("rsgit", &body))
    }

    fn render_summary(&self, repo: &Repo, req: &Request) -> String {
        let branches = self.git.output(repo, &["for-each-ref", "--count=8", "--sort=-committerdate", "--format=%(refname:short)%09%(objectname:short)%09%(subject)%09%(authorname)%09%(committerdate:relative)", "refs/heads"]).unwrap_or_default();
        let commits = self
            .git
            .output(
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
        let mut body = self.repo_nav(repo, "summary");
        body.push_str("<section class=\"summary-block\"><table><tr><th>Branch</th><th>Commit message</th><th>Author</th><th>Age</th></tr>");
        for line in branches.lines() {
            let cols: Vec<&str> = line.splitn(5, '\t').collect();
            if cols.len() == 5 {
                body.push_str(&format!("<tr><td><a href=\"/repo/{}/log?rev={}\">{}</a></td><td>{}</td><td>{}</td><td class=\"muted\">{}</td></tr>", html::attr(&html::url_path(repo.name())), html::attr(&html::url_query(cols[0])), html::text(cols[0]), html::text(cols[2]), html::text(cols[3]), html::text(cols[4])));
            }
        }
        body.push_str("</table></section><section class=\"summary-block\"><table><tr><th>Age</th><th>Commit message</th><th>Author</th></tr>");
        for line in commits.lines() {
            let cols: Vec<&str> = line.splitn(5, '\t').collect();
            if cols.len() == 5 {
                body.push_str(&format!("<tr><td>{}</td><td><a href=\"/repo/{}/commit?id={}\">{}</a>{}</td><td>{}</td></tr>", html::text(cols[0]), html::attr(&html::url_path(repo.name())), html::attr(&html::url_query(cols[1])), html::text(cols[3]), render_refs(cols[2]), html::text(cols[4])));
            }
        }
        body.push_str(&format!(
            "</table><p><a href=\"/repo/{}/log\">[...]</a></p></section>",
            html::attr(&html::url_path(repo.name()))
        ));
        body.push_str("<section class=\"summary-block\"><h2>Clone</h2><div class=\"clone-url\">");
        body.push_str(&html::text(&self.public_clone_command(repo, req)));
        body.push_str("</div></section>");
        response_html(200, "OK", html::page(repo.name(), &body))
    }

    fn render_log(&self, repo: &Repo) -> String {
        let out = self
            .git
            .output(
                repo,
                &[
                    "log",
                    "--date=short",
                    "--pretty=format:%h%x09%ad%x09%an%x09%s",
                    "-50",
                ],
            )
            .unwrap_or_default();
        let mut body = self.repo_nav(repo, "log");
        body.push_str("<h1>log</h1><table><tr><th>Commit</th><th>Date</th><th>Author</th><th>Subject</th></tr>");
        for line in out.lines() {
            let cols: Vec<&str> = line.splitn(4, '\t').collect();
            if cols.len() == 4 {
                body.push_str(&format!("<tr><td><a href=\"/repo/{}/commit?id={}\">{}</a></td><td>{}</td><td>{}</td><td>{}</td></tr>", html::attr(&html::url_path(repo.name())), html::attr(cols[0]), html::text(cols[0]), html::text(cols[1]), html::text(cols[2]), html::text(cols[3])));
            }
        }
        body.push_str("</table>");
        response_html(
            200,
            "OK",
            html::page(&format!("{} log", repo.name()), &body),
        )
    }

    fn render_tree(&self, repo: &Repo, req: &Request) -> String {
        let rev = req.query("rev").unwrap_or("HEAD");
        let path = req.query("path").unwrap_or("");
        if !safe_git_path(path) || !safe_git_rev(rev) {
            return response_html(
                400,
                "Bad Request",
                html::page("bad path", "<h1>bad path</h1>"),
            );
        }
        let spec = if path.is_empty() {
            rev.to_string()
        } else {
            format!("{rev}:{path}")
        };
        let out = self
            .git
            .output(repo, &["ls-tree", "--end-of-options", &spec])
            .unwrap_or_default();
        let mut body = self.repo_nav(repo, "tree");
        body.push_str("<h1>tree</h1><table><tr><th>Mode</th><th>Type</th><th>Name</th></tr>");
        for line in out.lines() {
            if let Some((meta, name)) = line.split_once('\t') {
                let meta_cols: Vec<&str> = meta.split_whitespace().collect();
                if meta_cols.len() >= 3 {
                    let child = if path.is_empty() {
                        name.to_string()
                    } else {
                        format!("{path}/{name}")
                    };
                    let page = if meta_cols[1] == "tree" {
                        "tree"
                    } else {
                        "blob"
                    };
                    body.push_str(&format!("<tr><td>{}</td><td>{}</td><td><a href=\"/repo/{}/{}?rev={}&path={}\">{}</a></td></tr>", html::text(meta_cols[0]), html::text(meta_cols[1]), html::attr(&html::url_path(repo.name())), page, html::attr(&html::url_query(rev)), html::attr(&html::url_query(&child)), html::text(name)));
                }
            }
        }
        body.push_str("</table>");
        response_html(
            200,
            "OK",
            html::page(&format!("{} tree", repo.name()), &body),
        )
    }

    fn render_blob(&self, repo: &Repo, req: &Request) -> String {
        let rev = req.query("rev").unwrap_or("HEAD");
        let path = req.query("path").unwrap_or("");
        if path.is_empty() || !safe_git_path(path) || !safe_git_rev(rev) {
            return response_html(
                400,
                "Bad Request",
                html::page("bad path", "<h1>bad path</h1>"),
            );
        }
        let spec = format!("{rev}:{path}");
        match self.git.output(repo, &["show", "--end-of-options", &spec]) {
            Ok(out) => response_html(
                200,
                "OK",
                html::page(
                    path,
                    &format!(
                        "{}<h1>{}</h1><pre>{}</pre>",
                        self.repo_nav(repo, "tree"),
                        html::text(path),
                        html::text(&out)
                    ),
                ),
            ),
            Err(_) => response_html(
                404,
                "Not Found",
                html::page("not found", "<h1>blob not found</h1>"),
            ),
        }
    }

    fn render_commit(&self, repo: &Repo, req: &Request) -> String {
        let id = req.query("id").unwrap_or("HEAD");
        if !safe_git_rev(id) {
            return response_html(
                400,
                "Bad Request",
                html::page("bad revision", "<h1>bad revision</h1>"),
            );
        }
        let out = self
            .git
            .output(
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
        response_html(200, "OK", html::page(&format!("commit {id}"), &format!("{}<h1>commit {}</h1><p><a href=\"/repo/{}/diff?id={}\">diff only</a></p><pre>{}</pre>", self.repo_nav(repo, "log"), html::text(id), html::attr(&html::url_path(repo.name())), html::attr(&html::url_query(id)), html::text(&out))))
    }

    fn render_diff(&self, repo: &Repo, req: &Request) -> String {
        let id = req.query("id").unwrap_or("HEAD");
        if !safe_git_rev(id) {
            return response_html(
                400,
                "Bad Request",
                html::page("bad revision", "<h1>bad revision</h1>"),
            );
        }
        let out = self
            .git
            .output(
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
        response_html(
            200,
            "OK",
            html::page(
                &format!("diff {id}"),
                &format!(
                    "{}<h1>diff {}</h1><pre>{}</pre>",
                    self.repo_nav(repo, "log"),
                    html::text(id),
                    html::text(&out)
                ),
            ),
        )
    }

    fn render_search(&self, repo: &Repo, req: &Request) -> String {
        let q = req.query("q").unwrap_or("").trim();
        let mut body = self.repo_nav(repo, "search");
        body.push_str("<h1>search</h1>");
        if q.is_empty() {
            body.push_str("<p class=\"muted\">Type a commit-message search above.</p>");
            return response_html(
                200,
                "OK",
                html::page(&format!("{} search", repo.name()), &body),
            );
        }
        if q.len() > MAX_SEARCH_QUERY_LEN || crate::security::has_control_chars(q) {
            return response_html(
                400,
                "Bad Request",
                html::page("bad search", "<h1>bad search query</h1>"),
            );
        }
        let out = self
            .git
            .output(
                repo,
                &[
                    "log",
                    "--all",
                    "--fixed-strings",
                    "--regexp-ignore-case",
                    "--date=short",
                    "--pretty=format:%ad%x09%h%x09%s%x09%an",
                    "--grep",
                    q,
                    "-50",
                ],
            )
            .unwrap_or_default();
        body.push_str("<table><tr><th>Age</th><th>Commit message</th><th>Author</th></tr>");
        for line in out.lines() {
            let cols: Vec<&str> = line.splitn(4, '\t').collect();
            if cols.len() == 4 {
                body.push_str(&format!("<tr><td>{}</td><td><a href=\"/repo/{}/commit?id={}\">{}</a></td><td>{}</td></tr>", html::text(cols[0]), html::attr(&html::url_path(repo.name())), html::attr(&html::url_query(cols[1])), html::text(cols[2]), html::text(cols[3])));
            }
        }
        body.push_str("</table>");
        response_html(
            200,
            "OK",
            html::page(&format!("{} search", repo.name()), &body),
        )
    }

    fn serve_git_head(&self, repo: &Repo) -> Vec<u8> {
        match fs::read(repo.git_dir().join("HEAD")) {
            Ok(bytes) => http::response_bytes(200, "OK", "text/plain; charset=utf-8", bytes),
            Err(_) => http::response_bytes(
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            ),
        }
    }

    fn serve_info_refs(&self, repo: &Repo, req: &Request) -> Vec<u8> {
        if req.has_query("service") {
            // Smart protocol is intentionally not advertised; clients fall back to dumb HTTP.
        }
        let refs = self
            .git
            .output(
                repo,
                &["for-each-ref", "--format=%(objectname)%09%(refname)"],
            )
            .unwrap_or_default();
        http::response_bytes(200, "OK", "text/plain; charset=utf-8", refs.into_bytes())
    }

    fn serve_info_packs(&self, repo: &Repo) -> Vec<u8> {
        let mut body = Vec::new();
        if let Ok(entries) = fs::read_dir(repo.git_dir().join("objects").join("pack")) {
            let mut packs: Vec<String> = entries
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n.starts_with("pack-") && n.ends_with(".pack"))
                .collect();
            packs.sort();
            for pack in packs {
                body.extend_from_slice(b"P ");
                body.extend_from_slice(pack.as_bytes());
                body.push(b'\n');
            }
        }
        http::response_bytes(200, "OK", "text/plain; charset=utf-8", body)
    }

    fn serve_git_file(&self, repo: &Repo, clone_path: &str) -> Vec<u8> {
        if !safe_clone_file_path(clone_path) {
            return http::response_bytes(
                400,
                "Bad Request",
                "text/plain; charset=utf-8",
                b"bad path".to_vec(),
            );
        }
        let git_dir = repo.git_dir();
        let Ok(path) = fs::canonicalize(git_dir.join(clone_path)) else {
            return http::response_bytes(
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            );
        };
        if !path.starts_with(&git_dir) {
            return http::response_bytes(
                403,
                "Forbidden",
                "text/plain; charset=utf-8",
                b"forbidden".to_vec(),
            );
        }
        let Ok(meta) = fs::metadata(&path) else {
            return http::response_bytes(
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            );
        };
        if meta.len() > self.config.max_clone_file_bytes() {
            return http::response_bytes(
                413,
                "Payload Too Large",
                "text/plain; charset=utf-8",
                b"too large".to_vec(),
            );
        }
        match fs::read(path) {
            Ok(bytes) => {
                let content_type = if clone_path.ends_with(".pack") || clone_path.ends_with(".idx")
                {
                    "application/octet-stream"
                } else {
                    "text/plain; charset=utf-8"
                };
                http::response_bytes(200, "OK", content_type, bytes)
            }
            Err(_) => http::response_bytes(
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            ),
        }
    }

    fn repo_nav(&self, repo: &Repo, active: &str) -> String {
        let base = format!("/repo/{}", html::url_path(repo.name()));
        let mut nav = String::from("<nav class=\"topbar\"><div><a href=\"/\">index</a>");
        for (label, path) in [("summary", "summary"), ("log", "log"), ("tree", "tree")] {
            nav.push_str(" | ");
            if active == label {
                nav.push_str("<strong>");
            }
            nav.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                html::attr(&format!("{base}/{path}")),
                label
            ));
            if active == label {
                nav.push_str("</strong>");
            }
        }
        nav.push_str("</div><form class=\"search\" method=\"get\" action=\"");
        nav.push_str(&html::attr(&format!("{base}/search")));
        nav.push_str("\"><input name=\"q\" type=\"search\" placeholder=\"search commits\"><button>Search</button></form></nav>");
        nav
    }

    fn public_clone_command(&self, repo: &Repo, req: &Request) -> String {
        match self.config.public_base() {
            Some(base) => {
                let url = format!(
                    "{}/repo/{}",
                    base.trim_end_matches('/'),
                    html::url_path(repo.name())
                );
                if safe_http_clone_url(&url) {
                    format!("git clone {url}")
                } else {
                    "clone URL not configured".to_string()
                }
            }
            None => req
                .host()
                .filter(|host| safe_host(host))
                .map(|host| {
                    format!(
                        "git clone http://{host}/repo/{}",
                        html::url_path(repo.name())
                    )
                })
                .unwrap_or_else(|| "clone URL not configured".to_string()),
        }
    }
}

fn path_parts(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

fn response_html(status: u16, reason: &str, body: String) -> String {
    String::from_utf8(http::response(
        status,
        reason,
        "text/html; charset=utf-8",
        &body,
    ))
    .unwrap_or_default()
}

fn render_refs(raw: &str) -> String {
    let mut out = String::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let class = if item == "HEAD" { "ref head" } else { "ref" };
        out.push_str(&format!(
            " <span class=\"{}\">{}</span>",
            class,
            html::text(item)
        ));
    }
    out
}
