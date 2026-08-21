//! Cookie header and Netscape cookie-file matching for direct HTTP reads.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderValue;
use url::Url;

use crate::error::ReadError;

/// Caller-supplied cookies for direct HTTP(S) URL acquisition.
#[derive(Clone)]
pub enum CookieSource {
    /// A single `Cookie` request header line.
    Header(String),
    /// Netscape HTTP cookie records; matching is URL-scoped at send time.
    Netscape(Vec<NetscapeCookie>),
}

#[derive(Clone)]
pub struct NetscapeCookie {
    domain: String,
    include_subdomains: bool,
    path: String,
    secure: bool,
    expires: Option<i64>,
    name: String,
    value: String,
}

impl std::fmt::Debug for CookieSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Header(_) => formatter
                .debug_tuple("Header")
                .field(&"[redacted]")
                .finish(),
            Self::Netscape(records) => formatter
                .debug_struct("Netscape")
                .field("records", &records.len())
                .finish(),
        }
    }
}

impl std::fmt::Debug for NetscapeCookie {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NetscapeCookie")
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("secure", &self.secure)
            .field("name", &self.name)
            .field("value", &"[redacted]")
            .finish()
    }
}

impl CookieSource {
    /// Parse a cookie header line or a Netscape cookies.txt file.
    pub fn parse(contents: &str) -> Result<Self, ReadError> {
        if looks_netscape(contents) {
            let records = parse_netscape(contents);
            if records.is_empty() {
                return Err(ReadError::EmptyNetscapeCookies);
            }
            return Ok(Self::Netscape(records));
        }
        Ok(Self::Header(validate_cookie_header(contents)?.to_owned()))
    }

    /// Load a Cookie header line or Netscape cookie file from disk.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ReadError> {
        let path = path.as_ref();
        let metadata = std::fs::metadata(path).map_err(|source| ReadError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ReadError::NotRegularFile {
                path: path.to_path_buf(),
            });
        }
        let contents = std::fs::read_to_string(path).map_err(|source| ReadError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&contents)
    }

    /// Build a header-line cookie source.
    pub fn header(value: impl Into<String>) -> Result<Self, ReadError> {
        Ok(Self::Header(
            validate_cookie_header(&value.into())?.to_owned(),
        ))
    }

    /// Cookie header to send to `url`, if any matching cookies exist.
    pub fn header_for_url(&self, url: &Url) -> Result<Option<String>, ReadError> {
        match self {
            Self::Header(value) => Ok(Some(validate_cookie_header(value)?.to_owned())),
            Self::Netscape(records) => Ok(header_from_records(records, url, unix_now())),
        }
    }
}

pub(crate) fn validate_cookie_header(cookie: &str) -> Result<&str, ReadError> {
    let cookie = cookie.trim();
    if cookie.is_empty() || cookie.contains('\r') || cookie.contains('\n') {
        return Err(ReadError::InvalidCookieHeader);
    }
    if HeaderValue::from_str(cookie).is_err() {
        return Err(ReadError::InvalidCookieHeaderBytes);
    }
    Ok(cookie)
}

fn looks_netscape(contents: &str) -> bool {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("# Netscape") || line.starts_with("# HTTP Cookie File") {
            return true;
        }
        let domain = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if domain.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        return fields.len() >= 7
            && parse_netscape_flag(fields[1]).is_some()
            && parse_netscape_flag(fields[3]).is_some();
    }
    false
}

fn parse_netscape(contents: &str) -> Vec<NetscapeCookie> {
    let mut records = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || (trimmed.starts_with('#') && !trimmed.starts_with("#HttpOnly_")) {
            continue;
        }
        if let Some(record) = parse_netscape_line(trimmed) {
            records.push(record);
        }
    }
    records
}

