//! Finding the current download URL of an image and fetching the file.

use std::cmp::Reverse;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ureq::Agent;

/// How many of the newest directories to try before giving up; the very
/// newest one is sometimes created before the image is published.
const FALLBACKS: usize = 3;

pub fn agent() -> Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
}

fn http_err(url: &str, e: &ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("{url}: HTTP {code}"),
        ureq::Error::Transport(t) => match t.message() {
            Some(msg) => format!("{url}: {msg}"),
            None => format!("{url}: {}", t.kind()),
        },
    }
}

/// Matches a name against a pattern where `*` is anything, `#` a run of
/// digits, `%` a version (digits and dots) and `[abc]` one of the listed
/// characters.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| glob(rest, &name[i..])),
        Some((b'#', rest)) => {
            let digits = name.iter().take_while(|c| c.is_ascii_digit()).count();
            (1..=digits).rev().any(|i| glob(rest, &name[i..]))
        }
        Some((b'%', rest)) => {
            let version = |c: &&u8| c.is_ascii_digit() || **c == b'.';
            let len = name.iter().take_while(version).count();
            (1..=len).rev().any(|i| glob(rest, &name[i..]))
        }
        Some((b'[', rest)) => match rest.iter().position(|&c| c == b']') {
            Some(end) => name
                .split_first()
                .is_some_and(|(c, tail)| rest[..end].contains(c) && glob(&rest[end + 1..], tail)),
            None => false,
        },
        Some((c, rest)) => name
            .split_first()
            .is_some_and(|(d, tail)| c == d && glob(rest, tail)),
    }
}

fn is_pattern(segment: &str) -> bool {
    segment.contains(['*', '#', '%', '['])
}

/// The numbers in a name, so that 9.10 sorts after 9.9 and 2025.10.01 after 2025.9.30.
fn version(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .map(|run| run.parse().unwrap_or(u64::MAX))
        .collect()
}

/// The last path segment of a URL, without query string.
pub fn file_name(url: &str) -> &str {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    url.trim_end_matches('/').rsplit('/').next().unwrap_or(url)
}

/// Everything on a page that looks like a link whose file name matches the
/// pattern, as (file name, link) pairs with the newest version first.
///
/// A directory listing also shows dates and sizes, which a pattern may match
/// by accident, so `hrefs_only` keeps to `href` attributes. Other pages (JSON
/// among them) are searched whole, with full URLs preferred over bare names.
fn links<'a>(body: &'a str, pattern: &str, hrefs_only: bool) -> Vec<(&'a str, &'a str)> {
    let mut previous = "";
    let mut found: Vec<(&str, &str)> = body
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>'))
        .filter(|token| {
            let is_href = previous.to_ascii_lowercase().ends_with("href=");
            previous = token;
            is_href || !hrefs_only
        })
        .map(|token| token.split('?').next().unwrap_or(token))
        .map(|link| (file_name(link), link))
        .filter(|(name, _)| glob(pattern.as_bytes(), name.as_bytes()))
        .collect();
    found.sort_by_key(|(name, link)| Reverse((version(name), *name, link.contains("://"))));
    found.dedup_by_key(|(name, _)| *name);
    found
}

fn fetch(agent: &Agent, url: &str) -> Result<(String, String), String> {
    let resp = agent.get(url).call().map_err(|e| http_err(url, &e))?;
    let final_url = resp.get_url().to_string();
    let body = resp.into_string().map_err(|e| format!("{url}: {e}"))?;
    // JSON may write slashes as `\/`.
    Ok((final_url, body.replace("\\/", "/")))
}

/// Makes a link found on `page` absolute.
fn join(page: &str, link: &str) -> String {
    let scheme_end = page.find("://").map_or(0, |i| i + 3);
    let origin_end = page[scheme_end..]
        .find('/')
        .map_or(page.len(), |i| i + scheme_end);
    if link.starts_with("http://") || link.starts_with("https://") {
        link.to_string()
    } else if let Some(rest) = link.strip_prefix("//") {
        format!("{}{rest}", &page[..scheme_end])
    } else if link.starts_with('/') {
        format!("{}{link}", &page[..origin_end])
    } else {
        let dir_end = page.rfind('/').filter(|&i| i >= origin_end);
        match dir_end {
            Some(i) => format!("{}{link}", &page[..=i]),
            None => format!("{page}/{link}"),
        }
    }
}

