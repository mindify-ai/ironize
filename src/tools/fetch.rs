//! Fetching web pages.

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{
    parse_args, str_arg, truncate_middle, Preview, Tool, ToolContext, ToolKind, ToolOutput,
};

const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_MAX_CHARS: usize = 50_000;

pub struct WebFetch;

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
    #[serde(default)]
    max_chars: Option<usize>,
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }

    fn label(&self) -> &'static str {
        "Fetch"
    }

    fn description(&self) -> &'static str {
        "Fetch an http(s) URL and return its content as text. HTML is converted to \
         readable text; JSON and plain text are returned as-is."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Absolute http or https URL." },
                "max_chars": { "type": "integer", "minimum": 100, "description": "Truncate the text to this many characters (default 50000)." }
            },
            "required": ["url"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    fn subject(&self, args: &Value, _ctx: &ToolContext) -> String {
        str_arg(args, "url").to_string()
    }

    async fn preview(&self, args: &Value, _ctx: &ToolContext) -> Preview {
        Preview::Text(format!("GET {}", str_arg(args, "url")))
    }

    async fn run(&self, args: Value, ctx: &ToolContext, cancel: CancellationToken) -> ToolOutput {
        let args: FetchArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let url = match url::Url::parse(&args.url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => u,
            Ok(_) => return ToolOutput::error("only http and https URLs are supported"),
            Err(e) => return ToolOutput::error(format!("invalid URL: {e}")),
        };
        tokio::select! {
            out = fetch(ctx, url, args.max_chars.unwrap_or(DEFAULT_MAX_CHARS)) => out,
            _ = cancel.cancelled() => ToolOutput::error("fetch interrupted by the user"),
        }
    }
}

async fn fetch(ctx: &ToolContext, url: url::Url, max_chars: usize) -> ToolOutput {
    let response = match ctx.http.get(url.clone()).send().await {
        Ok(r) => r,
        Err(e) => return ToolOutput::error(format!("request failed: {e}")),
    };
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                body.extend_from_slice(&bytes);
                if body.len() > MAX_BODY_BYTES {
                    body.truncate(MAX_BODY_BYTES);
                    break;
                }
            }
            Err(e) => return ToolOutput::error(format!("reading the response failed: {e}")),
        }
    }

    let is_html = content_type.contains("html");
    let is_text = is_html
        || content_type.is_empty()
        || content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript");
    if !is_text {
        return ToolOutput::error(format!(
            "unsupported content type {content_type:?} ({} bytes)",
            body.len()
        ));
    }
    let text = if is_html {
        html2text::from_read(body.as_slice(), 100)
    } else {
        String::from_utf8_lossy(&body).into_owned()
    };
    let text = truncate_middle(text.trim(), max_chars);
    let brief = format!("{} · {} chars", status, text.chars().count());
    if !status.is_success() {
        let mut out = ToolOutput::error(format!("HTTP {status}\n\n{text}"));
        out.brief = format!("HTTP {status}");
        return out;
    }
    ToolOutput::ok(format!("{url}\n\n{text}"), brief)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{ctx, run};
    use super::*;
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn converts_html_and_rejects_binary() {
        let server = MockServer::start().await;
        Mock::given(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<html><body><h1>Title</h1><p>Hello <b>world</b></p><script>x()</script></body></html>",
                "text/html",
            ))
            .mount(&server)
            .await;
        Mock::given(path("/img"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0u8; 10], "image/png"))
            .mount(&server)
            .await;
        Mock::given(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let (_dir, mut ctx) = ctx();
        ctx.http = reqwest::Client::builder().no_proxy().build().unwrap();
        let out = run(
            &WebFetch,
            &ctx,
            json!({ "url": format!("{}/page", server.uri()) }),
        )
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Title"));
        assert!(out.content.contains("Hello"));

        assert!(
            run(
                &WebFetch,
                &ctx,
                json!({ "url": format!("{}/img", server.uri()) })
            )
            .await
            .is_error
        );
        assert!(
            run(
                &WebFetch,
                &ctx,
                json!({ "url": format!("{}/missing", server.uri()) })
            )
            .await
            .is_error
        );
        assert!(
            run(&WebFetch, &ctx, json!({ "url": "file:///etc/passwd" }))
                .await
                .is_error
        );
    }
}
