use peachsh::{domain::*, engine::Engine, provider, store::Store};
use std::{sync::Arc, time::Duration};

// Explicit opt-in. The key stays in the parent environment; this test never saves it.
#[tokio::test]
#[ignore = "requires PEACHSH_TEST_KEY; makes small paid model calls"]
async fn xpeach_native_stream_and_coding_tool() {
    assert!(
        std::env::var("PEACHSH_TEST_KEY").is_ok(),
        "Set PEACHSH_TEST_KEY first"
    );
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&temp.path().join("live.db")).unwrap());
    let routes = [("sol", "gpt-5.6-sol"), ("astra", "gpt-6-astra")]
        .into_iter()
        .map(|(id, model)| Route {
            id: id.into(),
            name: id.into(),
            base_url: "https://xpeach.codes/v1".into(),
            model: model.into(),
            max_tokens: 1024,
            parallel_limit: 2,
            key_env: Some("PEACHSH_TEST_KEY".into()),
        })
        .collect::<Vec<_>>();
    store
        .save_settings(&Settings {
            workspace: temp.path().to_string_lossy().into(),
            max_concurrency: 2,
            routes: routes.clone(),
            newapi: None,
        })
        .unwrap();
    let engine = Engine::new(store, 2).unwrap();
    let models = provider::models(&engine.client, &routes[0], &engine.key(&routes[0]).unwrap())
        .await
        .unwrap();
    assert!(models.contains(&"gpt-5.6-sol".into()) && models.contains(&"gpt-6-astra".into()));
    let tasks = routes
        .iter()
        .map(|r| TaskSpec {
            name: r.id.clone(),
            role: "connectivity-check".into(),
            route_id: r.id.clone(),
            prompt: "Return only the number for 17 * 19.".into(),
            write_scopes: vec![],
            depends_on: vec![],
            tools: false,
            allow_commands: false,
            max_rounds: 1,
        })
        .collect();
    let run = engine
        .start(RunRequest {
            kind: SessionKind::Team,
            title: "Rust live parallel connectivity".into(),
            tasks,
        })
        .await
        .unwrap();
    let finished = wait(&engine, &run.id).await;
    for task in &finished.tasks {
        assert_eq!(
            task.status, "completed",
            "{}: {:?}",
            task.route.model, task.error
        );
        assert_eq!(task.output.trim(), "323");
        println!("{}: streaming + native scheduling passed", task.route.model);
    }
    let run=engine.start(RunRequest{kind:SessionKind::Team,title:"Rust live file tool".into(),tasks:vec![TaskSpec{name:"file-check".into(),role:"coding-tool-check".into(),route_id:"sol".into(),prompt:"Use write_file to create smoke/result.txt with exactly rust-live-ok (no newline). Then read_file to verify the exact content. Do not use commands. Reply OK after verification.".into(),depends_on:vec![],write_scopes:vec!["smoke".into()],tools:true,allow_commands:false,max_rounds:5}]}).await.unwrap();
    let finished = wait(&engine, &run.id).await;
    if finished.tasks[0].status != "completed" {
        for event in engine.store.events(&run.id, 0).unwrap() {
            println!("{}: {}", event.kind, event.data);
        }
    }
    assert_eq!(
        finished.tasks[0].status, "completed",
        "{:?}",
        finished.tasks[0].error
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("smoke/result.txt")).unwrap(),
        "rust-live-ok"
    );
    let events = engine.store.events(&run.id, 0).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.kind == "tool_start" && e.data["name"] == "read_file")
    );
    println!("gpt-5.6-sol: native tool write + read verification passed");
}
async fn wait(engine: &Engine, id: &str) -> Run {
    tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            let r = engine.store.run(id).unwrap();
            if !engine.is_busy() {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap()
}
