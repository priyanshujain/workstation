use serde_json::Value;
use share_protocol::{AgentMessage, ProgramStatus, Theme};

pub fn agent_message(control: &Value) -> Option<AgentMessage> {
    match control["type"].as_str()? {
        "resize" => Some(AgentMessage::Resize {
            cols: dimension(&control["cols"])?,
            rows: dimension(&control["rows"])?,
        }),
        "theme" => Some(AgentMessage::Theme {
            theme: theme(control),
        }),
        "child_exited" => Some(AgentMessage::Ended {
            reason: exit_reason(control),
        }),
        _ => None,
    }
}

pub fn exit_reason(payload: &Value) -> String {
    match payload["exit_code"].as_i64() {
        Some(code) => format!("program exited with status {code}"),
        None => "program exited".to_string(),
    }
}

pub fn current_status(reply: &Value) -> Option<ProgramStatus> {
    reply["records"]
        .as_array()?
        .iter()
        .max_by_key(|record| record["updated_at"].as_str().unwrap_or_default())
        .map(status)
}

fn status(record: &Value) -> ProgramStatus {
    ProgramStatus {
        state: text(&record["state"]).unwrap_or_else(|| "idle".to_string()),
        kind: text(&record["kind"]),
        app: text(&record["effective_app"]).or_else(|| text(&record["app"])),
        title: text(&record["title"]),
        msg: text(&record["msg"]),
        progress: record["progress"]
            .as_u64()
            .and_then(|value| u8::try_from(value).ok()),
    }
}

fn theme(control: &Value) -> Theme {
    Theme {
        foreground: text(&control["foreground"]),
        background: text(&control["background"]),
        cursor: text(&control["cursor"]),
        palette: control["palette"]
            .as_array()
            .map(|colors| colors.iter().filter_map(text).collect())
            .unwrap_or_default(),
    }
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn dimension(value: &Value) -> Option<u16> {
    value.as_u64().and_then(|value| u16::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_resize() {
        let control = json!({"type":"resize","cols":120,"rows":40,"cell_width_px":8});
        assert_eq!(
            agent_message(&control),
            Some(AgentMessage::Resize {
                cols: 120,
                rows: 40
            })
        );
    }

    #[test]
    fn maps_theme() {
        let control = json!({"type":"theme","foreground":"#ffffff","background":"#000000","palette":["0=#000000","1=#ff0000"]});
        let Some(AgentMessage::Theme { theme }) = agent_message(&control) else {
            panic!()
        };
        assert_eq!(theme.foreground.as_deref(), Some("#ffffff"));
        assert_eq!(theme.cursor, None);
        assert_eq!(theme.palette, vec!["0=#000000", "1=#ff0000"]);
    }

    #[test]
    fn maps_child_exit() {
        let control = json!({"type":"child_exited","exit_code":2});
        assert_eq!(
            agent_message(&control),
            Some(AgentMessage::Ended {
                reason: "program exited with status 2".to_string()
            })
        );
    }

    #[test]
    fn ignores_unknown_controls() {
        assert_eq!(agent_message(&json!({"type":"future"})), None);
    }

    #[test]
    fn picks_latest_status_record() {
        let reply = json!({"records":[
            {"state":"done","app":"cargo","updated_at":"2026-11-16T10:00:00Z"},
            {"state":"working","effective_app":"brew","msg":"Installing","progress":40,"updated_at":"2026-11-16T10:05:00Z"},
        ]});
        let status = current_status(&reply).unwrap();
        assert_eq!(status.state, "working");
        assert_eq!(status.app.as_deref(), Some("brew"));
        assert_eq!(status.progress, Some(40));
    }

    #[test]
    fn reports_no_status_without_records() {
        assert_eq!(current_status(&json!({"records":[]})), None);
    }
}
