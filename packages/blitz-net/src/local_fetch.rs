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
            // Bound the decoding allocation before accepting attacker input.
            // Three URL bytes can encode one decoded byte. The fixed allowance
            // covers the media type; decoded bytes are charged below.
            if request.url.as_str().len() > request.initiator.remaining_bytes()
                .saturating_mul(3).saturating_add(4096)
            {
                return Err(FetchError::Blocked("resource-byte-cap".into()));
            }
            let data = DataUrl::process(request.url.as_str())
                .map_err(|error| FetchError::InvalidRequest(format!("{error:?}")))?;
            let mime = data.mime_type().to_string();
            let (body, _) = data.decode_to_vec()
                .map_err(|error| FetchError::InvalidRequest(format!("{error:?}")))?;
            request.initiator.charge(body.len()).map_err(FetchError::Blocked)?;
            if let Ok(value) = mime.parse() { headers.insert("content-type", value); }
            Bytes::from(body)
        }
        scheme => return Err(FetchError::UnsupportedScheme(scheme.into())),
    };
    Ok(FetchResponse::new(request.url, StatusCode::OK).headers(headers).body(body))
}

fn read_file(file: &mut std::fs::File, initiator: &Initiator) -> Result<Bytes, FetchError> {
    let mut body = BytesMut::new();
    loop {
        let capacity = initiator.remaining_bytes().min(16 * 1024).max(1);
        let start = body.len();
        body.resize(start + capacity, 0);
        let read = file.read(&mut body[start..])
            .map_err(|error| FetchError::Network(error.to_string()))?;
        body.truncate(start + read);
        initiator.charge(read).map_err(FetchError::Blocked)?;
        if read == 0 { return Ok(body.freeze()); }
    }
}
