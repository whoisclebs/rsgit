//! HTML rendering helpers.

/// Escape text for HTML text-node context.
pub fn text(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape text for HTML attribute context.
pub fn attr(input: &str) -> String {
    text(input).replace('"', "&quot;").replace('\'', "&#x27;")
}

/// Percent-encode a path segment for URLs.
pub fn url_path(input: &str) -> String {
    url_encode(input, false)
}

/// Percent-encode a query value for URLs.
pub fn url_query(input: &str) -> String {
    url_encode(input, true)
}

fn url_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
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

/// Wrap a body fragment in the shared HTML shell.
pub fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{}</style></head><body>{}</body></html>",
        text(title),
        CSS,
        body
    )
}

const CSS: &str = "body{font:16px ui-monospace,SFMono-Regular,Consolas,'Liberation Mono',monospace;max-width:1180px;margin:1.5rem auto;padding:0 1rem;color:#f5f5f5;background:#111}a{color:#f5f5f5;text-decoration:none}a:hover{text-decoration:underline}.topbar{display:flex;gap:1rem;align-items:center;justify-content:space-between;margin-bottom:2rem}.search,.index-search{display:flex;gap:.4rem}.index-search{margin:1.5rem 0 2rem}.search input,.index-search input{background:#1d1d1d;border:1px solid #444;color:#f5f5f5;padding:.35rem .5rem}.search button,.index-search button{background:#2b2b2b;border:1px solid #555;color:#f5f5f5;padding:.35rem .6rem}table{border-collapse:collapse;width:100%;margin-bottom:.5rem}th,td{padding:.25rem .6rem;text-align:left;vertical-align:top}th{font-weight:700}tr:nth-child(even) td{background:#1d1d1d}.summary-block{margin-bottom:3rem}.muted{color:#9b9b9b}.ref{display:inline-block;background:#118611;border:1px solid #31b731;color:#fff;padding:0 .25rem;margin-left:.25rem}.ref.head{background:#9d1732;border-color:#d33}.clone-url{background:#1d1d1d;padding:.35rem .6rem}pre{background:#1d1d1d;border:1px solid #333;overflow:auto;padding:1rem}code{background:#1d1d1d;padding:.1rem .2rem}";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html_text() {
        assert_eq!(text("<x>&"), "&lt;x&gt;&amp;");
    }

    #[test]
    fn escapes_attributes() {
        assert_eq!(attr("'\"&"), "&#x27;&quot;&amp;");
    }
}