fn parse_netscape_line(line: &str) -> Option<NetscapeCookie> {
    let fields: Vec<&str> = line
        .strip_prefix("#HttpOnly_")
        .unwrap_or(line)
        .split('\t')
        .collect();
    if fields.len() < 7 {
        return None;
    }
    let include_subdomains = parse_netscape_flag(fields[1])? || fields[0].starts_with('.');
    let secure = parse_netscape_flag(fields[3])?;
    let expires = fields[4].parse::<i64>().ok()?;
    let name = fields[5];
    if name.is_empty() {
        return None;
    }
    Some(NetscapeCookie {
        domain: fields[0].trim_start_matches('.').to_ascii_lowercase(),
        include_subdomains,
        path: if fields[2].is_empty() {
            "/".to_owned()
        } else {
            fields[2].to_owned()
        },
        secure,
        expires: (expires > 0).then_some(expires),
        name: name.to_owned(),
        value: fields[6..].join("\t"),
    })
}

fn parse_netscape_flag(value: &str) -> Option<bool> {
    match value {
        "TRUE" | "true" => Some(true),
        "FALSE" | "false" => Some(false),
        _ => None,
    }
}

fn header_from_records(records: &[NetscapeCookie], url: &Url, now: i64) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    let https = url.scheme() == "https";
    let mut matched: Vec<&NetscapeCookie> = records
        .iter()
        .filter(|cookie| cookie_matches(cookie, &host, path, https, now))
        .collect();
    matched.sort_by(|left, right| {
        right
            .path
            .len()
            .cmp(&left.path.len())
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut seen = std::collections::BTreeSet::new();
    let mut parts = Vec::new();
    for cookie in matched {
        if seen.insert(&cookie.name) {
            parts.push(format!("{}={}", cookie.name, cookie.value));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

fn cookie_matches(cookie: &NetscapeCookie, host: &str, path: &str, https: bool, now: i64) -> bool {
    if cookie.secure && !https {
        return false;
    }
    if cookie.expires.is_some_and(|expires| expires <= now) {
        return false;
    }
    domain_matches(&cookie.domain, cookie.include_subdomains, host)
        && path_matches(&cookie.path, path)
}

fn domain_matches(cookie_domain: &str, include_subdomains: bool, host: &str) -> bool {
    if cookie_domain.is_empty() {
        return false;
    }
    if host.eq_ignore_ascii_case(cookie_domain) {
        return true;
    }
    if !include_subdomains || is_ip_host(host) {
        return false;
    }
    host.ends_with(&format!(".{cookie_domain}"))
}

fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    let cookie_path = if cookie_path.is_empty() {
        "/"
    } else {
        cookie_path
    };
    let request_path = if request_path.is_empty() {
        "/"
    } else {
        request_path
    };
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

fn is_ip_host(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

fn unix_now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{CookieSource, header_from_records, parse_netscape};
    use crate::error::ReadError;
    use url::Url;

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    #[test]
    fn parses_a_cookie_header_line() {
        let source = CookieSource::parse("sid=one; theme=light").unwrap();
        assert_eq!(
            source
                .header_for_url(&url("http://app.example.test/docs"))
                .unwrap()
                .as_deref(),
            Some("sid=one; theme=light")
        );
    }

    #[test]
    fn netscape_sends_only_matching_host_cookies() {
        let source = CookieSource::parse(
            "# Netscape HTTP Cookie File\n\
             .youtube.com\tTRUE\t/\tTRUE\t2147483647\tsid\tmust-not-send\n\
             app.example.test\tFALSE\t/\tFALSE\t0\tsid\tsession-one\n\
             app.example.test\tFALSE\t/app\tFALSE\t0\ttenant\tabc\n",
        )
        .unwrap();
        let header = source
            .header_for_url(&url("http://app.example.test/app/item"))
            .unwrap()
            .expect("host cookies");
        assert!(header.contains("sid=session-one"));
        assert!(header.contains("tenant=abc"));
        assert!(!header.contains("must-not-send"));
        assert!(!header.contains("youtube"));
    }

    #[test]
    fn netscape_does_not_send_secure_cookies_over_http() {
        let records = parse_netscape("app.example.test\tFALSE\t/\tTRUE\t0\tsid\tsecure-only\n");
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/app"), 1),
            None
        );
        assert_eq!(
            header_from_records(&records, &url("https://app.example.test/app"), 1).as_deref(),
            Some("sid=secure-only")
        );
    }

    #[test]
    fn netscape_skips_expired_cookies() {
        let records = parse_netscape("app.example.test\tFALSE\t/\tFALSE\t10\tsid\told\n");
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/"), 11),
            None
        );
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/"), 9).as_deref(),
            Some("sid=old")
        );
    }

    #[test]
    fn subdomain_flag_does_not_match_unrelated_hosts() {
        let records = parse_netscape(".ample.test\tTRUE\t/\tFALSE\t0\tsid\tnope\n");
        assert_eq!(
            header_from_records(&records, &url("http://example.test/"), 1),
            None
        );
    }

    #[test]
    fn netscape_leading_dot_matches_subdomains() {
        let records = parse_netscape(".example.test\tTRUE\t/\tFALSE\t0\tsid\tok\n");
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/"), 1).as_deref(),
            Some("sid=ok")
        );
        assert_eq!(
            header_from_records(&records, &url("http://example.test/"), 1).as_deref(),
            Some("sid=ok")
        );
    }

    #[test]
    fn netscape_host_only_does_not_match_child_hosts() {
        let records = parse_netscape("app.example.test\tFALSE\t/\tFALSE\t0\tsid\tok\n");
        assert_eq!(
            header_from_records(&records, &url("http://sub.app.example.test/"), 1),
            None
        );
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/"), 1).as_deref(),
            Some("sid=ok")
        );
    }

    #[test]
    fn netscape_path_is_a_prefix_boundary() {
        let records = parse_netscape("app.example.test\tFALSE\t/foo\tFALSE\t0\tsid\tok\n");
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/foobar"), 1),
            None
        );
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/foo"), 1).as_deref(),
            Some("sid=ok")
        );
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/foo/bar"), 1).as_deref(),
            Some("sid=ok")
        );
    }

    #[test]
    fn netscape_httponly_prefix_is_still_sent() {
        let records = parse_netscape("#HttpOnly_app.example.test\tFALSE\t/\tFALSE\t0\tsid\tok\n");
        assert_eq!(
            header_from_records(&records, &url("http://app.example.test/"), 1).as_deref(),
            Some("sid=ok")
        );
    }

    #[test]
    fn netscape_ignores_comments_and_blank_lines() {
        let source = CookieSource::parse(
            "# Netscape HTTP Cookie File\n\
             \n\
             # This is a comment\n\
             app.example.test\tFALSE\t/\tFALSE\t0\tsid\tok\n",
        )
        .unwrap();
        assert_eq!(
            source
                .header_for_url(&url("http://app.example.test/"))
                .unwrap()
                .as_deref(),
            Some("sid=ok")
        );
    }

    #[test]
    fn empty_netscape_file_is_a_distinct_error() {
        let error = CookieSource::parse("# Netscape HTTP Cookie File\n").unwrap_err();
        assert!(matches!(error, ReadError::EmptyNetscapeCookies));
        assert!(!error.to_string().contains("single line"));
    }

    #[test]
    fn debug_redacts_cookie_values() {
        let header = CookieSource::parse("sid=super-secret-session").unwrap();
        let header_debug = format!("{header:?}");
        assert!(!header_debug.contains("super-secret-session"));
        assert!(header_debug.contains("[redacted]"));

        let netscape = CookieSource::parse(
            "# Netscape HTTP Cookie File\n\
             app.example.test\tFALSE\t/\tFALSE\t0\tsid\tsuper-secret-session\n",
        )
        .unwrap();
        let netscape_debug = format!("{netscape:?}");
        assert!(!netscape_debug.contains("super-secret-session"));
        assert!(netscape_debug.contains("records"));
    }

    #[test]
    fn from_file_reads_a_header_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cookies.txt");
        std::fs::write(&path, "sid=ok\n").unwrap();
        let source = CookieSource::from_file(&path).unwrap();
        assert_eq!(
            source
                .header_for_url(&url("http://app.example.test/"))
                .unwrap()
                .as_deref(),
            Some("sid=ok")
        );
    }
}
