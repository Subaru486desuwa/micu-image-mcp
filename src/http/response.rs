use std::borrow::Cow;

use serde::Deserialize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImagePayload<'a> {
    Base64(Cow<'a, str>),
    Url(Cow<'a, str>),
}

#[derive(Deserialize)]
struct BorrowedImagesResponse<'a> {
    #[serde(default, borrow)]
    data: Vec<BorrowedImageItem<'a>>,
}

#[derive(Deserialize)]
struct BorrowedImageItem<'a> {
    #[serde(default)]
    b64_json: Option<&'a str>,
    #[serde(default)]
    url: Option<&'a str>,
}

#[derive(Deserialize)]
struct OwnedImagesResponse {
    #[serde(default)]
    data: Vec<OwnedImageItem>,
}

#[derive(Deserialize)]
struct OwnedImageItem {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct ErrorEnvelope<'a> {
    #[serde(default, borrow)]
    error: Option<ErrorObject<'a>>,
    #[serde(default, borrow)]
    message: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct ErrorObject<'a> {
    #[serde(default, borrow)]
    message: Option<Cow<'a, str>>,
}

pub fn extract_first_payload(body: &[u8]) -> Result<ImagePayload<'_>, String> {
    if let Ok(response) = serde_json::from_slice::<BorrowedImagesResponse<'_>>(body) {
        let Some(item) = response.data.into_iter().next() else {
            return Err("响应中未识别到图片".into());
        };
        if let Some(encoded) = item.b64_json.filter(|value| !value.is_empty()) {
            return Ok(ImagePayload::Base64(Cow::Borrowed(encoded)));
        }
        if let Some(url) = item.url.filter(|value| !value.is_empty()) {
            if let Some(encoded) = data_url_payload(url) {
                return Ok(ImagePayload::Base64(Cow::Borrowed(encoded)));
            }
            return Ok(ImagePayload::Url(Cow::Borrowed(url)));
        }
        return Err("响应中未识别到图片".into());
    }
    let response: OwnedImagesResponse =
        serde_json::from_slice(body).map_err(|_| "响应中未识别到图片".to_owned())?;
    let Some(item) = response.data.into_iter().next() else {
        return Err("响应中未识别到图片".into());
    };
    if let Some(encoded) = item.b64_json.filter(|value| !value.is_empty()) {
        return Ok(ImagePayload::Base64(Cow::Owned(encoded)));
    }
    if let Some(url) = item.url.filter(|value| !value.is_empty()) {
        if let Some(encoded) = owned_data_url_payload(url.clone()) {
            return Ok(ImagePayload::Base64(Cow::Owned(encoded)));
        }
        return Ok(ImagePayload::Url(Cow::Owned(url)));
    }
    Err("响应中未识别到图片".into())
}

pub fn error_detail(body: &[u8], secrets: &[&str]) -> String {
    let raw = match serde_json::from_slice::<ErrorEnvelope<'_>>(body) {
        Ok(envelope) => envelope
            .error
            .and_then(|error| error.message)
            .or(envelope.message)
            .unwrap_or_else(|| String::from_utf8_lossy(body)),
        Err(_) => String::from_utf8_lossy(body),
    };
    sanitize_sensitive(&raw, secrets)
        .chars()
        .take(400)
        .collect()
}

pub fn sanitize_sensitive(text: &str, secrets: &[&str]) -> String {
    let mut sanitized = text.to_owned();
    for secret in secrets.iter().copied().filter(|secret| !secret.is_empty()) {
        sanitized = sanitized.replace(secret, "[REDACTED]");
    }
    sanitized = redact_bearer_tokens(&sanitized);
    redact_long_base64_runs(&sanitized)
}

fn data_url_payload(url: &str) -> Option<&str> {
    let remainder = url.strip_prefix("data:image/")?;
    let marker = ";base64,";
    let marker_index = remainder.find(marker)?;
    if marker_index == 0 {
        return None;
    }
    let payload = remainder[marker_index + marker.len()..].trim();
    if payload.is_empty()
        || !payload.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '+' | '/' | '=' | ' ' | '\t' | '\r' | '\n')
        })
    {
        return None;
    }
    Some(payload)
}

