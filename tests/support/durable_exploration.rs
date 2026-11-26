use super::{parallel_exploration::Fixture, *};
use coda::budget::BudgetCallState;
use tokio_util::sync::CancellationToken;

async fn completed_parent() -> Fixture {
    let mut f = Fixture::start_parent(1, 2, 100).await;
    f.next().await.answer("ready for independent exploration");
    let run = wait_for_run(&f.engine, &f.run.id).await;
    assert_eq!(run.tasks[0].status, "completed");
    f
}

fn branch(path: &str, query: &str, deps: &[&str]) -> Value {
    json!({"path":path,"question":format!("question-{path}"),"query":query,"depends_on":deps})
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn concurrent_continuations_share_one_attempt_and_only_wait_for_their_dependencies() {
    let mut f = completed_parent().await;
    let engine = f.engine.clone();
    let parent = f.run.tasks[0].id.clone();
    let first = engine
        .submit_exploration(
            &parent,
            "first",
            &json!({"branches":[
                branch("alpha","search-marker",&[]),
                branch("beta","search-marker",&[]),
                branch("alpha","no-such-text",&[])
            ]}),
        )
        .unwrap();
    let [a, b, empty] = first.jobs.as_slice() else {
        panic!("expected three jobs")
    };
    let next = engine
        .submit_exploration(
            &parent,
            "second",
            &json!({"branches":[
                branch("gamma","search-marker",&[&a.id,&empty.id]),
                branch("gamma","search-marker",&[&b.id])
            ]}),
        )
        .unwrap();
    let [c, blocked] = next.jobs.as_slice() else {
        panic!("expected two jobs")
    };
    let cancel = CancellationToken::new();

    let (failed, ()) = tokio::join!(
        engine.continue_exploration(&parent, &b.id, false, &cancel),
        async {
            let req = f.next().await;
            assert!(
                req.body["messages"]
                    .to_string()
                    .contains("source-only-beta")
            );
            req.reply(
                json!({"reasoning_content":"unusable private reasoning"}),
                "stop",
            );
        }
    );
    assert_eq!(failed.unwrap().status, "failed");

    let (left, right, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(
            engine.continue_exploration(&parent, &c.id, false, &cancel),
            engine.continue_exploration(&parent, &c.id, false, &cancel),
            async {
                let req = f.next().await;
                assert!(
                    req.body["messages"]
                        .to_string()
                        .contains("source-only-alpha")
                );
                req.answer("saved alpha summary");

                let req = f.next().await;
                let prompt = req.body["messages"][1]["content"].as_str().unwrap();
                assert!(prompt.contains("source-only-gamma"));
                assert!(prompt.contains("saved alpha summary"));
                assert!(prompt.contains("alpha/sample.txt"));
                assert!(prompt.contains(&empty.id) && prompt.contains("no_matches"));
                for private in [
                    "source-only-alpha",
                    "source-only-beta",
                    "question-beta",
                    "unusable private reasoning",
                ] {
                    assert!(!prompt.contains(private));
                }
                req.answer("gamma consumed alpha");
            }
        )
    })
    .await
    .expect("both wakeups should finish with one global slot");
    for joined in [left.unwrap(), right.unwrap()] {
        assert_eq!(joined.status, "succeeded");
        assert_eq!(joined.attempt, 1);
        assert_eq!(joined.receipts.len(), 1);
        assert_eq!(joined.result.unwrap()["summary"], "gamma consumed alpha");
    }
    let batch = engine
        .continue_exploration_batch(&parent, &next.id, &cancel)
        .await
        .unwrap();
    assert_eq!(batch.jobs[1].id, blocked.id);
    assert_eq!(batch.jobs[1].status, "queued");
    assert!(batch.jobs[1].receipts.is_empty());
    let no_matches = engine.exploration_job(&parent, &empty.id).unwrap();
    assert_eq!(no_matches.status, "no_matches");
    assert!(no_matches.receipts.is_empty());
    let calls = engine.network_calls_for_task(&parent).unwrap();
    assert_eq!(calls.len(), 4); // parent, failed B, A, C
    assert_eq!(
        engine.store.task(&parent).unwrap().usage["total_tokens"],
        12
    );
    for call in calls {
        assert_eq!(
            engine.budget_call(&f.project, &call.call_id).unwrap().state,
            Some(BudgetCallState::Settled)
        );
    }
    let budget = engine.budget_status(&f.project).unwrap();
    assert_eq!((budget.spent_tokens, budget.held_tokens), (12, 0));
    assert!(f.requests.try_recv().is_err());
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn revoking_authorization_after_submission_prevents_dispatch() {
    let mut f = completed_parent().await;
    let parent = &f.run.tasks[0].id;
    let batch = f
        .engine
        .submit_exploration(
            parent,
            "before-revocation",
            &json!({"branches":[branch("alpha","search-marker",&[])]}),
        )
        .unwrap();
    let before = f.engine.network_calls_for_task(parent).unwrap();
    assert_eq!(before.len(), 1);
    f.engine
        .network_revoke(&before[0].authorization_id)
        .await
        .unwrap();
    let result = f
        .engine
        .continue_exploration(parent, &batch.jobs[0].id, false, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.status, "failed");
    assert!(result.receipts.is_empty());
    assert_eq!(f.engine.network_calls_for_task(parent).unwrap(), before);
    let budget = f.engine.budget_status(&f.project).unwrap();
    assert_eq!((budget.spent_tokens, budget.held_tokens), (3, 0));
    assert!(f.requests.try_recv().is_err());
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn dependency_from_another_session_rejects_every_branch_before_dispatch() {
    let mut f = completed_parent().await;
    let parent = &f.run.tasks[0];
    let prior = f
        .engine
        .submit_exploration(
            &parent.id,
            "first",
            &json!({"branches":[branch("alpha","search-marker",&[])]}),
        )
        .unwrap();
    let other = f
        .engine
        .start(RunRequest {
            title: "another session".into(),
            kind: SessionKind::Chat,
            tasks: vec![parent.spec.clone()],
        })
        .await
        .unwrap();
    f.next().await.answer("another session ready");
    let parent = &wait_for_run(&f.engine, &other.id).await.tasks[0];
    assert_eq!(parent.status, "completed");
    let rejected = f.engine.submit_exploration(
        &parent.id,
        "invalid",
        &json!({"branches":[
            branch("gamma","search-marker",&[]),branch("beta","search-marker",&[&prior.jobs[0].id])
        ]}),
    );
    assert!(rejected.is_err());
    assert!(f.engine.exploration_jobs(&parent.id).unwrap().is_empty());
    assert_eq!(
        f.engine.network_calls_for_task(&parent.id).unwrap().len(),
        1
    );
    assert!(f.requests.try_recv().is_err());
}
