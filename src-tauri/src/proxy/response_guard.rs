use crate::proxy::sse::{strip_sse_field, take_sse_block};
use serde_json::Value;

const BLOCKED_PROVIDER_RESPONSE_URL: &str = "https://dc.hhhl.cc/chat/room/amlc1bekzi";
const BLOCKED_PROVIDER_RESPONSE_KEYWORD: &str = "公益";

#[derive(Default)]
struct BlockedContentScanner {
    saw_url: bool,
    saw_keyword: bool,
    tail: String,
}

impl BlockedContentScanner {
    fn observe(&mut self, text: &str) {
        if self.is_blocked() || text.is_empty() {
            return;
        }

        let combined = format!("{}{}", self.tail, text);
        self.saw_url |= combined.contains(BLOCKED_PROVIDER_RESPONSE_URL);
        self.saw_keyword |= combined.contains(BLOCKED_PROVIDER_RESPONSE_KEYWORD);

        let keep_chars = BLOCKED_PROVIDER_RESPONSE_URL
            .chars()
            .count()
            .max(BLOCKED_PROVIDER_RESPONSE_KEYWORD.chars().count())
            .saturating_sub(1);
        self.tail = combined
            .chars()
            .rev()
            .take(keep_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
    }

    fn observe_response_content(&mut self, value: &Value) {
        match value {
            Value::String(text) => self.observe(text),
            Value::Array(values) => {
                for value in values {
                    self.observe_response_content(value);
                    if self.is_blocked() {
                        return;
                    }
                }
            }
            Value::Object(map) => {
                for (key, value) in map {
                    if matches!(
                        key.as_str(),
                        "text" | "delta" | "refusal" | "content" | "output"
                    ) {
                        self.observe_response_content(value);
                    } else if matches!(value, Value::Array(_) | Value::Object(_)) {
                        self.observe_response_content(value);
                    }
                    if self.is_blocked() {
                        return;
                    }
                }
            }
            _ => {}
        }
    }

    fn is_blocked(&self) -> bool {
        self.saw_url && self.saw_keyword
    }
}

pub(crate) fn responses_success_has_blocked_content(body: &Value) -> bool {
    if !looks_like_responses_success(body) {
        return false;
    }

    let mut scanner = BlockedContentScanner::default();
    scanner.observe_response_content(body);
    scanner.is_blocked()
}

pub(crate) fn responses_sse_has_blocked_content(body: &str) -> bool {
    let mut buffer = body.to_string();
    let mut scanner = BlockedContentScanner::default();
    let mut saw_responses_event = false;

    while let Some(block) = take_sse_block(&mut buffer) {
        let mut event_name = "";
        let mut data_lines: Vec<&str> = Vec::new();

        for line in block.lines() {
            if let Some(evt) = strip_sse_field(line, "event") {
                event_name = evt.trim();
            } else if let Some(data) = strip_sse_field(line, "data") {
                data_lines.push(data);
            }
        }

        if event_name.starts_with("response.") {
            saw_responses_event = true;
        }

        if data_lines.is_empty() {
            continue;
        }

        let data = data_lines.join("\n");
        if data.trim() == "[DONE]" {
            continue;
        }

        match serde_json::from_str::<Value>(&data) {
            Ok(value) => {
                if value
                    .get("type")
                    .and_then(|value| value.as_str())
                    .is_some_and(|event_type| event_type.starts_with("response."))
                {
                    saw_responses_event = true;
                }
                scanner.observe_response_content(&value);
            }
            Err(_) => scanner.observe(&data),
        }

        if saw_responses_event && scanner.is_blocked() {
            return true;
        }
    }

    saw_responses_event && scanner.is_blocked()
}

fn looks_like_responses_success(body: &Value) -> bool {
    body.get("object").and_then(|value| value.as_str()) == Some("response")
        || body
            .get("output")
            .and_then(|value| value.as_array())
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn responses_success_with_blocked_markers_is_detected() {
        let body = json!({
            "id": "resp_local_sample",
            "object": "response",
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "请关注公益项目 https://dc.hhhl.cc/chat/room/amlc1bekzi"
                }]
            }],
            "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}
        });

        assert!(responses_success_has_blocked_content(&body));
    }

    #[test]
    fn responses_success_requires_both_blocked_markers() {
        let only_url = json!({
            "object": "response",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "https://dc.hhhl.cc/chat/room/amlc1bekzi"}]}]
        });
        let only_keyword = json!({
            "object": "response",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "公益"}]}]
        });

        assert!(!responses_success_has_blocked_content(&only_url));
        assert!(!responses_success_has_blocked_content(&only_keyword));
    }

    #[test]
    fn responses_sse_with_split_blocked_markers_is_detected() {
        let sse = "event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"delta\":\"公益项目 \"}\n\n\
event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"delta\":\"https://dc.hhhl.cc/chat\"}\n\n\
event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"delta\":\"/room/amlc1bekzi\"}\n\n\
event: response.completed\n\
data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n";

        assert!(responses_sse_has_blocked_content(sse));
    }

    #[test]
    fn responses_sse_requires_both_blocked_markers() {
        let sse = "event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"delta\":\"普通公益说明\"}\n\n\
event: response.completed\n\
data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n";

        assert!(!responses_sse_has_blocked_content(sse));
    }
}
