use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReadError {
    #[error("unsupported URL scheme `{0}`; expected http or https")]
    UnsupportedScheme(String),

    #[error("URL must not contain embedded credentials")]
    UrlContainsCredentials,

    #[error("Cookie is only supported for direct HTTP(S) URL sources")]
    CookieRequiresUrl,

    #[error("Cookie header must be a single line without CR or LF")]
    InvalidCookieHeader,

    #[error("Netscape cookie file contains no usable records")]
    EmptyNetscapeCookies,

    #[error("Cookie header contains bytes that are not allowed in an HTTP header")]
    InvalidCookieHeaderBytes,

    #[error("cookie is not forwarded across origins")]
    CookieCrossOriginRedirect { url: String },

    #[error("request for `{url}` exceeded the redirect limit")]
    TooManyRedirects { url: String },

    #[error("input exceeds the {limit} byte limit")]
    InputTooLarge { limit: usize },

    #[error("document contains {found} elements, exceeding the {limit} element limit")]
    TooManyElements { found: usize, limit: usize },

    #[error("document nesting exceeds the {limit} level limit")]
    DocumentTooDeep { limit: usize },

    #[error("input is empty")]
    EmptyInput,

    #[error("input does not appear to be an HTML document")]
    NotHtml,

    #[error("input is not a valid XLSX workbook: {0}")]
    InvalidXlsx(String),

    #[error("Markdown is not a valid Opsail XLSX mirror: {0}")]
    InvalidMarkdownMirror(String),

    #[error("invalid spreadsheet range `{selector}`: {reason}")]
    InvalidSpreadsheetRange { selector: String, reason: String },

    #[error("spreadsheet worksheet `{0}` was not found")]
    WorksheetNotFound(String),

    #[error("spreadsheet OOXML exceeds the {limit} expanded byte limit")]
    SpreadsheetExpandedTooLarge { limit: usize },

    #[error("spreadsheet extraction task failed")]
    SpreadsheetTask,

    #[error("unsupported response content type `{0}`")]
    UnsupportedContentType(String),

    #[error("failed to read `{path}`")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("`{path}` is not a regular file")]
    NotRegularFile { path: PathBuf },

    #[error("failed to resolve `{path}`")]
    ResolveFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to create the HTTP client")]
    BuildClient(#[source] reqwest::Error),

    #[error("request failed for `{url}`")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("request for `{url}` returned HTTP {status}")]
    HttpStatus { url: String, status: u16 },

    #[error("request for `{url}` returned an interactive verification page")]
    VerificationRequired { url: String },

    #[error(transparent)]
    Chrome(#[from] opsail_chrome::ChromeError),

    #[error("failed while reading the response from `{url}`")]
    ReadResponse {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("failed to extract readable content")]
    Extraction(#[source] dom_smoothie::ReadabilityError),

    #[error("no readable content was found")]
    NoContent,
}
