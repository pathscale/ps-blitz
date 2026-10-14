use blitz_traits::net::{Bytes, Initiator};
use blitz_traits::platform::{FetchError, FetchRequest, FetchResponse, HeaderMap, StatusCode};
use bytes::BytesMut;
use data_url::DataUrl;
use std::io::Read;

pub(crate) fn fetch(request: FetchRequest) -> Result<FetchResponse, FetchError> {
    request.initiator.check_url(&request.url).map_err(FetchError::Blocked)?;
    let mut headers = HeaderMap::new();
    let body = match request.url.scheme() {
        "file" => {
            let path = request.url.to_file_path().map_err(|()| {
                FetchError::InvalidRequest(format!("not a local path: {}", request.url))
            })?;
            let mut file = std::fs::File::open(path)
                .map_err(|error| FetchError::Network(error.to_string()))?;
            read_file(&mut file, &request.initiator)?
        }
        "data" => {
            let data = DataUrl::process(request.url.as_str())
                .map_err(|error| FetchError::InvalidRequest(format!("{error:?}")))?;
            let mime = data.mime_type().to_string();
            let mut body = BytesMut::new();
            data.decode(|bytes| {
                request.initiator.charge(bytes.len()).map_err(FetchError::Blocked)?;
                body.extend_from_slice(bytes);
                Ok::<(), FetchError>(())
            }).map_err(|error| match error {
                data_url::forgiving_base64::DecodeError::WriteError(error) => error,
                data_url::forgiving_base64::DecodeError::InvalidBase64(error) => {
                    FetchError::InvalidRequest(format!("{error:?}"))
                }
            })?;
            if let Ok(value) = mime.parse() { headers.insert("content-type", value); }
            body.freeze()
        }
        scheme => return Err(FetchError::UnsupportedScheme(scheme.into())),
    };
    Ok(FetchResponse::new(request.url, StatusCode::OK).headers(headers).body(body))
}

fn read_file(file: &mut std::fs::File, initiator: &Initiator) -> Result<Bytes, FetchError> {
    let mut body = BytesMut::new();
    loop {
        let capacity = initiator.remaining_bytes().clamp(1, 16 * 1024);
        let start = body.len();
        body.resize(start + capacity, 0);
        let read = file.read(&mut body[start..])
            .map_err(|error| FetchError::Network(error.to_string()))?;
        body.truncate(start + read);
        initiator.charge(read).map_err(FetchError::Blocked)?;
        if read == 0 { return Ok(body.freeze()); }
    }
}
