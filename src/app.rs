//! Application routing and page rendering.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::config::Config;
use crate::error::Result;
use crate::git;
use crate::html;
use crate::http::{self, Request};
use crate::repo::{self, Repo};
use crate::security::{safe_clone_file_path, safe_host, safe_http_clone_url};

const MAX_REQUEST_BYTES: usize = 8192;
const MAX_SEARCH_QUERY_LEN: usize = 128;

/// rsgit application state.
pub struct App {
    config: Config,
}

impl App {
    /// Create an application with immutable configuration.
    pub fn new(config: Config) -> Self {
        Self { config }
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
            "info/refs" => Some(self.serve_info_refs(&repo)),
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
            return html_response(
                404,
                "Not Found",
                html::page("not found", "<h1>not found</h1>"),
            );
        }
        let Some(repo_name) = parts.get(1) else {
            return html_response(
                400,
                "Bad Request",
                html::page("missing repository", "<h1>missing repository</h1>"),
            );
        };
        let Some(repo) = repo::find(&self.config, repo_name) else {
            return html_response(
                404,
                "Not Found",
                html::page("repository not found", "<h1>repository not found</h1>"),
            );
        };
        match parts.get(2).copied().unwrap_or("summary") {
            "summary" => self.render_summary(&repo, req),
            "log" => self.render_log(&repo),
            "tree" => self.not_implemented(
                &repo,
                "tree",
                "Native tree parsing requires object decompression support.",
            ),
            "blob" => self.not_implemented(
                &repo,
                "blob",
                "Native blob parsing requires object decompression support.",
            ),
            "commit" => self.not_implemented(
                &repo,
                "commit",
                "Native commit parsing requires object decompression support.",
            ),
            "diff" => self.not_implemented(
                &repo,
                "diff",
                "Native diff parsing requires object decompression support.",
            ),
            "search" => self.render_search(&repo, req),
            _ => html_response(
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
        let mut body = format!("<h1>rsgit</h1><p>A tiny Git browser.</p><form class=\"index-search\" method=\"get\" action=\"/\"><input name=\"q\" type=\"search\" placeholder=\"search repositories\" value=\"{}\"><button>Search</button></form><table><tr><th>Repository</th><th>Description</th></tr>", html::attr(q_raw));
        for repo in repos {
            let desc = git::description(&repo);
            if !q.is_empty()
                && !repo.name().to_ascii_lowercase().contains(&q)
                && !desc.to_ascii_lowercase().contains(&q)
            {
                continue;
            }
            body.push_str(&format!(
                "<tr><td><a href=\"/repo/{}/summary\">{}</a></td><td>{}</td></tr>",
                html::attr(&html::url_path(repo.name())),
                html::text(repo.name()),
                html::text(&desc)
            ));
        }
        body.push_str("</table>");
        html_response(200, "OK", html::page("rsgit", &body))
    }

    fn render_summary(&self, repo: &Repo, req: &Request) -> String {
        let mut body = self.repo_nav(repo, "summary");
        body.push_str("<section class=\"summary-block\"><table><tr><th>Branch</th><th>Object</th><th>Backend</th><th>Age</th></tr>");
        for branch in git::branches(repo).iter().take(8) {
            let short = branch.name.trim_start_matches("refs/heads/");
            let short_oid = branch.oid.get(..8).unwrap_or(&branch.oid);
            body.push_str(&format!("<tr><td>{}</td><td>{}</td><td>manual filesystem reader</td><td class=\"muted\">unknown</td></tr>", html::text(short), html::text(short_oid)));
        }
        body.push_str("</table></section><section class=\"summary-block\"><table><tr><th>Status</th><th>Message</th><th>Owner</th></tr><tr><td>manual</td><td>Commit, tree, blob, search, and diff parsing are disabled until native object decompression is implemented.</td><td>rsgit</td></tr></table></section>");
        body.push_str("<section class=\"summary-block\"><h2>Clone</h2><div class=\"clone-url\">");
        body.push_str(&html::text(&self.public_clone_command(repo, req)));
        body.push_str("</div></section>");
        html_response(200, "OK", html::page(repo.name(), &body))
    }

    fn render_log(&self, repo: &Repo) -> String {
        let mut body = self.repo_nav(repo, "log");
        body.push_str("<h1>refs</h1><table><tr><th>Object</th><th>Ref</th></tr>");
        for r in git::refs(repo) {
            let short = r.oid.get(..8).unwrap_or(&r.oid);
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td></tr>",
                html::text(short),
                html::text(&r.name)
            ));
        }
        body.push_str("</table>");
        html_response(
            200,
            "OK",
            html::page(&format!("{} refs", repo.name()), &body),
        )
    }

    fn render_search(&self, repo: &Repo, req: &Request) -> String {
        let q = req.query("q").unwrap_or("").trim();
        if q.len() > MAX_SEARCH_QUERY_LEN || crate::security::has_control_chars(q) {
            return html_response(
                400,
                "Bad Request",
                html::page("bad search", "<h1>bad search query</h1>"),
            );
        }
        self.not_implemented(
            repo,
            "search",
            "Commit search is disabled until native commit parsing is implemented.",
        )
    }

    fn not_implemented(&self, repo: &Repo, page: &str, message: &str) -> String {
        html_response(
            501,
            "Not Implemented",
            html::page(
                page,
                &format!(
                    "{}<h1>{}</h1><p class=\"muted\">{}</p>",
                    self.repo_nav(repo, page),
                    html::text(page),
                    html::text(message)
                ),
            ),
        )
    }

    fn serve_git_head(&self, repo: &Repo) -> Vec<u8> {
        git::head_bytes(repo)
            .map(|bytes| http::response_bytes(200, "OK", "text/plain; charset=utf-8", bytes))
            .unwrap_or_else(|| {
                http::response_bytes(
                    404,
                    "Not Found",
                    "text/plain; charset=utf-8",
                    b"not found".to_vec(),
                )
            })
    }

    fn serve_info_refs(&self, repo: &Repo) -> Vec<u8> {
        http::response_bytes(200, "OK", "text/plain; charset=utf-8", git::info_refs(repo))
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
                body.extend_from_slice(format!("P {pack}\n").as_bytes());
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
            Ok(bytes) => http::response_bytes(
                200,
                "OK",
                if clone_path.ends_with(".pack") || clone_path.ends_with(".idx") {
                    "application/octet-stream"
                } else {
                    "text/plain; charset=utf-8"
                },
                bytes,
            ),
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

fn html_response(status: u16, reason: &str, body: String) -> String {
    String::from_utf8(http::response(
        status,
        reason,
        "text/html; charset=utf-8",
        &body,
    ))
    .unwrap_or_default()
}
