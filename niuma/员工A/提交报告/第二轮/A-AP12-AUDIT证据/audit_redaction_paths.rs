use peachsh::secrets;
use serde_json::{Value, json};

#[test]
fn approved_command_bearer_output_diverges_between_finished_evidence_paths() {
    let raw = json!({
        "status": 0,
        "stderr": "",
        "stdout": "bearer abc123\r\n"
    });
    let serialized = raw.to_string();

    // finish_approval stores this representation in the tool message, after
    // safe_task_value recursively redacts Task JSON.
    let message_content = secrets::redact_persisted(&Value::String(serialized));
    let message_result = message_content
        .as_str()
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .unwrap_or_else(|| Value::String(message_content.as_str().unwrap().to_owned()));

    // task_event_tx parses the result first and then redacts the structure.
    let event_result = secrets::redact_persisted(&raw);

    println!("message_content={message_content}");
    println!("message_result={message_result}");
    println!("event_result={event_result}");
    assert_ne!(message_result, event_result);
}
