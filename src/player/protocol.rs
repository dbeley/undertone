//! mpv JSON IPC: encoding commands, decoding events.
//!
//! Kept free of I/O so the wire format is unit-testable.

use serde_json::{json, Value};

use super::{EndReason, PlayerEvent};

/// Encode a command as one line of mpv IPC JSON.
pub fn encode_command(args: &[&str]) -> String {
    json!({ "command": args }).to_string()
}

/// Encode `observe_property`.
pub fn encode_observe(id: u32, property: &str) -> String {
    json!({ "command": ["observe_property", id, property] }).to_string()
}

/// Set a property: `set <name> <value>`.
pub fn encode_set(name: &str, value: &str) -> String {
    encode_command(&["set", name, value])
}

/// Parse one line of mpv output into an event.
///
/// Returns `None` for replies (which carry `request_id` and no `event`), for
/// properties we do not observe, and for data that is null (mpv reports
/// `time-pos` as null before the first frame).
pub fn parse_event(line: &str) -> Option<PlayerEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    let event = value.get("event")?.as_str()?;

    match event {
        "property-change" => {
            let name = value.get("name")?.as_str()?;
            let data = value.get("data")?;
            match name {
                "time-pos" => data.as_f64().map(PlayerEvent::Position),
                "duration" => data.as_f64().map(PlayerEvent::Duration),
                "pause" => data.as_bool().map(PlayerEvent::Paused),
                "volume" => data.as_f64().map(PlayerEvent::Volume),
                _ => None,
            }
        }
        "file-loaded" => Some(PlayerEvent::FileLoaded),
        "end-file" => Some(PlayerEvent::Ended(end_reason(
            value.get("reason").and_then(Value::as_str),
        ))),
        "idle" => Some(PlayerEvent::Idle),
        _ => None,
    }
}

fn end_reason(reason: Option<&str>) -> EndReason {
    match reason {
        Some("eof") => EndReason::Eof,
        Some("stop") => EndReason::Stop,
        Some("error") => EndReason::Error,
        Some("redirect") => EndReason::Redirect,
        Some("quit") => EndReason::Quit,
        _ => EndReason::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_command_as_json_line() {
        assert_eq!(
            encode_command(&["loadfile", "http://x/y.mp3", "replace"]),
            r#"{"command":["loadfile","http://x/y.mp3","replace"]}"#
        );
    }

    #[test]
    fn encodes_property_observation() {
        assert_eq!(
            encode_observe(1, "time-pos"),
            r#"{"command":["observe_property",1,"time-pos"]}"#
        );
    }

    #[test]
    fn encodes_set() {
        assert_eq!(
            encode_set("pause", "false"),
            r#"{"command":["set","pause","false"]}"#
        );
    }

    #[test]
    fn parses_position_and_duration() {
        let line = r#"{"event":"property-change","id":1,"name":"time-pos","data":12.5}"#;
        assert_eq!(parse_event(line), Some(PlayerEvent::Position(12.5)));
        let line = r#"{"event":"property-change","id":2,"name":"duration","data":213.0}"#;
        assert_eq!(parse_event(line), Some(PlayerEvent::Duration(213.0)));
    }

    #[test]
    fn parses_pause_and_volume() {
        let line = r#"{"event":"property-change","name":"pause","data":true}"#;
        assert_eq!(parse_event(line), Some(PlayerEvent::Paused(true)));
        let line = r#"{"event":"property-change","name":"volume","data":65.0}"#;
        assert_eq!(parse_event(line), Some(PlayerEvent::Volume(65.0)));
    }

    #[test]
    fn parses_end_file_reasons() {
        for (reason, expected) in [
            ("eof", EndReason::Eof),
            ("stop", EndReason::Stop),
            ("error", EndReason::Error),
            ("redirect", EndReason::Redirect),
            ("quit", EndReason::Quit),
            ("nonsense", EndReason::Unknown),
        ] {
            let line = format!(r#"{{"event":"end-file","reason":"{reason}"}}"#);
            assert_eq!(parse_event(&line), Some(PlayerEvent::Ended(expected)));
        }
    }

    #[test]
    fn parses_file_loaded_and_idle() {
        assert_eq!(
            parse_event(r#"{"event":"file-loaded"}"#),
            Some(PlayerEvent::FileLoaded)
        );
        assert_eq!(parse_event(r#"{"event":"idle"}"#), Some(PlayerEvent::Idle));
    }

    #[test]
    fn ignores_replies_and_unknown_events() {
        // A command reply, not an event.
        assert_eq!(parse_event(r#"{"request_id":1,"error":"success"}"#), None);
        assert_eq!(parse_event(r#"{"event":"playback-restart"}"#), None);
        assert_eq!(
            parse_event(r#"{"event":"property-change","name":"seekable","data":true}"#),
            None
        );
    }

    #[test]
    fn ignores_null_and_malformed_data() {
        // mpv reports time-pos as null before the first frame.
        assert_eq!(
            parse_event(r#"{"event":"property-change","name":"time-pos","data":null}"#),
            None
        );
        assert_eq!(parse_event("not json"), None);
        assert_eq!(parse_event(""), None);
    }
}
