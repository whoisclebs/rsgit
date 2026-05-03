<a id="readme-top"></a>

[![License][license-shield]][license-url]
[![Rust][rust-shield]][rust-url]
[![Docker][docker-shield]][docker-url]
[![Issues][issues-shield]][issues-url]

<br />
<div align="center">
  <h3 align="center">rsgit</h3>

  <p align="center">
    A tiny, read-only Git web browser written in Rust.
    <br />
    <a href="https://github.com/whoisclebs/rsgit"><strong>Explore the repository »</strong></a>
    <br />
    <br />
    <a href="https://github.com/whoisclebs/rsgit/issues">Report Bug</a>
    &middot;
    <a href="https://github.com/whoisclebs/rsgit/issues">Request Feature</a>
  </p>
</div>

<details>
  <summary>Table of Contents</summary>
  <ol>
    <li>
      <a href="#about-the-project">About The Project</a>
      <ul>
        <li><a href="#built-with">Built With</a></li>
        <li><a href="#features">Features</a></li>
        <li><a href="#non-goals">Non-goals</a></li>
      </ul>
    </li>
    <li>
      <a href="#getting-started">Getting Started</a>
      <ul>
        <li><a href="#prerequisites">Prerequisites</a></li>
        <li><a href="#installation">Installation</a></li>
        <li><a href="#configuration">Configuration</a></li>
      </ul>
    </li>
    <li><a href="#usage">Usage</a></li>
    <li><a href="#docker">Docker</a></li>
    <li><a href="#security-model">Security Model</a></li>
    <li><a href="#performance-baseline">Performance Baseline</a></li>
    <li><a href="#roadmap">Roadmap</a></li>
    <li><a href="#development">Development</a></li>
    <li><a href="#license">License</a></li>
  </ol>
</details>

## About The Project

`rsgit` is a lightweight, cgit-inspired Git browser for repositories that are already meant to be visible. It serves a small HTML interface and read-only dumb HTTP clone endpoints from local Git repositories.

The project intentionally separates responsibilities:

- **rsgit**: browse and clone repositories over HTTP, read-only.
- **Git over SSH**: push to bare repositories using `git-receive-pack` outside rsgit.

The current reader implements the subset of Git object storage rsgit needs for browsing, searching recent commits, and serving read-only clone endpoints.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

### Built With

- [Rust][rust-url]
- `std::net::TcpListener` for the minimal HTTP server
- `flate2` with the Rust backend for Git object zlib inflation
- A small custom Git object reader for refs, loose objects, pack indexes, pack objects, and deltas

<p align="right">(<a href="#readme-top">back to top</a>)</p>

### Features

- Repository index
- Repository search on the index page
- Repository summary
- Commit log
- Tree browser
- Blob viewer
- Commit view
- Commit-message search over recent commits
- Dark, cgit-inspired summary page
- Dumb HTTP clone support via `git clone http://host/repo/name`
- Built-in minimal HTTP server
- No `git` subprocess execution
- No push support by design

### Non-goals

- Full cgit compatibility
- Authentication inside rsgit
- Push/write endpoints
- Smart Git HTTP protocol
- Syntax highlighting
- Async runtime or web framework

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Getting Started

### Prerequisites

- Rust toolchain for local builds
- Local Git repositories to browse
- Docker, only if using the container image

Runtime does **not** require the `git` command for page rendering.

### Installation

Clone and build:

```sh
git clone https://github.com/whoisclebs/rsgit.git
cd rsgit
cargo build --release
```

Run locally:

```sh
RSGIT_ADDR=127.0.0.1:9000 \
RSGIT_REPO_ROOT=/path/to/repos \
cargo run --release
```

Open:

```text
http://127.0.0.1:9000/
```

### Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `RSGIT_ADDR` | `127.0.0.1:8080` | Socket address for the HTTP server. |
| `RSGIT_REPO_ROOT` | `.` | Directory scanned one level deep for repositories. |
| `RSGIT_PUBLIC_BASE` | unset | Public base URL used to render clone commands behind a proxy. |

