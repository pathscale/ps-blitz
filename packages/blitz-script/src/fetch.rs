//! Synchronous fetching of external script sources (`<script src="...">`)

use std::fmt;
use std::io::Read;
use std::path::PathBuf;

use url::Url;

pub(crate) const MAX_SCRIPT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum FetchError {
    UnsupportedScheme(String),
    Io(std::io::Error),
    InvalidData(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedScheme(scheme) => {
                write!(f, "unsupported URL scheme for script: {scheme}")
            }
            Self::Io(error) => write!(f, "IO error fetching script: {error}"),
            Self::InvalidData(msg) => write!(f, "invalid script data: {msg}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Trait for synchronously fetching external script sources.
///
/// Scripts are fetched synchronously because the HTML spec requires classic
/// scripts to execute in document order, blocking parsing. Implementations
/// should provide [`Self::fetch`]. The default [`Self::fetch_for_document`]
/// applies file-URL authorization and the source-size limit before returning
/// source text. An override of `fetch_for_document` is responsible for
/// preserving those checks (and any additional policy required by its host).
pub trait ScriptFetcher: 'static {
    fn fetch(&self, url: &Url) -> Result<String, FetchError>;

    fn fetch_for_document(
        &self,
        url: &Url,
        document_url: Option<&Url>,
    ) -> Result<String, FetchError> {
        // Keep policy checks at the boundary used by document script loading;
        // custom implementations overriding this method must preserve them.
        authorize_script_url(url, document_url)?;
        let source = self.fetch(url)?;
        if source.len() > MAX_SCRIPT_BYTES {
            return Err(FetchError::InvalidData(format!(
                "script source exceeds {MAX_SCRIPT_BYTES} bytes"
            )));
        }
        Ok(source)
    }
}

/// The default [`ScriptFetcher`]: supports `file:` and `data:` URLs.
pub struct DefaultScriptFetcher;

impl ScriptFetcher for DefaultScriptFetcher {
    fn fetch(&self, url: &Url) -> Result<String, FetchError> {
        match url.scheme() {
            "file" => Err(FetchError::InvalidData(
                "file script fetch requires document authority".to_string(),
            )),
            "data" => decode_data_url(url),
            scheme => Err(FetchError::UnsupportedScheme(scheme.to_string())),
        }
    }

    fn fetch_for_document(
        &self,
        url: &Url,
        document_url: Option<&Url>,
    ) -> Result<String, FetchError> {
        if url.scheme() == "file" {
            let path = authorized_file_path(url, document_url)?;
            read_script_file(path)
        } else {
            self.fetch(url)
        }
    }
}

pub(crate) fn authorize_script_url(
    url: &Url,
    document_url: Option<&Url>,
) -> Result<(), FetchError> {
    if url.scheme() != "file" {
        return Ok(());
    }
    authorized_file_path(url, document_url).map(|_| ())
}

fn file_url_path(url: &Url) -> Result<PathBuf, FetchError> {
    if url.host_str().is_some_and(|host| host != "localhost") {
        return Err(FetchError::InvalidData(format!(
            "unsupported authority in file URL: {url}"
        )));
    }
    url.to_file_path()
        .map_err(|_| FetchError::InvalidData(format!("invalid file URL: {url}")))
}

fn require_regular_file(path: &std::path::Path, role: &str) -> Result<(), FetchError> {
    let metadata = std::fs::metadata(path).map_err(FetchError::Io)?;
    if !metadata.is_file() {
        return Err(FetchError::InvalidData(format!(
            "{role} is not a regular file"
        )));
    }
    Ok(())
}

fn authorized_file_path(url: &Url, document_url: Option<&Url>) -> Result<PathBuf, FetchError> {
    let document_url = document_url.ok_or_else(|| {
        FetchError::InvalidData("file script access requires a file document URL".to_string())
    })?;
    if document_url.scheme() != "file" {
        return Err(FetchError::InvalidData(
            "file script access requires a file document URL".to_string(),
        ));
    }

    let document_path = file_url_path(document_url)?;
    let requested_path = file_url_path(url)?;
    let canonical_document = std::fs::canonicalize(document_path).map_err(FetchError::Io)?;
    require_regular_file(&canonical_document, "document")?;
    let root = canonical_document.parent().ok_or_else(|| {
        FetchError::InvalidData("document file has no parent directory".to_string())
    })?;

    let canonical_requested = std::fs::canonicalize(requested_path).map_err(FetchError::Io)?;
    if !canonical_requested.starts_with(root) {
        return Err(FetchError::InvalidData(format!(
            "script file is outside the document directory: {url}"
        )));
    }
    require_regular_file(&canonical_requested, "script")?;
    Ok(canonical_requested)
}

fn read_script_file(path: PathBuf) -> Result<String, FetchError> {
    let before_open = std::fs::metadata(&path).map_err(FetchError::Io)?;
    if !before_open.is_file() {
        return Err(FetchError::InvalidData(
            "script is not a regular file".to_string(),
        ));
    }
    if before_open.len() > MAX_SCRIPT_BYTES as u64 {
        return Err(FetchError::InvalidData(format!(
            "script source exceeds {MAX_SCRIPT_BYTES} bytes"
        )));
    }

    let file = std::fs::File::open(path).map_err(FetchError::Io)?;
    let opened_metadata = file.metadata().map_err(FetchError::Io)?;
    if !opened_metadata.is_file() {
        return Err(FetchError::InvalidData(
            "script is not a regular file".to_string(),
        ));
    }
    if opened_metadata.len() > MAX_SCRIPT_BYTES as u64 {
        return Err(FetchError::InvalidData(format!(
            "script source exceeds {MAX_SCRIPT_BYTES} bytes"
        )));
    }

    let mut bytes = Vec::new();
    file.take(MAX_SCRIPT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(FetchError::Io)?;
    if bytes.len() > MAX_SCRIPT_BYTES {
        return Err(FetchError::InvalidData(format!(
            "script source exceeds {MAX_SCRIPT_BYTES} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|error| FetchError::InvalidData(format!("{error:?}")))
}

fn decode_data_url(url: &Url) -> Result<String, FetchError> {
    let data_url = data_url::DataUrl::process(url.as_str())
        .map_err(|error| FetchError::InvalidData(format!("{error:?}")))?;
    let mut bytes = Vec::new();
    data_url
        .decode(|chunk| {
            if chunk.len() > MAX_SCRIPT_BYTES.saturating_sub(bytes.len()) {
                return Err(FetchError::InvalidData(format!(
                    "script source exceeds {MAX_SCRIPT_BYTES} bytes"
                )));
            }
            bytes.extend_from_slice(chunk);
            Ok::<(), FetchError>(())
        })
        .map_err(|error| match error {
            data_url::forgiving_base64::DecodeError::WriteError(error) => error,
            data_url::forgiving_base64::DecodeError::InvalidBase64(error) => {
                FetchError::InvalidData(format!("{error:?}"))
            }
        })?;
    String::from_utf8(bytes).map_err(|error| FetchError::InvalidData(format!("{error:?}")))
}
