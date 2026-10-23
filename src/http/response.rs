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

/// Only an unqualified rejection of the requested model authorizes an alias POST.
/// Structured model codes cannot override contradictory messages or parameter fields.
pub fn model_rejected(status: u16, body: &[u8], model: &str) -> bool {
    if !matches!(status, 400 | 404 | 422 | 503) {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let Some(error) = value.get("error").and_then(serde_json::Value::as_object) else {
        return false;
    };
    // Missing/null metadata is allowed; malformed metadata is not absence.
    let field = |name| match error.get(name) {
        None | Some(serde_json::Value::Null) => Some(""),
        Some(serde_json::Value::String(value)) => Some(value.as_str()),
        _ => None,
    };
    let (Some(code), Some(kind), Some(param), Some(message)) = (
        field("code"),
        field("type"),
        field("param"),
        field("message"),
    ) else {
        return false;
    };
    if !param.is_empty() && param != "model" {
        return false;
    }
    let code = code.trim().to_ascii_lowercase();
    let kind = kind.trim().to_ascii_lowercase();
    let explicit_model_code = model_failure_code(&code) || model_failure_code(&kind);
    if (!code.is_empty() && !model_failure_code(&code) && code != "no_available_channel")
        || (!kind.is_empty()
            && !model_failure_code(&kind)
            && !matches!(
                kind.as_str(),
                "new_api_error" | "invalid_request_error" | "no_available_channel"
            ))
    {
        return false;
    }
    let message = message.trim().to_ascii_lowercase();
    let message = rejection_sentence(&message);
    if message.is_empty() {
        return explicit_model_code;
    }
    // A generic model-only message can rely on a structured model code, but a
    // message naming another model, permissions or an input cannot bypass parsing.
    if explicit_model_code
        && [
            "model not found",
            "model is not found",
            "model does not exist",
            "unsupported model",
            "unknown model",
            "invalid model",
            "model not supported",
            "model is not supported",
            "model unavailable",
            "model is unavailable",
        ]
        .contains(&message)
    {
        return true;
    }
    message_rejects_requested_model(message, model)
}

fn model_failure_code(code: &str) -> bool {
    matches!(
        code,
        "model_not_found"
            | "invalid_model"
            | "unsupported_model"
            | "model_not_supported"
            | "model_not_available"
            | "model_unavailable"
    )
}

fn rejection_sentence(message: &str) -> &str {
    fn trim(text: &str) -> &str {
        text.trim().trim_end_matches(['.', '!', '。', '！'])
    }
    let message = trim(message);
    // New API appends opaque request IDs. Do not discard arbitrary parenthetical
    // reasons such as permissions, unsupported parameters, or authentication.
    for (marker, close) in [
        (" (request id: ", ")"),
        (" (request_id: ", ")"),
        ("（request id: ", "）"),
    ] {
        if let Some((sentence, trace)) = message.rsplit_once(marker)
            && let Some(trace) = trace.strip_suffix(close)
            && !trace.is_empty()
            && trace
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
        {
            return trim(sentence);
        }
    }
    message
}

fn message_rejects_requested_model(message: &str, model: &str) -> bool {
    let token = |c: char| c.is_ascii_alphanumeric() || "-._".contains(c);
    message.match_indices(model).any(|(start, _)| {
        if message[..start].chars().next_back().is_some_and(token)
            || message[start + model.len()..]
                .chars()
                .next()
                .is_some_and(token)
        {
            return false;
        }
        let punctuation = |c: char| c.is_whitespace() || "'\"`:：,，".contains(c);
        let before = message[..start].trim_end_matches(punctuation);
        let after = message[start + model.len()..].trim_start_matches(punctuation);
        // Predicates are complete clauses, not prefixes: "not supported with
        // background=transparent" is a parameter error, not a missing model ID.
        ([
            "model",
            "the model",
            "requested model",
            "the requested model",
        ]
        .contains(&before)
            && [
                "does not exist",
                "not found",
                "was not found",
                "is not supported",
                "not supported",
                "is unavailable",
                "is not available",
            ]
            .contains(&after))
            || ([
                "unknown model",
                "unsupported model",
                "invalid model",
                "model not found",
            ]
            .contains(&before)
                && after.is_empty())
            || ([
                "no available channel for model",
                "no available channels for model",
            ]
            .contains(&before)
                && after.is_empty())
            || ((["模型", "请求的模型", "指定的模型", "当前分组下对于模型"].contains(&before)
                || (before.starts_with("当前分组 ") && before.ends_with(" 下对于模型")))
                && ["不存在", "不支持", "无可用渠道", "没有可用渠道"].contains(&after))
    })
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

#[cfg(test)]
mod model_rejection_tests {
    use super::model_rejected;
    use serde_json::json;

    #[test]
    fn only_explicit_model_rejections_are_safe_to_switch() {
        let model = "gpt-image-2.5-flare";
        for value in [
            json!({"error":{"code":"model_not_found"}}),
            json!({"error":{"code":"unsupported_model","param":"model"}}),
            json!({"error":{"message":"The model `gpt-image-2.5-flare` does not exist"}}),
            json!({"error":{"message":"Unknown model gpt-image-2.5-flare"}}),
            json!({"error":{"message":"No available channel for model gpt-image-2.5-flare"}}),
            json!({"error":{"message":"当前分组下对于模型 gpt-image-2.5-flare 无可用渠道"}}),
        ] {
            for status in [400, 404, 422, 503] {
                assert!(
                    model_rejected(status, &serde_json::to_vec(&value).unwrap(), model),
                    "{status} {value}"
                );
            }
        }
        for (status, value) in [
            (200, json!({"error":{"code":"model_not_found"}})),
            (
                400,
                json!({"error":{"message":"The model gpt-image-2.5-flare is not supported for this size"}}),
            ),
            (401, json!({"error":{"code":"model_not_found"}})),
            (403, json!({"error":{"code":"model_not_found"}})),
            (429, json!({"error":{"code":"model_not_found"}})),
            (500, json!({"error":{"code":"model_not_found"}})),
            (503, json!({"error":{"message":"Service unavailable"}})),
            (
                404,
                json!({"error":{"message":"Endpoint not found for model gpt-image-2.5-flare"}}),
            ),
            (
                400,
                json!({"error":{"message":"Output format not supported for model gpt-image-2.5-flare"}}),
            ),
            (
                503,
                json!({"error":{"type":"RATE_LIMIT_ERROR","message":"No available channel for model gpt-image-2.5-flare"}}),
            ),
            (
                400,
                json!({"error":{"code":"invalid_prompt","message":"model gpt-image-2.5-flare not found"}}),
            ),
            (
                404,
                json!({"message":"model gpt-image-2.5-flare not found"}),
            ),
            (
                400,
                json!({"error":{"message":"model gpt-image-2.5-flare-openai not found"}}),
            ),
            (
                400,
                json!({"error":{"message":"model gpt-image-2.5-flare not supported quality"}}),
            ),
            (
                400,
                json!({"error":{"param":"size","message":"model gpt-image-2.5-flare not supported"}}),
            ),
            (
                503,
                json!({"error":{"type":"insufficient_quota","message":"No available channel for model gpt-image-2.5-flare"}}),
            ),
            (
                400,
                json!({"error":{"code":"model_not_found","message":"gpt-image-2.5-flare quota exhausted"}}),
            ),
        ] {
            assert!(
                !model_rejected(status, &serde_json::to_vec(&value).unwrap(), model),
                "{status} {value}"
            );
        }
        assert!(!model_rejected(404, b"not a JSON error", model));
    }
}
