# rsgit

`rsgit` is a tiny Git web browser written in Rust with a small native Git reader. It does not execute the `git` command.

It is intentionally much smaller than cgit. The first goal is a lightweight MVP that can browse local Git repositories using Rust's standard library plus the `git` CLI at runtime.

## Features

- Repository index
- Repository search on the index page
- Repository summary
- Commit log
- Tree browser
- Blob viewer
- Commit view
- Diff view
- Commit-message search
- Dark, cgit-inspired summary page with branch, recent commit, and clone sections
- Basic dumb HTTP Git clone support through `git clone http://host/repo/name`
- Built-in minimal HTTP server
- No `git` subprocess execution

## Non-goals for now

- Full cgit compatibility
- Authentication
- Snapshots/archive downloads
- Smart Git protocol endpoints
- Cache layer
- Syntax highlighting
- Async runtime

## Requirements

- Rust toolchain
- `git` available in `PATH`

## Run

From this directory:

```sh
cargo run
```

By default, `rsgit` listens on `127.0.0.1:8080` and scans the current directory for repositories.

Configure it with environment variables:

```sh
RSGIT_ADDR=127.0.0.1:9000 RSGIT_REPO_ROOT=/path/to/repos cargo run --release
```

Then open:

```text
http://127.0.0.1:9000/
```

## Repository layout

`RSGIT_REPO_ROOT` is scanned one level deep. Each immediate child directory is treated as a repository if it is either:

- a normal Git working tree with `.git/`, or
- a bare repository with `HEAD` and `objects/`.

## Validation

```sh
cargo fmt -- --check
cargo check
```

## Security model

`rsgit` is designed for browsing repositories that are already intended to be visible to its users.

Important defaults and safeguards:

- repository names are constrained to one path segment;
- repository paths are canonicalized under `RSGIT_REPO_ROOT`;
- symlinked repository directories are rejected;
- Git revisions and tree paths are validated before being passed to `git`;
- Git commands are executed without shell interpolation;
- Git command output and runtime are bounded;
- HTTP responses include basic security headers;
- internal `file://` clone paths are not shown;
- the Clone section displays a valid `git clone ...` command pointing back to rsgit.

For public deployments, put `rsgit` behind a reverse proxy with TLS, request timeouts, and rate limiting. If repositories are private, add authentication at the proxy layer.

Optional public base URL override, useful behind a reverse proxy:

```sh
RSGIT_PUBLIC_BASE=https://git.example.com cargo run
```

The displayed command becomes:

```sh
git clone https://git.example.com/repo/myrepo
```

Clone support is intentionally minimal. It serves the dumb HTTP Git endpoints (`HEAD`, `info/refs`, `objects/info/packs`, and object/pack files) directly from the repository. For best compatibility with packed repositories, keep pack files below the configured safety limit.

## Docker

Build:

```sh
docker build -t rsgit:local .
```

Published images are built by GitHub Actions and pushed to GitHub Container Registry:

```sh
docker pull ghcr.io/whoisclebs/rsgit:latest
```

Run with a read-only repository mount and restricted container privileges:

```sh
docker run --rm \
  --read-only \
  --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m \
  --cap-drop=ALL \
  --security-opt no-new-privileges:true \
  --memory=128m \
  --cpus=0.5 \
  --pids-limit=64 \
  -e RSGIT_ADDR=0.0.0.0:8080 \
  -e RSGIT_REPO_ROOT=/repos \
  -v /srv/public-git:/repos:ro \
  -p 127.0.0.1:8080:8080 \
  rsgit:local
```

Or with Compose:

```sh
docker compose up --build
```

Do not mount `/`, `$HOME`, Docker sockets, or private repository trees unless access is intentionally public or protected by external auth.

## Design notes

- Uses `std::net::TcpListener` instead of a web framework.
- Uses a small custom Git object reader for the subset rsgit needs.
- Keeps HTML/CSS inline and minimal.
- Keeps routing simple and explicit.