/// Follows a URL whose path segments may be patterns, reading the directory
/// listing wherever one is and taking the newest match.
fn walk(agent: &Agent, base: &str, segments: &[&str]) -> Result<String, String> {
    let Some((segment, rest)) = segments.split_first() else {
        return Ok(base.trim_end_matches('/').to_string());
    };
    if !is_pattern(segment) {
        let next = format!("{base}{segment}");
        return match rest {
            [] => Ok(next),
            _ => walk(agent, &format!("{next}/"), rest),
        };
    }
    let (_, body) = fetch(agent, base)?;
    let mut first_err = format!("{base}: nic neodpovídá vzoru {segment}");
    let hrefs_only = body.to_ascii_lowercase().contains("href=");
    let found = links(&body, segment, hrefs_only);
    for (i, (name, _)) in found.iter().take(FALLBACKS).enumerate() {
        if rest.is_empty() {
            return Ok(format!("{base}{name}"));
        }
        match walk(agent, &format!("{base}{name}/"), rest) {
            Ok(url) => return Ok(url),
            Err(e) if i == 0 => first_err = e,
            Err(_) => {}
        }
    }
    Err(first_err)
}

/// Finds the current download URL for a catalog source, which is either
///
/// - a URL, where path segments may be patterns (see `glob`) resolved from
///   directory listings, or
/// - `scan <page> <pattern>`: the newest link on the page whose file name
///   matches the pattern.
pub fn resolve(agent: &Agent, source: &str) -> Result<String, String> {
    if let Some(rest) = source.strip_prefix("scan ") {
        let (page, pattern) = rest
            .split_once(' ')
            .ok_or_else(|| format!("neplatný zdroj {source:?}"))?;
        let (page, body) = fetch(agent, page)?;
        let found = links(&body, pattern.trim(), false);
        let (_, link) = found
            .first()
            .ok_or_else(|| format!("{page}: nic neodpovídá vzoru {pattern}"))?;
        return Ok(join(&page, link));
    }
    let bad = || format!("neplatný zdroj {source:?}");
    let scheme_end = source.find("://").ok_or_else(bad)? + 3;
    let mut segments = source[scheme_end..].split('/');
    let host = segments.next().filter(|h| !h.is_empty()).ok_or_else(bad)?;
    let segments: Vec<&str> = segments.collect();
    walk(
        agent,
        &format!("{}{host}/", &source[..scheme_end]),
        &segments,
    )
}

/// Fails when the server answers with a web page, as download portals do
/// for links that are not the file itself.
fn expect_file(url: &str, resp: &ureq::Response) -> Result<(), String> {
    if resp.content_type() == "text/html" {
        return Err(format!("{url}: server místo obrazu vrací webovou stránku"));
    }
    Ok(())
}

/// Size of the file behind a URL, when the server tells.
pub fn size(agent: &Agent, url: &str) -> Result<Option<u64>, String> {
    let resp = agent.head(url).call().map_err(|e| http_err(url, &e))?;
    expect_file(url, &resp)?;
    Ok(resp.header("Content-Length").and_then(|v| v.parse().ok()))
}

pub enum Fetched {
    New,
    /// The file was already in the directory.
    Existing,
}

/// Downloads a URL into the directory. The data goes to `<name>.part`, which
/// is renamed when complete, and an interrupted download continues from
/// where the partial file ends. `progress` gets bytes done and the total.
pub fn download(
    agent: &Agent,
    url: &str,
    dir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Fetched, String> {
    let name = file_name(url);
    if name.is_empty() {
        return Err(format!("{url}: adresa neobsahuje jméno souboru"));
    }
    let disk = |what: &str, p: &Path, e: io::Error| format!("nelze {what} {}: {e}", p.display());
    fs::create_dir_all(dir).map_err(|e| disk("vytvořit", dir, e))?;
    let dest = dir.join(name);
    if dest.exists() {
        return Ok(Fetched::Existing);
    }
    let part = dir.join(format!("{name}.part"));
    let mut done = fs::metadata(&part).map_or(0, |m| m.len());

    let mut request = agent.get(url);
    if done > 0 {
        request = request.set("Range", &format!("bytes={done}-"));
    }
    let resp = match request.call() {
        // Nothing left after the partial file, so it cannot be continued.
        Err(ureq::Error::Status(416, _)) => agent.get(url).call(),
        other => other,
    }
    .map_err(|e| http_err(url, &e))?;
    expect_file(url, &resp)?;
    if resp.status() != 206 {
        done = 0;
    }
    let length: Option<u64> = resp.header("Content-Length").and_then(|v| v.parse().ok());
    let total = length.map(|n| n + done);

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(done > 0)
        .truncate(done == 0)
        .open(&part)
        .map_err(|e| disk("otevřít", &part, e))?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 16];
    progress(done, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("zrušeno".to_string());
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("{name}: {e}")),
        };
        file.write_all(&buf[..n])
            .map_err(|e| disk("zapsat", &part, e))?;
        done += n as u64;
        progress(done, total);
    }
    if total.is_some_and(|t| t != done) {
        return Err(format!(
            "{name}: spojení skončilo předčasně, další pokus naváže"
        ));
    }
    drop(file);
    fs::rename(&part, &dest).map_err(|e| disk("přejmenovat", &part, e))?;
    Ok(Fetched::New)
}

