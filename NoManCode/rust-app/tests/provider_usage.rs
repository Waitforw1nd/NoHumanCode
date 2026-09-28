use peachsh::{
    provider::Decoder,
    provider_usage::{TokenUsage, UsageInvalidReason as Reason, UsageObservation, classify_usage},
};
use serde_json::{Value, json};

fn valid(prompt: u64, completion: u64) -> Value {
    json!({"prompt_tokens":prompt,"completion_tokens":completion,"total_tokens":prompt+completion})
}

#[test]
fn classifier_accepts_zero_and_sqlite_integer_boundary() {
    for (prompt, completion) in [
        (0, 0),
        (11, 7),
        (i64::MAX as u64, 0),
        (1, i64::MAX as u64 - 1),
    ] {
        assert_eq!(
            classify_usage(&valid(prompt, completion)),
            UsageObservation::Valid(TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
            })
        );
    }
}

#[test]
fn classifier_rejects_incomplete_non_integer_and_inconsistent_usage() {
    assert_eq!(classify_usage(&Value::Null), UsageObservation::Missing);
    for value in [json!(false), json!([]), json!(12), json!("secret-details")] {
        assert_eq!(
            classify_usage(&value),
            UsageObservation::Invalid(Reason::InvalidShape)
        );
    }
    for field in ["prompt_tokens", "completion_tokens", "total_tokens"] {
        let mut absent = valid(1, 1);
        absent.as_object_mut().unwrap().remove(field);
        assert_eq!(
            classify_usage(&absent),
            UsageObservation::Invalid(Reason::MissingField)
        );
        for wrong in [
            Value::Null,
            json!(-1),
            json!(1.0),
            json!("1"),
            json!(false),
            json!({}),
            json!([]),
        ] {
            let mut value = valid(1, 1);
            value[field] = wrong;
            assert_eq!(
                classify_usage(&value),
                UsageObservation::Invalid(Reason::InvalidInteger)
            );
        }
        for wrong in [i64::MAX as u64 + 1, u64::MAX] {
            let mut value = valid(1, 1);
            value[field] = json!(wrong);
            assert_eq!(
                classify_usage(&value),
                UsageObservation::Invalid(Reason::OutOfRange)
            );
        }
    }
    for value in [
        json!({"prompt_tokens":2,"completion_tokens":3,"total_tokens":4}),
        json!({"prompt_tokens":i64::MAX,"completion_tokens":i64::MAX,"total_tokens":i64::MAX}),
    ] {
        assert_eq!(
            classify_usage(&value),
            UsageObservation::Invalid(Reason::InconsistentTotal)
        );
    }
    let beyond_u64: Value = serde_json::from_str(
        "{\"prompt_tokens\":18446744073709551616,\"completion_tokens\":0,\"total_tokens\":0}",
    )
    .unwrap();
    assert!(matches!(
        classify_usage(&beyond_u64),
        UsageObservation::Invalid(_)
    ));
}

fn decode(usages: &[Value], chunk_size: usize) -> (Value, Value) {
    let mut data = String::from("data: {\"choices\":[{\"delta\":{\"content\":\"桃子\"}}]}\r\n\r\n");
    for usage in usages {
        data.push_str(&format!(
            "data: {}\n\n",
            json!({"choices":[],"usage":usage})
        ));
    }
    data.push_str(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    );
    let mut decoder = Decoder::default();
    let mut text = String::new();
    for bytes in data.as_bytes().chunks(chunk_size) {
        decoder
            .feed(bytes, &mut |s| {
                text.push_str(s);
                Ok(())
            })
            .unwrap();
    }
    let usage = decoder.usage.clone();
    let message = decoder.finish(&mut |_| Ok(())).unwrap();
    assert_eq!(text, "桃子");
    assert_eq!(message["content"], text);
    (message, usage)
}

#[test]
fn fragmented_decoder_normalizes_details_and_deduplicates_snapshots() {
    let mut first = valid(10, 4);
    first["prompt_tokens_details"] = json!({"cached_tokens":8,"secret":"fixture-do-not-retain"});
    let mut second = valid(10, 4);
    second["completion_tokens_details"] = json!({"reasoning_tokens":3});
    for size in [1, 2, 3, 17, 10000] {
        let (_, result) = decode(
            &[Value::Null, first.clone(), Value::Null, second.clone()],
            size,
        );
        assert_eq!(result, valid(10, 4));
        assert!(!result.to_string().contains("fixture-do-not-retain"));
    }
}

#[test]
fn decoder_latches_conflicts_even_when_total_is_unchanged() {
    for sequence in [
        vec![valid(2, 3), valid(3, 2), valid(2, 3)],
        vec![valid(2, 3), valid(2, 4), Value::Null, valid(2, 3)],
    ] {
        let (_, result) = decode(&sequence, 1);
        assert_eq!(
            classify_usage(&result),
            UsageObservation::Invalid(Reason::ConflictingDuplicate)
        );
        assert_eq!(result.as_object().unwrap().len(), 1);
        assert!(result.get("total_tokens").is_none());
    }
}

#[test]
fn decoder_never_repairs_invalid_or_combines_partial_snapshots() {
    for sequence in [
        vec![
            json!({"prompt_tokens":2}),
            json!({"completion_tokens":3,"total_tokens":5}),
            valid(2, 3),
        ],
        vec![
            valid(2, 3),
            json!({"secret":"fixture-do-not-retain"}),
            valid(2, 3),
        ],
        vec![json!("fixture-do-not-retain"), Value::Null, valid(2, 3)],
        vec![
            json!({"prompt_tokens":2,"completion_tokens":3,"total_tokens":-1}),
            valid(2, 3),
        ],
    ] {
        let (_, result) = decode(&sequence, 2);
        assert!(matches!(
            classify_usage(&result),
            UsageObservation::Invalid(_)
        ));
        assert!(!result.to_string().contains("fixture-do-not-retain"));
    }
}

#[test]
fn decoder_missing_usage_preserves_successful_message() {
    for sequence in [vec![], vec![Value::Null], vec![Value::Null, Value::Null]] {
        let (message, result) = decode(&sequence, 1);
        assert_eq!(message["role"], "assistant");
        assert_eq!(classify_usage(&result), UsageObservation::Missing);
    }
}

#[test]
fn invalid_marker_cannot_become_valid_usage_or_leak_arbitrary_reason() {
    for value in [
        json!({"_nhc_usage_invalid":"fixture-do-not-retain"}),
        json!({"_nhc_usage_invalid":42}),
        json!({"_nhc_usage_invalid":"conflicting_duplicate","prompt_tokens":1,"completion_tokens":1,"total_tokens":2}),
    ] {
        assert_eq!(
            classify_usage(&value),
            UsageObservation::Invalid(Reason::InvalidShape)
        );
        let (_, normalized) = decode(&[value], 1);
        assert!(!normalized.to_string().contains("fixture-do-not-retain"));
    }
}
