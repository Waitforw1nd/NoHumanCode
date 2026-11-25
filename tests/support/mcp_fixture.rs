//! Re-enter the requesting test as a real MCP stdio server with the test binary's JSON parser.
use serde_json::{Value, json};
use std::io::{BufRead, Write};

pub fn server(test_name: &str) -> Value {
    json!({
        "command": std::env::current_exe().unwrap(),
        "args": ["--exact", test_name, "--nocapture"],
        "env": {"CODA_TEST_MCP_SERVER": "1", "FOO": "bar", "EXPLICIT_TOKEN": "allowed-explicit"}
    })
}

pub fn serve_if_child() -> bool {
    if std::env::var_os("CODA_TEST_MCP_SERVER").is_none() {
        return false;
    }
    let mut stdout = std::io::stdout().lock();
    // The test harness can print a test-name prefix without a trailing newline.
    writeln!(stdout).unwrap();
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let result = match request["method"].as_str().unwrap() {
            "initialize" => {
                json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"demo","version":"1"}})
            }
            "notifications/initialized" => continue,
            "tools/list" => {
                json!({"tools":[{"name":"echo_tool","description":"Echoes input","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]})
            }
            "tools/call" => {
                assert_eq!(request["params"]["name"], "echo_tool");
                let text = request["params"]["arguments"]["text"].as_str().unwrap();
                let inherited_value = std::env::var("FOO").unwrap();
                let token = std::env::var("EXPLICIT_TOKEN").unwrap();
                json!({"content":[{"type":"text","text":format!("mcp-echo:{text}|foo={inherited_value}|token={token}")}],"isError":false})
            }
            method => panic!("unexpected MCP method {method}"),
        };
        let response = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
        writeln!(stdout, "{response}").unwrap();
        stdout.flush().unwrap();
    }
    true
}
