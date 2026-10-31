#![cfg(windows)]
use peachsh::{
    engine::Engine,
    plugin_management::SetPluginEnabled,
    runtime_management::RuntimeId,
    server::{self, App},
    store::Store,
};
use reqwest::Client;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc, time::Duration};

const TOKEN: &str = "composition-http-secret-token";
const STORE_SECRET: &str = "synthetic-store-credential-731";

#[tokio::test]
async fn composition_contract_security_disabled_and_storage_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("composition.sqlite3");
    let engine = Engine::new(Arc::new(Store::open(&db).unwrap()), 1).unwrap();
    engine
        .store
        .put_secret("route-secret", STORE_SECRET)
        .unwrap();
    for (id, key) in [
        ("builtin.files", "files-off"),
        ("builtin.shell", "shell-off"),
    ] {
        engine
            .set_plugin_enabled(
                id,
                SetPluginEnabled {
                    enabled: false,
                    expected_revision: 0,
                },
                key,
            )
            .unwrap();
    }
    for (id, key) in [
        (RuntimeId::ChatAgent, "agent-off"),
        (RuntimeId::CompletionProvider, "provider-off"),
    ] {
        engine
            .set_runtime_enabled(
                id,
                SetPluginEnabled {
                    enabled: false,
                    expected_revision: 0,
                },
                key,
            )
            .unwrap();
    }
    drop(engine);
    let engine = Engine::new(Arc::new(Store::open(&db).unwrap()), 1).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = server::router(App {
        engine,
        token: TOKEN.into(),
        origin: origin.clone(),
    });
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let endpoint = format!("{origin}/api/host/composition");
    for request in [
        client.get(&endpoint),
        client.get(&endpoint).header("x-peachsh-token", "wrong"),
        client
            .get(&endpoint)
            .header("x-peachsh-token", TOKEN)
            .header("host", "evil.invalid"),
        client
            .get(&endpoint)
            .header("x-peachsh-token", TOKEN)
            .header("origin", "http://evil.invalid"),
        client
            .get(&endpoint)
            .header("x-peachsh-token", TOKEN)
            .header("sec-fetch-site", "cross-site"),
    ] {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 403);
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert!(!response.text().await.unwrap().contains(TOKEN));
    }
    let response = client
        .get(&endpoint)
        .header("x-peachsh-token", TOKEN)
        .header("origin", &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "version",
            "consistency",
            "capabilities",
            "plugins",
            "runtimes"
        ])
    );
    assert_eq!(value["version"], 1);
    assert_eq!(value["consistency"], "per_component");
    let expected = [
        ("core", "host.core", vec![], vec!["host.core"], false),
        (
            "session-log",
            "host.session-log",
            vec!["core"],
            vec!["host.session-log"],
            false,
        ),
        (
            "local-project",
            "host.local-project",
            vec!["core", "workspace"],
            vec!["host.local-project"],
            false,
        ),
        (
            "workspace",
            "host.workspace",
            vec!["core"],
            vec!["host.workspace"],
            false,
        ),
        (
            "provider-xpeach",
            "builtin.provider-xpeach",
            vec!["core"],
            vec!["provider.completion-codec"],
            true,
        ),
        (
            "file-read-search",
            "builtin.files",
            vec!["workspace", "local-project", "approval-policy"],
            vec!["tool.read_file", "tool.list_files", "tool.search_files"],
            true,
        ),
        (
            "approval-policy",
            "host.approval-policy",
            vec!["core", "local-project"],
            vec!["host.approval-policy"],
            false,
        ),
        (
            "file-write",
            "builtin.files",
            vec!["workspace", "local-project", "approval-policy"],
            vec!["tool.write_file"],
            true,
        ),
        (
            "shell",
            "builtin.shell",
            vec!["workspace", "local-project", "approval-policy"],
            vec!["tool.run_command"],
            true,
        ),
        (
            "git-diff",
            "host.review-recovery",
            vec!["workspace", "local-project"],
            vec!["host.git-diff"],
            false,
        ),
        (
            "checkpoint-restore",
            "host.review-recovery",
            vec!["workspace", "local-project"],
            vec!["host.checkpoint-restore"],
            false,
        ),
        (
            "recovery",
            "host.recovery",
            vec!["core"],
            vec!["host.recovery"],
            false,
        ),
        (
            "chat-agent",
            "builtin.chat-agent",
            vec!["core", "approval-policy"],
            vec!["agent.strategy"],
            true,
        ),
    ];
    let caps = value["capabilities"].as_array().unwrap();
    assert_eq!(caps.len(), expected.len());
    let ids: BTreeSet<_> = caps.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 13);
    for (id, provider, dependencies, interfaces, managed) in expected {
        let c = caps.iter().find(|c| c["id"] == id).unwrap();
        assert_eq!(
            c.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "id",
                "binding_kind",
                "provider_id",
                "provider_version",
                "interfaces",
                "lifecycle",
                "dependencies"
            ])
        );
        assert_eq!(c["provider_id"], provider);
        assert_eq!(
            c["provider_version"],
            if provider == "builtin.files" {
                "1.2.0"
            } else {
                "1.0.0"
            }
        );
        assert_eq!(
            c["binding_kind"],
            if managed {
                "plugin_runtime"
            } else {
                "host_service"
            }
        );
        assert_eq!(
            c["lifecycle"],
            if managed { "managed" } else { "host_required" }
        );
        assert_eq!(c["dependencies"], json!(dependencies));
        let names: BTreeSet<_> = c["interfaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                assert_eq!(
                    i.as_object()
                        .unwrap()
                        .keys()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>(),
                    BTreeSet::from(["name", "version"])
                );
                assert_eq!(i["version"], "1.0.0");
                i["name"].as_str().unwrap()
            })
            .collect();
        assert_eq!(names, interfaces.into_iter().collect());
    }
    let plugins: Value = client
        .get(format!("{origin}/api/plugins"))
        .header("x-peachsh-token", TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let runtimes: Value = client
        .get(format!("{origin}/api/runtime/components"))
        .header("x-peachsh-token", TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(value["plugins"], plugins["plugins"]);
    assert_eq!(value["runtimes"], runtimes["components"]);
    for entry in value["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .chain(value["runtimes"].as_array().unwrap())
    {
        assert_eq!(entry["desired"]["enabled"], false);
        assert_eq!(entry["runtime"]["admission_open"], false);
    }
    assert!(!value.to_string().contains(TOKEN));
    assert!(!value.to_string().contains(STORE_SECRET));
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "DELETE FROM plugin_desired WHERE plugin_id='builtin.files'",
            [],
        )
        .unwrap();
    let failure = client
        .get(&endpoint)
        .header("x-peachsh-token", TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(failure.status(), 404);
    let body = failure.text().await.unwrap();
    for forbidden in [
        TOKEN,
        STORE_SECRET,
        "plugin_desired",
        "SELECT",
        "sqlite",
        "composition.sqlite3",
    ] {
        assert!(!body.contains(forbidden), "{body}");
    }
    server.abort();
}
