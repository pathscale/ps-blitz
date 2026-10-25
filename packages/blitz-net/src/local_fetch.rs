use blitz_traits::net::{Bytes, Initiator};
use blitz_traits::platform::{FetchError, FetchRequest, FetchResponse, HeaderMap, StatusCode};
use bytes::BytesMut;
use data_url::DataUrl;
use std::io::Read;

const MAX_LOCAL_BODY_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn fetch(request: FetchRequest) -> Result<FetchResponse, FetchError> {
    request
        .initiator
        .check_url(&request.url)
        .map_err(FetchError::Blocked)?;
    let mut headers = HeaderMap::new();
    let body = match request.url.scheme() {
        "file" => {
            let path = request.url.to_file_path().map_err(|()| {
                FetchError::InvalidRequest(format!("not a local path: {}", request.url))
            })?;
            let path = std::fs::canonicalize(path)
                .map_err(|error| FetchError::Network(error.to_string()))?;
            let metadata =
                std::fs::metadata(&path).map_err(|error| FetchError::Network(error.to_string()))?;
            if !metadata.is_file() {
                return Err(FetchError::Blocked("resource-file-scope".into()));
            }
            if metadata.len() > MAX_LOCAL_BODY_BYTES as u64 {
                return Err(FetchError::Blocked("resource-byte-cap".into()));
            }
            let file = std::fs::File::open(&path)
                .map_err(|error| FetchError::Network(error.to_string()))?;
            let metadata = file
                .metadata()
                .map_err(|error| FetchError::Network(error.to_string()))?;
            if !metadata.is_file() {
                return Err(FetchError::Blocked("resource-file-scope".into()));
            }
            if metadata.len() > MAX_LOCAL_BODY_BYTES as u64 {
                return Err(FetchError::Blocked("resource-byte-cap".into()));
            }
            read_file(file, &request.initiator)?
        }
        "data" => {
            let data = DataUrl::process(request.url.as_str())
                .map_err(|error| FetchError::InvalidRequest(format!("{error:?}")))?;
            let mime = data.mime_type().to_string();
            let mut body = BytesMut::new();
            data.decode(|bytes| {
                let next_len = body
                    .len()
                    .checked_add(bytes.len())
                    .ok_or_else(|| FetchError::Blocked("resource-byte-cap".into()))?;
                if next_len > MAX_LOCAL_BODY_BYTES {
                    return Err(FetchError::Blocked("resource-byte-cap".into()));
                }
                request
                    .initiator
                    .charge(bytes.len())
                    .map_err(FetchError::Blocked)?;
                body.extend_from_slice(bytes);
                Ok::<(), FetchError>(())
            })
            .map_err(|error| match error {
                data_url::forgiving_base64::DecodeError::WriteError(error) => error,
                data_url::forgiving_base64::DecodeError::InvalidBase64(error) => {
                    FetchError::InvalidRequest(format!("{error:?}"))
                }
            })?;
            if let Ok(value) = mime.parse() {
                headers.insert("content-type", value);
            }
            body.freeze()
        }
        scheme => return Err(FetchError::UnsupportedScheme(scheme.into())),
    };
    Ok(FetchResponse::new(request.url, StatusCode::OK)
        .headers(headers)
        .body(body))
}

fn read_file(file: std::fs::File, initiator: &Initiator) -> Result<Bytes, FetchError> {
    let mut reader = file.take(MAX_LOCAL_BODY_BYTES as u64 + 1);
    let mut body = BytesMut::new();
    let mut scratch = [0u8; 16 * 1024];
    loop {
        let remaining_body = MAX_LOCAL_BODY_BYTES.saturating_sub(body.len());
        let remaining_budget = initiator.remaining_bytes();
        let capacity = if remaining_body == 0 || remaining_budget == 0 {
            1
        } else {
            remaining_body.min(remaining_budget).min(scratch.len())
        };
        let read = reader
            .read(&mut scratch[..capacity])
            .map_err(|error| FetchError::Network(error.to_string()))?;
        if read == 0 {
            return Ok(body.freeze());
        }
        if read > remaining_body {
            return Err(FetchError::Blocked("resource-byte-cap".into()));
        }
        initiator.charge(read).map_err(FetchError::Blocked)?;
        body.extend_from_slice(&scratch[..read]);
    }
}
