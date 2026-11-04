use anyhow::Context;
use url::Url;

/// Sanitizes a URL for logging by removing query parameters and fragments
/// to prevent sensitive data (tokens, session IDs, etc.) from appearing in logs.
///
/// # Arguments
/// * `url` - The URL to sanitize
///
/// # Returns
/// A sanitized URL containing only protocol, host, and path
///
/// # Examples
/// ```
/// let sanitized = sanitize_url_for_logging("https://api.example.com/data?token=secret&id=123#section");
/// assert_eq!(sanitized, "https://api.example.com/data");
/// ```
pub fn sanitize_url_for_logging(url: &str) -> String {
    match Url::parse(url) {
        Ok(parsed) => {
            let mut sanitized = String::new();

            // Add scheme
            sanitized.push_str(parsed.scheme());
            sanitized.push_str("://");

            // Add host
            if let Some(host) = parsed.host_str() {
                sanitized.push_str(host);

                // Add port if present and not default
                if let Some(port) = parsed.port() {
                    sanitized.push(':');
                    sanitized.push_str(&port.to_string());
                }
            }

            // Add path
            sanitized.push_str(parsed.path());

            sanitized
        }
        Err(_) => {
            // If URL parsing fails, try to extract basic components manually
            if let Some(query_start) = url.find('?') {
                url[..query_start].to_string()
            } else if let Some(fragment_start) = url.find('#') {
                url[..fragment_start].to_string()
            } else {
                url.to_string()
            }
        }
    }
}

/// `text` with every `http(s)://` URL in it cut to protocol, host and path
/// ([`sanitize_url_for_logging`]). A URL ends at whitespace or at one of
/// `)`, `,`, `"`, `'`, `>`, `]`, `;`.
pub fn sanitize_urls_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let start = match (rest.find("http://"), rest.find("https://")) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => {
                out.push_str(rest);
                return out;
            }
        };
        out.push_str(&rest[..start]);
        let url_and_rest = &rest[start..];
        let end = url_and_rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, ')' | ',' | '"' | '\'' | '>' | ']' | ';')
            })
            .unwrap_or(url_and_rest.len());
        out.push_str(&sanitize_url_for_logging(&url_and_rest[..end]));
        rest = &url_and_rest[end..];
    }
}

/// Creates a standardized error context for reqwest HTTP requests
///
/// # Arguments
/// * `function_name` - Name of the function where the error occurred
/// * `url` - The URL that was being requested (will be sanitized)
/// * `error_type` - Type of error (e.g., "HTTP_REQUEST_ERR", "HTTP_STATUS_ERR")
///
/// # Returns
/// A formatted error context string
pub fn create_reqwest_context(function_name: &str, url: &str, error_type: &str) -> String {
    let sanitized_url = sanitize_url_for_logging(url);
    format!("{} in {}: {}", error_type, function_name, sanitized_url)
}

/// Extension trait for adding HTTP context to anyhow errors
pub trait HttpContextExt<T> {
    /// Adds HTTP request context to an anyhow Result
    fn with_http_context(self, function_name: &str, url: &str) -> anyhow::Result<T>;

    /// Adds HTTP status context to an anyhow Result  
    fn with_http_status_context(self, function_name: &str, url: &str) -> anyhow::Result<T>;

    /// Adds generic HTTP error context to an anyhow Result
    fn with_http_error_context(
        self,
        function_name: &str,
        url: &str,
        error_type: &str,
    ) -> anyhow::Result<T>;
}

impl<T> HttpContextExt<T> for Result<T, reqwest::Error> {
    fn with_http_context(self, function_name: &str, url: &str) -> anyhow::Result<T> {
        self.context(create_reqwest_context(
            function_name,
            url,
            "HTTP_REQUEST_ERR",
        ))
    }

    fn with_http_status_context(self, function_name: &str, url: &str) -> anyhow::Result<T> {
        self.context(create_reqwest_context(
            function_name,
            url,
            "HTTP_STATUS_ERR",
        ))
    }

