use content_disposition;
use reqwest::{
    blocking::Client,
    header::{self, HeaderMap},
    StatusCode, Url,
};
use sanitize_filename;

#[derive(Debug, Clone)]
pub struct UrlInfo {
    pub file_size: u64,
    pub file_name: String,
    pub supports_range: bool,
    pub can_fast_download: bool,
    pub final_url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

fn get_file_size(headers: &HeaderMap, status: &StatusCode) -> u64 {
    if *status == StatusCode::PARTIAL_CONTENT {
        headers
            .get(header::CONTENT_RANGE)
            .and_then(|hv| hv.to_str().ok())
            .and_then(|s| s.rsplit('/').next())
            .and_then(|total| total.parse().ok())
            .unwrap_or_default()
    } else {
        headers
            .get(header::CONTENT_LENGTH)
            .and_then(|hv| hv.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or_default()
    }
}

fn get_header_str(headers: &HeaderMap, header_name: &header::HeaderName) -> Option<String> {
    headers
        .get(header_name)
        .and_then(|hv| hv.to_str().ok())
        .map(String::from)
}

fn get_filename(headers: &HeaderMap, final_url: &Url) -> String {
    let from_disposition = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|hv| hv.to_str().ok())
        .and_then(|s| content_disposition::parse_content_disposition(s).filename_full())
        .filter(|s| !s.trim().is_empty());

    let from_url = final_url
        .path_segments()
        .and_then(|segments| segments.last())
        .and_then(|s| urlencoding::decode(s).ok())
        .filter(|s| !s.trim().is_empty())
        .map(|s| (&s).to_string());

    let raw_name = from_disposition
        .or(from_url)
        .unwrap_or_else(|| final_url.to_string());

    sanitize_filename::sanitize_with_options(
        &raw_name,
        sanitize_filename::Options {
            windows: true,
            truncate: true,
            replacement: "_",
        },
    )
}

pub fn get_url_info(url: &str, client: &Client) -> Result<UrlInfo, reqwest::Error> {
    let resp = client.head(url).send()?;

    let resp = match resp.error_for_status() {
        Ok(resp) => resp,
        Err(_) => return get_url_info_fallback(url, client),
    };

    let status = resp.status();
    let final_url = resp.url();
    let final_url_str = final_url.to_string();

    let resp_headers = resp.headers();
    let file_size = get_file_size(resp_headers, &status);

    let supports_range = match resp.headers().get(header::ACCEPT_RANGES) {
        Some(accept_ranges) => accept_ranges
            .to_str()
            .ok()
            .map(|v| v.split(' '))
            .and_then(|supports| supports.into_iter().find(|&ty| ty == "bytes"))
            .is_some(),
        None => return get_url_info_fallback(url, client),
    };

    Ok(UrlInfo {
        final_url: final_url_str,
        file_name: get_filename(resp_headers, &final_url),
        file_size,
        supports_range,
        can_fast_download: file_size > 0 && supports_range,
        etag: get_header_str(resp_headers, &header::ETAG),
        last_modified: get_header_str(resp_headers, &header::LAST_MODIFIED),
    })
}

fn get_url_info_fallback(url: &str, client: &Client) -> Result<UrlInfo, reqwest::Error> {
    let resp = client
        .get(url)
        .header(header::RANGE, "bytes=0-")
        .send()?
        .error_for_status()?;
    let status = resp.status();
    let final_url = resp.url();
    let final_url_str = final_url.to_string();

    let resp_headers = resp.headers();
    let file_size = get_file_size(resp_headers, &status);
    let supports_range = status == StatusCode::PARTIAL_CONTENT;
    Ok(UrlInfo {
        final_url: final_url_str,
        file_name: get_filename(resp_headers, &final_url),
        file_size,
        supports_range,
        can_fast_download: file_size > 0 && supports_range,
        etag: get_header_str(resp_headers, &header::ETAG),
        last_modified: get_header_str(resp_headers, &header::LAST_MODIFIED),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redirect_and_content_range() {
        let mut server = mockito::Server::new();

        let mock_redirect = server
            .mock("GET", "/redirect")
            .with_status(301)
            .with_header("Location", "/real-file.txt")
            .create();

        let mock_file = server
            .mock("GET", "/real-file.txt")
            .with_status(206)
            .with_header("Content-Range", "bytes 0-1023/2048")
            .with_body(vec![0; 1024])
            .create();

        let client = Client::new();
        let url_info = get_url_info(&format!("{}/redirect", server.url()), &client)
            .expect("Request should succeed");

        assert_eq!(url_info.file_size, 2048);
        assert_eq!(url_info.file_name, "real-file.txt");
        assert_eq!(
            url_info.final_url,
            format!("{}/real-file.txt", server.url())
        );
        assert!(url_info.supports_range);

        mock_redirect.assert();
        mock_file.assert();
    }

    #[test]
    fn test_content_range_priority() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/file")
            .with_status(206)
            .with_header("Content-Range", "bytes 0-1023/2048")
            .create();

        let client = Client::new();
        let url_info = get_url_info(&format!("{}/file", server.url()), &client)
            .expect("Request should succeed");

        assert_eq!(url_info.file_size, 2048);
        mock.assert();
    }

    #[test]
    fn test_filename_sources() {
        let mut server = mockito::Server::new();

        // Test Content-Disposition source
        let mock1 = server
            .mock("GET", "/test1")
            .with_header("Content-Disposition", "attachment; filename=\"test.txt\"")
            .create();
        let url_info = get_url_info(&format!("{}/test1", server.url()), &Client::new()).unwrap();
        assert_eq!(url_info.file_name, "test.txt");
        mock1.assert();

        // Test URL path source
        let mock2 = server.mock("GET", "/test2/file.pdf").create();
        let url_info =
            get_url_info(&format!("{}/test2/file.pdf", server.url()), &Client::new()).unwrap();
        assert_eq!(url_info.file_name, "file.pdf");
        mock2.assert();

        // Test sanitization
        let mock3 = server
            .mock("GET", "/test3")
            .with_header(
                "Content-Disposition",
                "attachment; filename*=UTF-8''%E6%82%AA%E3%81%84%3C%3E%E3%83%95%E3%82%A1%E3%82%A4%E3%83%AB%3F%E5%90%8D.txt"
            )
            .create();
        let url_info = get_url_info(&format!("{}/test3", server.url()), &Client::new()).unwrap();
        assert_eq!(url_info.file_name, "悪い__ファイル_名.txt");
        mock3.assert();
    }

    #[test]
    fn test_error_handling() {
        let mut server = mockito::Server::new();
        let mock1 = server.mock("GET", "/404").with_status(404).create();

        let client = Client::new();

        match get_url_info(&format!("{}/404", server.url()), &client) {
            Ok(info) => assert!(false, "404 status code should not success: {:?}", info),
            Err(err) => {
                assert!(err.is_status(), "should be error about status code");
                assert_eq!(err.status(), Some(StatusCode::NOT_FOUND));
            }
        }

        mock1.assert();
    }
}