fn owned_data_url_payload(mut url: String) -> Option<String> {
    let remainder = url.strip_prefix("data:image/")?;
    let marker = ";base64,";
    let marker_index = remainder.find(marker)?;
    if marker_index == 0 {
        return None;
    }
    let payload_start = "data:image/".len() + marker_index + marker.len();
    let payload = url.split_off(payload_start);
    let trimmed = payload.trim();
    if trimmed.is_empty()
        || !trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '+' | '/' | '=' | ' ' | '\t' | '\r' | '\n')
        })
    {
        return None;
    }
    if trimmed.len() == payload.len() {
        Some(payload)
    } else {
        Some(trimmed.to_owned())
    }
}

fn redact_bearer_tokens(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("bearer ") {
        let start = cursor + relative;
        let token_start = start + "bearer ".len();
        output.push_str(&text[cursor..token_start]);
        let token_length = text[token_start..]
            .chars()
            .take_while(|character| {
                !character.is_whitespace()
                    && !matches!(character, ';' | ',' | '"' | '\'' | ')' | ']')
            })
            .map(char::len_utf8)
            .sum::<usize>();
        if token_length == 0 {
            cursor = token_start;
        } else {
            output.push_str("[REDACTED]");
            cursor = token_start + token_length;
        }
    }
    output.push_str(&text[cursor..]);
    output
}

fn redact_long_base64_runs(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |output: &mut String, run: &mut String| {
        if run.len() >= 64 {
            output.push_str("[REDACTED_BASE64]");
        } else {
            output.push_str(run);
        }
        run.clear();
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '=') {
            run.push(character);
        } else {
            flush(&mut output, &mut run);
            output.push(character);
        }
    }
    flush(&mut output, &mut run);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_borrowed_b64_url_and_data_url_without_chat_fallback() {
        assert!(matches!(
            extract_first_payload(br#"{"data":[{"b64_json":"AAAA"}]}"#),
            Ok(ImagePayload::Base64(Cow::Borrowed("AAAA")))
        ));
        assert_eq!(
            extract_first_payload(br#"{"data":[{"b64_json":"AAAA"}]}"#),
            Ok(ImagePayload::Base64(Cow::Borrowed("AAAA")))
        );
        assert_eq!(
            extract_first_payload(br#"{"data":[{"url":"https://example.test/x.png"}]}"#),
            Ok(ImagePayload::Url(Cow::Borrowed(
                "https://example.test/x.png"
            )))
        );
        assert_eq!(
            extract_first_payload(br#"{"data":[{"url":"data:image/png;base64,AAAABBBB"}]}"#),
            Ok(ImagePayload::Base64(Cow::Borrowed("AAAABBBB")))
        );
        assert_eq!(
            extract_first_payload(br#"{"data":[{"url":"https:\/\/example.test\/x.png"}]}"#),
            Ok(ImagePayload::Url(Cow::Owned(
                "https://example.test/x.png".into()
            )))
        );
        assert!(
            extract_first_payload(
                br#"{"choices":[{"message":{"content":"![](https://x/y.png)"}}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_json_and_missing_payload_have_stable_public_errors() {
        assert!(
            extract_first_payload(b"not json").is_err_and(|error| error == "响应中未识别到图片")
        );
        assert!(
            extract_first_payload(br#"{"data":[]}"#)
                .is_err_and(|error| error == "响应中未识别到图片")
        );
    }

    #[test]
    fn error_detail_never_leaks_key_authorization_or_large_base64() {
        let secret = "sk-very-secret-value";
        let body = format!(
            "{{\"error\":{{\"message\":\"Authorization: Bearer {secret}; image=iVBORw0KGgo{}\"}}}}",
            "A".repeat(300)
        );
        let detail = error_detail(body.as_bytes(), &[secret]);
        assert!(!detail.contains(secret));
        assert!(!detail.contains("iVBORw0KGgo"));
        assert!(detail.contains("[REDACTED]"));
        assert!(detail.chars().count() <= 400);
    }
}