/// A size in MB or GB, as shown to the user.
pub fn human(bytes: u64) -> String {
    let mb = bytes as f64 / (1 << 20) as f64;
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(pattern: &str, name: &str) -> bool {
        glob(pattern.as_bytes(), name.as_bytes())
    }

    #[test]
    fn glob_patterns() {
        assert!(g(
            "debian-#.#.#-amd64-netinst.iso",
            "debian-13.1.0-amd64-netinst.iso"
        ));
        assert!(!g(
            "debian-#.#.#-amd64-netinst.iso",
            "debian-13.1.0-amd64-netinst.iso.sig"
        ));
        assert!(!g(
            "debian-#.#.#-amd64-netinst.iso",
            "debian-edu-13.1.0-amd64-netinst.iso"
        ));
        assert!(g("#", "42"));
        assert!(!g("#", "test"));
        assert!(!g("#", "42.1"));
        assert!(g("2[02468].04", "26.04"));
        assert!(!g("2[02468].04", "25.04"));
        assert!(g(
            "ubuntu-*-desktop-amd64.iso",
            "ubuntu-26.04.1-desktop-amd64.iso"
        ));
        assert!(g("#*", "22"));
        assert!(g("#*", "22.2"));
        assert!(g(
            "ubuntu-%-desktop-amd64.iso",
            "ubuntu-26.04.1-desktop-amd64.iso"
        ));
        assert!(!g(
            "ubuntu-%-desktop-amd64.iso",
            "ubuntu-26.10-beta-desktop-amd64.iso"
        ));
    }

    #[test]
    fn newest_link_first() {
        let body = r#"<a href="../">up</a>
            <a href="9/">9/</a> <a href="10/">10/</a> <a href="8.10/">8.10/</a>
            <a href="?C=N;O=D">Name</a>"#;
        let names = |hrefs| -> Vec<&str> {
            let found = links(body, "#", hrefs);
            found.iter().map(|(n, _)| *n).collect()
        };
        assert_eq!(names(true), ["10", "9"]);

        // The size column of a listing is not a directory.
        let body = r#"<a href="9/">9/</a> 2026-01-01 74663091"#;
        assert_eq!(links(body, "#", true).len(), 1);
        assert_eq!(links(body, "#", false).len(), 2);

        let body = r#"{"browser_download_url":"https://example.org/dl/v1.9/tool-1.9.iso"}
            {"browser_download_url":"https://example.org/dl/v1.10/tool-1.10.iso"}"#;
        let found = links(body, "tool-*.iso", false);
        assert_eq!(found[0].1, "https://example.org/dl/v1.10/tool-1.10.iso");

        // A bare name next to the full URL, as in the GitHub API.
        let body = r#"{"name":"tool.iso","url":"https://example.org/dl/v2/tool.iso"}"#;
        assert_eq!(
            links(body, "tool.iso", false)[0].1,
            "https://example.org/dl/v2/tool.iso"
        );
    }

    #[test]
    fn relative_links() {
        let page = "https://example.org/a/b/index.html";
        assert_eq!(join(page, "c.iso"), "https://example.org/a/b/c.iso");
        assert_eq!(join(page, "/c.iso"), "https://example.org/c.iso");
        assert_eq!(
            join(page, "//cdn.example.org/c.iso"),
            "https://cdn.example.org/c.iso"
        );
        assert_eq!(
            join(page, "http://other.org/c.iso"),
            "http://other.org/c.iso"
        );
        assert_eq!(
            join("https://example.org", "c.iso"),
            "https://example.org/c.iso"
        );
    }

    #[test]
    fn names_and_sizes() {
        assert_eq!(file_name("https://example.org/a/b.iso?x=1"), "b.iso");
        assert_eq!(file_name("https://example.org/a/dir/"), "dir");
        assert_eq!(human(700 << 20), "700 MB");
        assert_eq!(human(3 << 30), "3.00 GB");
    }
}