    fn with_http_error_context(
        self,
        function_name: &str,
        url: &str,
        error_type: &str,
    ) -> anyhow::Result<T> {
        self.context(create_reqwest_context(function_name, url, error_type))
    }
}

impl<T> HttpContextExt<T> for Result<T, reqwest_middleware::Error> {
    fn with_http_context(self, function_name: &str, url: &str) -> anyhow::Result<T> {
        self.map_err(|e| anyhow::anyhow!(e))
            .context(create_reqwest_context(
                function_name,
                url,
                "HTTP_REQUEST_ERR",
            ))
    }

    fn with_http_status_context(self, function_name: &str, url: &str) -> anyhow::Result<T> {
        self.map_err(|e| anyhow::anyhow!(e))
            .context(create_reqwest_context(
                function_name,
                url,
                "HTTP_STATUS_ERR",
            ))
    }

    fn with_http_error_context(
        self,
        function_name: &str,
        url: &str,
        error_type: &str,
    ) -> anyhow::Result<T> {
        self.map_err(|e| anyhow::anyhow!(e))
            .context(create_reqwest_context(function_name, url, error_type))
    }
}

impl<T> HttpContextExt<T> for anyhow::Result<T> {
    fn with_http_context(self, function_name: &str, url: &str) -> anyhow::Result<T> {
        self.context(create_reqwest_context(
            function_name,
            url,
            "HTTP_REQUEST_ERR",
        ))
    }

    fn with_http_status_context(self, function_name: &str, url: &str) -> anyhow::Result<T> {
        self.context(create_reqwest_context(
            function_name,
            url,
            "HTTP_STATUS_ERR",
        ))
    }

    fn with_http_error_context(
        self,
        function_name: &str,
        url: &str,
        error_type: &str,
    ) -> anyhow::Result<T> {
        self.context(create_reqwest_context(function_name, url, error_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_url_for_logging() {
        // Test with query parameters
        assert_eq!(
            sanitize_url_for_logging("https://api.example.com/data?token=secret&id=123"),
            "https://api.example.com/data"
        );

        // Test with fragment
        assert_eq!(
            sanitize_url_for_logging("https://example.com/page#section"),
            "https://example.com/page"
        );

        // Test with both query and fragment
        assert_eq!(
            sanitize_url_for_logging("https://api.example.com/data?key=value#top"),
            "https://api.example.com/data"
        );

        // Test with port
        assert_eq!(
            sanitize_url_for_logging("https://api.example.com:8080/data?token=secret"),
            "https://api.example.com:8080/data"
        );

        // Test clean URL (no changes needed)
        assert_eq!(
            sanitize_url_for_logging("https://api.example.com/data"),
            "https://api.example.com/data"
        );

        // Test with path containing sensitive info (should be preserved as it's part of the path)
        assert_eq!(
            sanitize_url_for_logging("https://api.example.com/users/123/profile?token=secret"),
            "https://api.example.com/users/123/profile"
        );
    }

    #[test]
    fn urls_in_text_lose_query_and_fragment() {
        assert_eq!(
            sanitize_urls_in_text(
                "error sending request for url (https://a.example/r/latest?cdk=SECRET&x=1): reset; \
                 retry http://b.example:8080/p#frag, done"
            ),
            "error sending request for url (https://a.example/r/latest): reset; \
             retry http://b.example:8080/p, done"
        );
        assert_eq!(sanitize_urls_in_text("no url here"), "no url here");
    }

    #[test]
    fn test_create_reqwest_context() {
        let context = create_reqwest_context(
            "get_user_data",
            "https://api.example.com/users?token=secret",
            "HTTP_REQUEST_ERR",
        );
        assert_eq!(
            context,
            "HTTP_REQUEST_ERR in get_user_data: https://api.example.com/users"
        );
    }
}