`RSGIT_REPO_ROOT` is scanned one level deep. Each immediate child is treated as a repository if it is either:

- a normal Git working tree with `.git/`, or
- a bare repository with `HEAD` and `objects/`.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Usage

Browse repositories:

```text
http://127.0.0.1:9000/
```

Open a repository summary:

```text
http://127.0.0.1:9000/repo/myrepo.git/summary
```

Clone through rsgit:

```sh
git clone http://127.0.0.1:9000/repo/myrepo.git
```

When deployed behind a public reverse proxy:

```sh
RSGIT_PUBLIC_BASE=https://git.example.com
```

The rendered clone command becomes:

```sh
git clone https://git.example.com/repo/myrepo.git
```

Push is intentionally handled by Git over SSH, not by rsgit:

```sh
git remote add vps ssh://git@example.com:2222/srv/git/myrepo.git
git push vps main
```

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Docker

Pull the published image:

```sh
docker pull ghcr.io/whoisclebs/rsgit:latest
```

Run with a read-only repository mount and restricted privileges:

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
  ghcr.io/whoisclebs/rsgit:latest
```

Or with Compose:

```sh
docker compose up --build
```

Do not mount `/`, `$HOME`, Docker sockets, or private repository trees unless access is intentionally public or protected by external authentication.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Security Model

`rsgit` is designed for public, read-only browsing of repositories.

Safeguards:

- repository names are constrained to one URL path segment;
- repository paths are canonicalized under `RSGIT_REPO_ROOT`;
- symlinked repository directories are rejected;
- revisions and Git tree paths are validated;
- no `git` subprocess execution;
- clone endpoints are limited to Git object and pack files;
- internal `file://` clone paths are never displayed;
- HTTP responses include basic security headers.

For public deployments, put rsgit behind a reverse proxy with TLS, request timeouts, and rate limiting. If repositories are private, add authentication at the proxy layer.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Performance Baseline

Measured on Windows/Cygwin with the `golpher` repository and the release binary:

| Scenario | Working Set | Private Memory | Notes |
| --- | ---: | ---: | --- |
| Idle | ~5 MiB | ~0.7 MiB | Server listening only. |
| After mixed requests | ~6.2 MiB | ~2.0 MiB | Summary, tree, log, blob. |
| Peak observed | ~6.5 MiB | ~2.2 MiB | After additional summary load. |

Sequential `curl` load of 1000 summary requests averaged about `0.06 vCPU` on the test machine.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Roadmap

- [x] Read refs and `packed-refs`
- [x] Read loose Git objects
- [x] Read pack indexes and pack objects
- [x] Resolve OFS/REF deltas
- [x] Render summary, log, commit, tree, blob, and search
- [x] Serve dumb HTTP clone endpoints
- [ ] Native textual diff rendering
- [ ] Streaming clone object/pack responses instead of full-file reads
- [ ] More integration fixtures for packed and delta-heavy repositories
- [ ] Reverse-proxy deployment examples with SSH push container

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Development

Validation:

```sh
cargo fmt -- --check
cargo check
cargo clippy -- -D warnings
cargo test
```

Conventional commits are used:

```text
feat: add new capability
fix: correct broken behavior
refactor: restructure without behavior change
docs: update documentation
```

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## License

Distributed under the MIT License. See [`LICENSE`](LICENSE) for details.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- MARKDOWN LINKS -->
[license-shield]: https://img.shields.io/badge/license-MIT-blue.svg?style=for-the-badge
[license-url]: https://github.com/whoisclebs/rsgit
[rust-shield]: https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white
[rust-url]: https://www.rust-lang.org/
[docker-shield]: https://img.shields.io/badge/GHCR-rsgit-blue?style=for-the-badge&logo=docker&logoColor=white
[docker-url]: https://github.com/whoisclebs/rsgit/pkgs/container/rsgit
[issues-shield]: https://img.shields.io/github/issues/whoisclebs/rsgit.svg?style=for-the-badge
[issues-url]: https://github.com/whoisclebs/rsgit/issues
