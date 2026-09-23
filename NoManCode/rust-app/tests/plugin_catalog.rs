//! Public-API contract tests for `plugin_catalog` (D-R1-01 / NEXT-02A).
//!
//! Pure in-memory tests: no filesystem, network, database, provider, process,
//! or WASM execution. Each D-item records its assertion inline.

use peachsh::plugin_catalog::*;
use serde_json::json;

fn v(major: u32, minor: u32, patch: u32) -> ExactVersion {
    ExactVersion {
        major,
        minor,
        patch,
    }
}

fn ikey(kind: InterfaceKind, name: &str, version: (u32, u32, u32)) -> InterfaceKey {
    InterfaceKey {
        kind,
        name: name.to_string(),
        version: v(version.0, version.1, version.2),
    }
}

fn svc(name: &str, version: (u32, u32, u32)) -> InterfaceKey {
    ikey(InterfaceKind::Service, name, version)
}

fn manifest(
    id: &str,
    version: (u32, u32, u32),
    scope: ScopeKind,
    provides: Vec<InterfaceKey>,
    requires: Vec<InterfaceKey>,
) -> CatalogManifestV1 {
    CatalogManifestV1 {
        manifest_version: 1,
        id: id.to_string(),
        display_name: format!("{id} plugin"),
        version: v(version.0, version.1, version.2),
        scope,
        runtime: RuntimeKind::Builtin,
        lifecycle: LifecycleKind::HostManaged,
        surfaces: vec![Surface::Cli],
        permissions: Vec::new(),
        config_schema: json!({}),
        provides,
        requires,
    }
}

fn session_manifest(
    id: &str,
    version: (u32, u32, u32),
    provides: Vec<InterfaceKey>,
    requires: Vec<InterfaceKey>,
) -> CatalogManifestV1 {
    manifest(id, version, ScopeKind::Session, provides, requires)
}

fn kind_str(kind: InterfaceKind) -> &'static str {
    match kind {
        InterfaceKind::Service => "service",
        InterfaceKind::Command => "command",
        InterfaceKind::Query => "query",
        InterfaceKind::Event => "event",
        InterfaceKind::Resource => "resource",
        InterfaceKind::Capability => "capability",
    }
}

fn scope_str(scope: ScopeKind) -> &'static str {
    match scope {
        ScopeKind::Host => "host",
        ScopeKind::Project => "project",
        ScopeKind::Session => "session",
    }
}

fn key_json(key: &InterfaceKey) -> serde_json::Value {
    json!({
        "kind": kind_str(key.kind),
        "name": key.name,
        "version": {"major": key.version.major, "minor": key.version.minor, "patch": key.version.patch},
    })
}

fn manifest_json(
    id: &str,
    version: (u32, u32, u32),
    scope: ScopeKind,
    provides: Vec<InterfaceKey>,
    requires: Vec<InterfaceKey>,
) -> String {
    json!({
        "manifest_version": 1,
        "id": id,
        "display_name": format!("{id} plugin"),
        "version": {"major": version.0, "minor": version.1, "patch": version.2},
        "scope": scope_str(scope),
        "runtime": "builtin",
        "lifecycle": "host_managed",
        "surfaces": ["cli"],
        "permissions": [],
        "config_schema": {},
        "provides": provides.iter().map(key_json).collect::<Vec<_>>(),
        "requires": requires.iter().map(key_json).collect::<Vec<_>>(),
    })
    .to_string()
}

fn session_scope() -> ScopeKey {
    ScopeKey::Session {
        project_id: "prj-demo".to_string(),
        session_id: "ses-demo".to_string(),
    }
}

fn session_catalog() -> PluginCatalog {
    PluginCatalog::new(session_scope()).unwrap()
}

fn roots(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

fn plugin_ids(plan: &ResolutionPlan) -> Vec<&str> {
    plan.ordered_plugins
        .iter()
        .map(|plugin| plugin.id.as_str())
        .collect()
}

/// Assert every hop of a closed path is a real consumer→provider edge and the
/// path is closed (first == last).
fn assert_closed_dep_path(path: &[String], real_edges: &[(&str, &str)]) {
    assert!(path.len() >= 2, "cycle path too short: {path:?}");
    assert_eq!(path.first(), path.last(), "path not closed: {path:?}");
    for hop in path.windows(2) {
        assert!(
            real_edges.contains(&(hop[0].as_str(), hop[1].as_str())),
            "hop {} -> {} is not a real dependency edge",
            hop[0],
            hop[1]
        );
    }
}

/// D01: full chain — JSON parse two declarations, register, resolve.
/// Asserts plugin version and interface version are independent, the provider
/// precedes the consumer, and the plan carries scope plus full bindings.
#[test]
fn d01_full_chain_parse_register_resolve() {
    let provider = parse_manifest(&manifest_json(
        "chat.core",
        (0, 5, 0),
        ScopeKind::Session,
        vec![svc("chat.history", (1, 0, 0))],
        vec![],
    ))
    .unwrap();
    let consumer = parse_manifest(&manifest_json(
        "chat.app",
        (2, 0, 0),
        ScopeKind::Session,
        vec![],
        vec![svc("chat.history", (1, 0, 0))],
    ))
    .unwrap();
    // Plugin version is not the interface version.
    assert_eq!(provider.version, v(0, 5, 0));

    let mut catalog = session_catalog();
    catalog.register(provider).unwrap();
    catalog.register(consumer).unwrap();

    let plan = catalog.resolve(&roots(&["chat.app"])).unwrap();
    assert_eq!(plan.scope, session_scope());
    assert!(!plan.ordered_plugins.is_empty());
    assert!(!plan.bindings.is_empty());
    assert_eq!(
        plugin_ids(&plan),
        vec!["chat.core", "chat.app"],
        "provider must be ordered before its consumer"
    );
    assert_eq!(
        plan.bindings,
        vec![DependencyBinding {
            consumer_id: "chat.app".to_string(),
            requirement: svc("chat.history", (1, 0, 0)),
            provider_id: "chat.core".to_string(),
            provider_version: v(0, 5, 0),
        }]
    );
    assert_eq!(plan.ordered_plugins[0].version, v(0, 5, 0));
    assert_eq!(plan.ordered_plugins[1].version, v(2, 0, 0));
}

/// D02: chain + diamond + multiple roots + multiple requirements bound to one
/// provider. Shared nodes appear once; multi-interface bindings to the same
/// provider collapse to one topology edge while keeping per-requirement
/// bindings. Reversed registration order and reversed/duplicated roots yield
/// an identical plan.
#[test]
fn d02_graph_orders_deterministically_and_dedups() {
    //       top                 chain-c          multi
    //      /   \                   |                |
    //   mid-a  mid-b           chain-b          (x-part, y-part)
    //      \   /                   |                |
    //      base                chain-a            both
    let members = [
        session_manifest(
            "top",
            (1, 0, 0),
            vec![],
            vec![svc("a.out", (1, 0, 0)), svc("b.out", (1, 0, 0))],
        ),
        session_manifest(
            "mid-a",
            (1, 0, 0),
            vec![svc("a.out", (1, 0, 0))],
            vec![svc("shared", (1, 0, 0))],
        ),
        session_manifest(
            "mid-b",
            (1, 0, 0),
            vec![svc("b.out", (1, 0, 0))],
            vec![svc("shared", (1, 0, 0))],
        ),
        session_manifest("base", (1, 0, 0), vec![svc("shared", (1, 0, 0))], vec![]),
        session_manifest(
            "chain-c",
            (1, 0, 0),
            vec![],
            vec![svc("mid.link", (1, 0, 0))],
        ),
        session_manifest(
            "chain-b",
            (1, 0, 0),
            vec![svc("mid.link", (1, 0, 0))],
            vec![svc("root.link", (1, 0, 0))],
        ),
        session_manifest(
            "chain-a",
            (1, 0, 0),
            vec![svc("root.link", (1, 0, 0))],
            vec![],
        ),
        session_manifest(
            "multi",
            (1, 0, 0),
            vec![],
            vec![svc("x.part", (1, 0, 0)), svc("y.part", (1, 0, 0))],
        ),
        session_manifest(
            "both",
            (1, 0, 0),
            vec![svc("x.part", (1, 0, 0)), svc("y.part", (1, 0, 0))],
            vec![],
        ),
    ];

    let mut catalog = session_catalog();
    for member in &members {
        catalog.register(member.clone()).unwrap();
    }
    let plan = catalog
        .resolve(&roots(&["top", "chain-c", "multi"]))
        .unwrap();

    assert_eq!(
        plugin_ids(&plan),
        vec![
            "base", "both", "chain-a", "chain-b", "chain-c", "mid-a", "mid-b", "multi", "top",
        ],
        "ready nodes must emit by ascending id with providers first"
    );
    // Two requirements of `multi` bound to `both` stay two bindings, but the
    // single deduplicated topology edge is what lets `multi` resolve cleanly.
    let multi_bindings: Vec<_> = plan
        .bindings
        .iter()
        .filter(|binding| binding.consumer_id == "multi")
        .collect();
    assert_eq!(multi_bindings.len(), 2);
    assert!(multi_bindings.iter().all(|b| b.provider_id == "both"));
    let mut multi_reqs: Vec<&str> = multi_bindings
        .iter()
        .map(|binding| binding.requirement.name.as_str())
        .collect();
    multi_reqs.sort_unstable();
    assert_eq!(multi_reqs, ["x.part", "y.part"]);
    assert_eq!(
        plan.bindings.len(),
        8,
        "one binding per requirement across the whole closure"
    );
    assert_eq!(
        plan.ordered_plugins
            .iter()
            .filter(|plugin| plugin.id == "both")
            .count(),
        1,
        "shared provider appears exactly once"
    );

    // Input order invariance: reversed registration, reversed roots, and
    // duplicated roots all produce the identical plan.
    let mut reversed = session_catalog();
    for member in members.iter().rev() {
        reversed.register(member.clone()).unwrap();
    }
    let reversed_plan = reversed
        .resolve(&roots(&["multi", "chain-c", "top"]))
        .unwrap();
    assert_eq!(plan, reversed_plan);
    let duplicated_roots = catalog
        .resolve(&roots(&["multi", "top", "top", "chain-c", "multi"]))
        .unwrap();
    assert_eq!(plan, duplicated_roots);

    // A single root resolves only its own transitive closure.
    let top_only = catalog.resolve(&roots(&["top"])).unwrap();
    assert_eq!(plugin_ids(&top_only), vec!["base", "mid-a", "mid-b", "top"]);
}

/// D03: missing vs version-mismatch vs kind mismatch are distinct.
#[test]
fn d03_missing_version_and_kind_are_distinct() {
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "ver-older",
            (1, 0, 0),
            vec![svc("api", (0, 9, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "ver-old",
            (1, 0, 0),
            vec![svc("api", (1, 0, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "ver-old2",
            (1, 0, 0),
            vec![svc("api", (1, 0, 0))],
            vec![],
        ))
        .unwrap();
    // Same name and same version, but a different kind — must not satisfy.
    catalog
        .register(session_manifest(
            "kind-other",
            (1, 0, 0),
            vec![ikey(InterfaceKind::Query, "api", (2, 0, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "needs",
            (1, 0, 0),
            vec![],
            vec![svc("api", (2, 0, 0))],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "needs-miss",
            (1, 0, 0),
            vec![],
            vec![svc("ghost", (1, 0, 0))],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "needs-kind",
            (1, 0, 0),
            vec![],
            vec![ikey(InterfaceKind::Event, "api", (1, 0, 0))],
        ))
        .unwrap();

    match catalog.resolve(&roots(&["needs"])) {
        Err(CatalogError::VersionMismatch {
            consumer_id,
            requirement,
            available,
        }) => {
            assert_eq!(consumer_id, "needs");
            assert_eq!(requirement, svc("api", (2, 0, 0)));
            assert_eq!(
                available,
                vec![v(0, 9, 0), v(1, 0, 0)],
                "available versions sorted and deduplicated"
            );
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
    match catalog.resolve(&roots(&["needs-miss"])) {
        Err(CatalogError::MissingDependency {
            consumer_id,
            requirement,
        }) => {
            assert_eq!(consumer_id, "needs-miss");
            assert_eq!(requirement, svc("ghost", (1, 0, 0)));
        }
        other => panic!("expected MissingDependency, got {other:?}"),
    }
    // Different kind at the required name+version is a miss, not a mismatch.
    match catalog.resolve(&roots(&["needs-kind"])) {
        Err(CatalogError::MissingDependency {
            consumer_id,
            requirement,
        }) => {
            assert_eq!(consumer_id, "needs-kind");
            assert_eq!(requirement, ikey(InterfaceKind::Event, "api", (1, 0, 0)));
        }
        other => panic!("expected MissingDependency for kind mismatch, got {other:?}"),
    }
}

/// D04: multiple exact providers are ambiguous with sorted ids (never a
/// first-registered pick); one exact plus one wrong-version provider binds
/// the exact one.
#[test]
fn d04_ambiguous_provider_lists_sorted_ids() {
    let make = |catalog: &mut PluginCatalog, order: &[&str]| {
        for id in order {
            catalog
                .register(session_manifest(
                    id,
                    (1, 0, 0),
                    vec![svc("dup", (1, 0, 0))],
                    vec![],
                ))
                .unwrap();
        }
        catalog
            .register(session_manifest(
                "needs-dup",
                (1, 0, 0),
                vec![],
                vec![svc("dup", (1, 0, 0))],
            ))
            .unwrap();
    };
    let mut forward = session_catalog();
    make(&mut forward, &["prov-a", "prov-b"]);
    let mut backward = session_catalog();
    make(&mut backward, &["prov-b", "prov-a"]);
    for catalog in [&forward, &backward] {
        match catalog.resolve(&roots(&["needs-dup"])) {
            Err(CatalogError::AmbiguousProvider {
                consumer_id,
                requirement,
                providers,
            }) => {
                assert_eq!(consumer_id, "needs-dup");
                assert_eq!(requirement, svc("dup", (1, 0, 0)));
                assert_eq!(providers, ["prov-a", "prov-b"]);
            }
            other => panic!("expected AmbiguousProvider, got {other:?}"),
        }
    }

    // One exact + one wrong-version candidate resolves to the exact provider.
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "good",
            (1, 0, 0),
            vec![svc("pick", (1, 0, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "wrong",
            (1, 0, 0),
            vec![svc("pick", (9, 9, 9))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "picker",
            (1, 0, 0),
            vec![],
            vec![svc("pick", (1, 0, 0))],
        ))
        .unwrap();
    let plan = catalog.resolve(&roots(&["picker"])).unwrap();
    assert_eq!(plugin_ids(&plan), vec!["good", "picker"]);
    assert_eq!(
        plan.bindings,
        vec![DependencyBinding {
            consumer_id: "picker".to_string(),
            requirement: svc("pick", (1, 0, 0)),
            provider_id: "good".to_string(),
            provider_version: v(1, 0, 0),
        }],
        "the wrong-version provider must neither bind nor enter the closure"
    );
}

/// D05: self-loop, two-node cycle, off-cycle dependents, and duplicate edges.
#[test]
fn d05_cycles_report_real_closed_paths_only() {
    // Self loop: unique provider of its own requirement.
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "loopy",
            (1, 0, 0),
            vec![svc("spin", (1, 0, 0))],
            vec![svc("spin", (1, 0, 0))],
        ))
        .unwrap();
    match catalog.resolve(&roots(&["loopy"])) {
        Err(CatalogError::DependencyCycle { path }) => {
            assert_eq!(path, ["loopy", "loopy"]);
            assert_closed_dep_path(&path, &[("loopy", "loopy")]);
        }
        other => panic!("expected DependencyCycle for self loop, got {other:?}"),
    }

    // pa <-> pb, plus x which only consumes the cycle from outside.
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "pa",
            (1, 0, 0),
            vec![svc("from-a", (1, 0, 0))],
            vec![svc("from-b", (1, 0, 0))],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "pb",
            (1, 0, 0),
            vec![svc("from-b", (1, 0, 0))],
            vec![svc("from-a", (1, 0, 0))],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "x",
            (1, 0, 0),
            vec![],
            vec![svc("from-a", (1, 0, 0))],
        ))
        .unwrap();
    let edges = [("pa", "pb"), ("pb", "pa"), ("x", "pa")];
    for root in ["pa", "x"] {
        match catalog.resolve(&roots(&[root])) {
            Err(CatalogError::DependencyCycle { path }) => {
                assert_eq!(path, ["pa", "pb", "pa"]);
                assert_closed_dep_path(&path, &edges);
                assert!(
                    !path[..path.len() - 1].iter().any(|id| id == "x"),
                    "off-cycle node must not be reported as part of the cycle"
                );
            }
            other => panic!("expected DependencyCycle for root {root}, got {other:?}"),
        }
    }

    // Two requirements bound to the same provider are one edge, not a cycle.
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "hub",
            (1, 0, 0),
            vec![svc("part.one", (1, 0, 0)), svc("part.two", (1, 0, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "dup-edge",
            (1, 0, 0),
            vec![],
            vec![svc("part.one", (1, 0, 0)), svc("part.two", (1, 0, 0))],
        ))
        .unwrap();
    let plan = catalog.resolve(&roots(&["dup-edge"])).unwrap();
    assert_eq!(plugin_ids(&plan), vec!["hub", "dup-edge"]);
    assert_eq!(plan.bindings.len(), 2);
}

/// D06: isolated broken plugins cannot pollute a healthy root; empty,
/// duplicated, unknown, and malformed roots each follow the contract; multiple
/// failures report the first error in deterministic traversal order.
#[test]
fn d06_reachability_and_root_rules() {
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "ok-base",
            (1, 0, 0),
            vec![svc("ok.iface", (1, 0, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "healthy",
            (1, 0, 0),
            vec![],
            vec![svc("ok.iface", (1, 0, 0))],
        ))
        .unwrap();
    // Broken members that stay unreachable from `healthy`.
    catalog
        .register(session_manifest(
            "bad-miss",
            (1, 0, 0),
            vec![],
            vec![svc("nothing", (1, 0, 0))],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "dup-a",
            (1, 0, 0),
            vec![svc("duped", (1, 0, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "dup-b",
            (1, 0, 0),
            vec![svc("duped", (1, 0, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "bad-amb",
            (1, 0, 0),
            vec![],
            vec![svc("duped", (1, 0, 0))],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "cyc-a",
            (1, 0, 0),
            vec![svc("ca", (1, 0, 0))],
            vec![svc("cb", (1, 0, 0))],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "cyc-b",
            (1, 0, 0),
            vec![svc("cb", (1, 0, 0))],
            vec![svc("ca", (1, 0, 0))],
        ))
        .unwrap();

    let plan = catalog.resolve(&roots(&["healthy"])).unwrap();
    assert_eq!(plugin_ids(&plan), vec!["ok-base", "healthy"]);
    assert_eq!(plan.bindings.len(), 1);

    // The broken members are real and diagnosed when selected directly.
    match catalog.resolve(&roots(&["bad-miss"])) {
        Err(CatalogError::MissingDependency {
            consumer_id,
            requirement,
        }) => {
            assert_eq!(consumer_id, "bad-miss");
            assert_eq!(requirement, svc("nothing", (1, 0, 0)));
        }
        other => panic!("expected MissingDependency for bad-miss, got {other:?}"),
    }
    match catalog.resolve(&roots(&["bad-amb"])) {
        Err(CatalogError::AmbiguousProvider {
            consumer_id,
            providers,
            ..
        }) => {
            assert_eq!(consumer_id, "bad-amb");
            assert_eq!(providers, ["dup-a", "dup-b"]);
        }
        other => panic!("expected AmbiguousProvider for bad-amb, got {other:?}"),
    }
    match catalog.resolve(&roots(&["cyc-a"])) {
        Err(CatalogError::DependencyCycle { path }) => {
            assert_eq!(path, ["cyc-a", "cyc-b", "cyc-a"]);
        }
        other => panic!("expected DependencyCycle for cyc-a, got {other:?}"),
    }

    // Empty roots: empty plan, still scoped.
    let empty = catalog.resolve(&[]).unwrap();
    assert_eq!(empty.scope, session_scope());
    assert!(empty.ordered_plugins.is_empty());
    assert!(empty.bindings.is_empty());

    // Duplicate roots dedupe to the same plan.
    assert_eq!(
        plan,
        catalog.resolve(&roots(&["healthy", "healthy"])).unwrap()
    );

    // Well-formed but unregistered, and malformed root ids.
    assert_eq!(
        catalog.resolve(&roots(&["ghost"])),
        Err(CatalogError::UnknownRoot {
            id: "ghost".to_string()
        })
    );
    assert!(matches!(
        catalog.resolve(&roots(&["NOT A PLUGIN"])),
        Err(CatalogError::InvalidManifest { .. })
    ));

    // Deterministic first error: roots processed by ascending id.
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "z-miss",
            (1, 0, 0),
            vec![],
            vec![svc("gone", (1, 0, 0))],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "a-miss",
            (1, 0, 0),
            vec![],
            vec![svc("gone", (1, 0, 0))],
        ))
        .unwrap();
    match catalog.resolve(&roots(&["z-miss", "a-miss"])) {
        Err(CatalogError::MissingDependency { consumer_id, .. }) => {
            assert_eq!(
                consumer_id, "a-miss",
                "first failure follows ascending root id"
            )
        }
        other => panic!("expected MissingDependency, got {other:?}"),
    }

    // Across error classes too: a smaller root's closure failure precedes a
    // larger unregistered root's UnknownRoot, and vice versa.
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "a-miss",
            (1, 0, 0),
            vec![],
            vec![svc("gone", (1, 0, 0))],
        ))
        .unwrap();
    match catalog.resolve(&roots(&["z-ghost", "a-miss"])) {
        Err(CatalogError::MissingDependency { consumer_id, .. }) => {
            assert_eq!(consumer_id, "a-miss")
        }
        other => {
            panic!("expected a-miss MissingDependency before z-ghost UnknownRoot, got {other:?}")
        }
    }
    match catalog.resolve(&roots(&["a-miss", "0-ghost"])) {
        Err(CatalogError::UnknownRoot { id }) => assert_eq!(id, "0-ghost"),
        other => panic!("expected 0-ghost UnknownRoot before a-miss failure, got {other:?}"),
    }
}

const BASE: &str = r#"{
    "manifest_version": 1,
    "id": "ok.plugin",
    "display_name": "OK Plugin",
    "version": {"major": 1, "minor": 2, "patch": 3},
    "scope": "session",
    "runtime": "builtin",
    "lifecycle": "host_managed",
    "surfaces": ["cli"],
    "permissions": [],
    "config_schema": {},
    "provides": [],
    "requires": []
}"#;

fn expect_invalid(result: Result<CatalogManifestV1, CatalogError>, label: &str) {
    match result {
        Err(CatalogError::InvalidManifest { .. }) => {}
        other => panic!("expected InvalidManifest for {label}, got {other:?}"),
    }
}

/// D07a: malformed ids/names and boundary-legal values.
#[test]
fn d07_id_and_name_rules_with_boundaries() {
    let parse_id = |id: &str| {
        parse_manifest(&BASE.replace("\"id\": \"ok.plugin\"", &format!("\"id\": {id:?}")))
    };
    for bad in [
        "",
        "A",
        ".a",
        "-a",
        "_a",
        "a b",
        "a/b",
        "a:b",
        "a中",
        "x".repeat(65).as_str(),
    ] {
        expect_invalid(parse_id(bad), "bad plugin id");
    }
    for ok in ["a", "9lives", "a.b-c_d", "x".repeat(64).as_str()] {
        assert!(parse_id(ok).is_ok(), "{ok:?}");
    }

    // Interface names follow the same charset at 1..=128 bytes.
    let bad_name = BASE.replace(
        "\"provides\": []",
        "\"provides\": [{\"kind\": \"service\", \"name\": \"Bad Name\", \"version\": {\"major\": 1, \"minor\": 0, \"patch\": 0}}]",
    );
    expect_invalid(parse_manifest(&bad_name), "bad interface name");
    let long_name = BASE.replace(
        "\"provides\": []",
        &format!(
            "\"provides\": [{{\"kind\": \"service\", \"name\": {:?}, \"version\": {{\"major\": 1, \"minor\": 0, \"patch\": 0}}}}]",
            "n".repeat(129)
        ),
    );
    expect_invalid(parse_manifest(&long_name), "129-byte interface name");
    let ok_name = BASE.replace(
        "\"provides\": []",
        &format!(
            "\"provides\": [{{\"kind\": \"service\", \"name\": {:?}, \"version\": {{\"major\": 1, \"minor\": 0, \"patch\": 0}}}}]",
            "n".repeat(128)
        ),
    );
    assert!(parse_manifest(&ok_name).is_ok());

    // display_name boundary: 200 UTF-8 bytes passes, 201 fails.
    let display_201 = BASE.replace(
        "\"display_name\": \"OK Plugin\"",
        &format!("\"display_name\": {:?}", "界".repeat(67)),
    );
    expect_invalid(parse_manifest(&display_201), "201-byte display_name");
    // Exactly 200 bytes: 66 * 3-byte chars + 2 ASCII.
    let display_200 = BASE.replace(
        "\"display_name\": \"OK Plugin\"",
        &format!("\"display_name\": {:?}", "界".repeat(66) + "xx"),
    );
    let manifest = parse_manifest(&display_200).unwrap();
    assert_eq!(manifest.display_name.len(), 200);
}

/// D07b: unknown/missing fields, wrong types, integer overflow, unsupported
/// versions, non-object config_schema, duplicate declared values.
#[test]
fn d07_structure_type_and_duplicate_declarations() {
    // Unknown and missing members.
    let unknown = BASE.replace(
        "\"manifest_version\": 1,",
        "\"manifest_version\": 1,\n    \"surprise\": true,",
    );
    match parse_manifest(&unknown) {
        Err(CatalogError::InvalidManifest { path, .. }) => {
            assert_eq!(path, "surprise", "unknown-field error must name the member")
        }
        other => panic!("expected InvalidManifest for unknown field, got {other:?}"),
    }
    for field in [
        "manifest_version",
        "id",
        "display_name",
        "version",
        "scope",
        "runtime",
        "lifecycle",
        "surfaces",
        "permissions",
        "config_schema",
        "provides",
        "requires",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(BASE).unwrap();
        value.as_object_mut().unwrap().remove(field);
        match parse_manifest(&value.to_string()) {
            Err(CatalogError::InvalidManifest { path, .. }) => {
                assert_eq!(path, field, "missing-field error must name the field")
            }
            other => panic!("expected InvalidManifest for missing {field}, got {other:?}"),
        }
    }

    // Nested objects get nested paths: extra member and missing member.
    let nested_unknown = BASE.replace(
        "\"version\": {\"major\": 1, \"minor\": 2, \"patch\": 3}",
        "\"version\": {\"major\": 1, \"minor\": 2, \"patch\": 3, \"extra\": 0}",
    );
    match parse_manifest(&nested_unknown) {
        Err(CatalogError::InvalidManifest { path, .. }) => {
            assert_eq!(path, "version.extra")
        }
        other => panic!("expected InvalidManifest for nested unknown field, got {other:?}"),
    }
    let nested_missing = BASE.replace(
        "\"version\": {\"major\": 1, \"minor\": 2, \"patch\": 3}",
        "\"version\": {\"major\": 1, \"minor\": 2}",
    );
    match parse_manifest(&nested_missing) {
        Err(CatalogError::InvalidManifest { path, .. }) => {
            assert_eq!(path, "version.patch")
        }
        other => panic!("expected InvalidManifest for nested missing field, got {other:?}"),
    }

    // manifest_version must be the integer 1.
    let set_version = |v: &str| {
        BASE.replace(
            "\"manifest_version\": 1",
            &format!("\"manifest_version\": {v}"),
        )
    };
    for bad in ["\"1\"", "1.5", "-1", "18446744073709551616"] {
        expect_invalid(
            parse_manifest(&set_version(bad)),
            "non-integer manifest_version",
        );
    }
    for wrong in ["0", "2"] {
        assert!(matches!(
            parse_manifest(&set_version(wrong)),
            Err(CatalogError::UnsupportedManifestVersion { .. })
        ));
    }

    // Integer overflow inside a version triple.
    let overflow = BASE.replace("\"major\": 1", "\"major\": 4294967296");
    expect_invalid(parse_manifest(&overflow), "u32 overflow");

    // Non-object config_schema.
    for bad in ["[1, 2]", "\"text\"", "42"] {
        let replaced = BASE.replace(
            "\"config_schema\": {}",
            &format!("\"config_schema\": {bad}"),
        );
        expect_invalid(parse_manifest(&replaced), "non-object config_schema");
    }

    // Unknown enum values.
    for (field, bad) in [
        ("scope", "\"universe\""),
        ("runtime", "\"telepathy\""),
        ("lifecycle", "\"self_managed\""),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(BASE).unwrap();
        *value.get_mut(field).unwrap() = serde_json::from_str(bad).unwrap();
        expect_invalid(parse_manifest(&value.to_string()), "unknown enum value");
    }

    // surfaces: empty, duplicate, unknown element.
    let set = |surfaces: &str| {
        BASE.replace(
            "\"surfaces\": [\"cli\"]",
            &format!("\"surfaces\": {surfaces}"),
        )
    };
    expect_invalid(parse_manifest(&set("[]")), "empty surfaces");
    expect_invalid(
        parse_manifest(&set("[\"cli\", \"cli\"]")),
        "duplicate surface",
    );
    expect_invalid(parse_manifest(&set("[\"hologram\"]")), "unknown surface");
    assert!(parse_manifest(&set("[\"cli\", \"desktop\", \"headless\"]")).is_ok());

    // Duplicate permission names and invalid permission name.
    let perms = |p: &str| BASE.replace("\"permissions\": []", &format!("\"permissions\": {p}"));
    expect_invalid(
        parse_manifest(&perms("[\"fs.read\", \"fs.read\"]")),
        "duplicate permission",
    );
    expect_invalid(
        parse_manifest(&perms("[\"Bad Perm\"]")),
        "invalid permission name",
    );

    // Fully identical provides/requires keys are rejected.
    let dup_provides = BASE.replace(
        "\"provides\": []",
        "\"provides\": [{\"kind\": \"service\", \"name\": \"s\", \"version\": {\"major\": 1, \"minor\": 0, \"patch\": 0}}, {\"kind\": \"service\", \"name\": \"s\", \"version\": {\"major\": 1, \"minor\": 0, \"patch\": 0}}]",
    );
    expect_invalid(parse_manifest(&dup_provides), "duplicate provides key");
    let dup_requires = BASE.replace(
        "\"requires\": []",
        "\"requires\": [{\"kind\": \"query\", \"name\": \"q\", \"version\": {\"major\": 1, \"minor\": 0, \"patch\": 0}}, {\"kind\": \"query\", \"name\": \"q\", \"version\": {\"major\": 1, \"minor\": 0, \"patch\": 0}}]",
    );
    expect_invalid(parse_manifest(&dup_requires), "duplicate requires key");
    // Same kind+name at different versions is allowed to coexist.
    let two_versions = BASE.replace(
        "\"provides\": []",
        "\"provides\": [{\"kind\": \"service\", \"name\": \"s\", \"version\": {\"major\": 1, \"minor\": 0, \"patch\": 0}}, {\"kind\": \"service\", \"name\": \"s\", \"version\": {\"major\": 2, \"minor\": 0, \"patch\": 0}}]",
    );
    assert!(parse_manifest(&two_versions).is_ok());
}

/// D07c: duplicate JSON member names are rejected at every object level,
/// including inside config_schema and inside arrays. The error carries the
/// field path of the repeated member — except inside config_schema, whose
/// member names are open content and stay out of the path.
#[test]
fn d07_duplicate_json_members_rejected_everywhere() {
    fn dup_path(result: Result<CatalogManifestV1, CatalogError>) -> String {
        match result {
            Err(CatalogError::InvalidManifest { path, .. }) => path,
            other => panic!("expected InvalidManifest, got {other:?}"),
        }
    }
    let dup_root = BASE.replace(
        "\"id\": \"ok.plugin\"",
        "\"id\": \"ok.plugin\", \"id\": \"other.id\"",
    );
    assert_eq!(dup_path(parse_manifest(&dup_root)), "id");

    let dup_config = BASE.replace(
        "\"config_schema\": {}",
        "\"config_schema\": {\"a\": 1, \"a\": 2}",
    );
    assert_eq!(dup_path(parse_manifest(&dup_config)), "config_schema");

    let dup_nested = BASE.replace(
        "\"config_schema\": {}",
        "\"config_schema\": {\"items\": [{\"x\": 1, \"x\": 2}], \"more\": [{\"y\": 1}, {\"y\": 1, \"y\": 2}]}",
    );
    assert_eq!(dup_path(parse_manifest(&dup_nested)), "config_schema");

    let dup_key_object = BASE.replace(
        "\"provides\": []",
        "\"provides\": [{\"kind\": \"service\", \"kind\": \"query\", \"name\": \"s\", \"version\": {\"major\": 1, \"minor\": 0, \"patch\": 0}}]",
    );
    assert_eq!(
        dup_path(parse_manifest(&dup_key_object)),
        "provides[0].kind"
    );

    // Whole input must be JSON and a single document.
    expect_invalid(parse_manifest("not json"), "non-JSON input");
    expect_invalid(parse_manifest("[1, 2]"), "non-object manifest");
    expect_invalid(parse_manifest(&format!("{BASE} {{}}")), "trailing content");
}

/// D07d: open config_schema accepts arbitrary member names and nesting.
#[test]
fn d07_open_config_schema_and_boundaries() {
    let open = BASE.replace(
        "\"config_schema\": {}",
        "\"config_schema\": {\"type\": \"object\", \"properties\": {\"anything goes\": {\"x-cust.om\": [1, {\"deep\": null}]}}, \"required\": []}",
    );
    let manifest = parse_manifest(&open).unwrap();
    assert!(manifest.config_schema.is_object());

    // Empty arrays are legal for permissions/provides/requires.
    assert!(parse_manifest(BASE).is_ok());
}

/// D07e: directly constructed structs are fully re-validated by register, and
/// a failed register leaves the catalog byte-identical.
#[test]
fn d07_register_revalidates_direct_constructs_atomically() {
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest("seeded", (1, 0, 0), vec![], vec![]))
        .unwrap();
    let before: Vec<CatalogManifestV1> = catalog.descriptors().into_iter().cloned().collect();

    let mut bad = session_manifest("second", (1, 0, 0), vec![], vec![]);
    bad.id = "BAD ID".to_string();
    assert!(matches!(
        catalog.register(bad),
        Err(CatalogError::InvalidManifest { .. })
    ));

    let mut bad_version = session_manifest("third", (1, 0, 0), vec![], vec![]);
    bad_version.manifest_version = 2;
    assert!(matches!(
        catalog.register(bad_version),
        Err(CatalogError::UnsupportedManifestVersion { .. })
    ));

    let mut bad_scope = session_manifest("fourth", (1, 0, 0), vec![], vec![]);
    bad_scope.scope = ScopeKind::Host;
    assert!(matches!(
        catalog.register(bad_scope),
        Err(CatalogError::ScopeMismatch { .. })
    ));

    let mut bad_schema = session_manifest("fifth", (1, 0, 0), vec![], vec![]);
    bad_schema.config_schema = json!(["not", "an", "object"]);
    assert!(matches!(
        catalog.register(bad_schema),
        Err(CatalogError::InvalidManifest { .. })
    ));

    let mut dup_key = session_manifest("dup.key", (1, 0, 0), vec![], vec![]);
    dup_key.provides = vec![svc("dup.k", (1, 0, 0)), svc("dup.k", (1, 0, 0))];
    assert!(
        matches!(
            catalog.register(dup_key),
            Err(CatalogError::InvalidManifest { .. })
        ),
        "duplicate declared interface key must be revalidated at register"
    );

    let after: Vec<CatalogManifestV1> = catalog.descriptors().into_iter().cloned().collect();
    assert_eq!(before, after, "failed register must not mutate the catalog");
}

/// D07f: scope identity validation on catalog construction.
#[test]
fn d07_scope_identity_is_validated() {
    assert!(PluginCatalog::new(ScopeKey::Host).is_ok());
    for bad in [
        "",
        "has space",
        "has\ttab",
        "xai-secretvalue",
        "p".repeat(129).as_str(),
    ] {
        assert!(matches!(
            PluginCatalog::new(ScopeKey::Project {
                project_id: bad.to_string()
            }),
            Err(CatalogError::InvalidScope { .. })
        ));
        assert!(matches!(
            PluginCatalog::new(ScopeKey::Session {
                project_id: "prj-ok".to_string(),
                session_id: bad.to_string()
            }),
            Err(CatalogError::InvalidScope { .. })
        ));
        assert!(
            matches!(
                PluginCatalog::new(ScopeKey::Session {
                    project_id: bad.to_string(),
                    session_id: "ses-ok".to_string()
                }),
                Err(CatalogError::InvalidScope { .. })
            ),
            "bad session project_id must also be rejected"
        );
    }
    // Boundary-legal scope ids are accepted (128 bytes each).
    assert!(
        PluginCatalog::new(ScopeKey::Session {
            project_id: "p".repeat(128),
            session_id: "s".repeat(128)
        })
        .is_ok()
    );
}

/// D08: same id always collides regardless of version; display_name is not
/// identity.
#[test]
fn d08_duplicate_id_rules() {
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest("dup.id", (1, 0, 0), vec![], vec![]))
        .unwrap();
    let seeded: Vec<CatalogManifestV1> = catalog.descriptors().into_iter().cloned().collect();

    for version in [(1, 0, 0), (9, 9, 9)] {
        match catalog.register(session_manifest("dup.id", version, vec![], vec![])) {
            Err(CatalogError::DuplicatePlugin { id }) => assert_eq!(id, "dup.id"),
            other => panic!("expected DuplicatePlugin for version {version:?}, got {other:?}"),
        }
    }
    // Original entry byte-identical after both rejections.
    let after: Vec<CatalogManifestV1> = catalog.descriptors().into_iter().cloned().collect();
    assert_eq!(
        seeded, after,
        "rejected duplicates must not mutate the catalog"
    );

    // Same display_name, different ids: both register.
    let mut a = session_manifest("first.id", (1, 0, 0), vec![], vec![]);
    a.display_name = "Shared Name".to_string();
    let mut b = session_manifest("second.id", (1, 0, 0), vec![], vec![]);
    b.display_name = "Shared Name".to_string();
    catalog.register(a).unwrap();
    catalog.register(b).unwrap();
    assert_eq!(catalog.descriptors().len(), 3);
}

/// D09: scope kinds are enforced and catalog instances never satisfy each
/// other's requirements.
#[test]
fn d09_scope_kinds_and_instance_isolation() {
    let mut project_catalog = PluginCatalog::new(ScopeKey::Project {
        project_id: "prj-one".to_string(),
    })
    .unwrap();
    assert!(matches!(
        project_catalog.register(session_manifest("wrong.kind", (1, 0, 0), vec![], vec![])),
        Err(CatalogError::ScopeMismatch {
            expected: ScopeKind::Project,
            found: ScopeKind::Session,
        })
    ));
    assert!(matches!(
        project_catalog.register(manifest(
            "host.kind",
            (1, 0, 0),
            ScopeKind::Host,
            vec![],
            vec![]
        )),
        Err(CatalogError::ScopeMismatch {
            expected: ScopeKind::Project,
            found: ScopeKind::Host,
        })
    ));
    project_catalog
        .register(manifest(
            "right.kind",
            (1, 0, 0),
            ScopeKind::Project,
            vec![],
            vec![],
        ))
        .unwrap();

    // Two project catalogs: same interface name provided only in the other
    // instance must not satisfy this instance's consumer.
    let mut p1 = PluginCatalog::new(ScopeKey::Project {
        project_id: "prj-one".to_string(),
    })
    .unwrap();
    p1.register(manifest(
        "prov.one",
        (1, 0, 0),
        ScopeKind::Project,
        vec![svc("shared.cap", (1, 0, 0))],
        vec![],
    ))
    .unwrap();
    let mut p2 = PluginCatalog::new(ScopeKey::Project {
        project_id: "prj-two".to_string(),
    })
    .unwrap();
    p2.register(manifest(
        "need",
        (1, 0, 0),
        ScopeKind::Project,
        vec![],
        vec![svc("shared.cap", (1, 0, 0))],
    ))
    .unwrap();
    assert!(
        matches!(
            p2.resolve(&roots(&["need"])),
            Err(CatalogError::MissingDependency { .. })
        ),
        "a provider in another catalog instance must not satisfy requirements"
    );

    // Registering p2's own provider fixes p2 only; removing it later does not
    // touch p1.
    p2.register(manifest(
        "prov.two",
        (1, 0, 0),
        ScopeKind::Project,
        vec![svc("shared.cap", (1, 0, 0))],
        vec![],
    ))
    .unwrap();
    let plan = p2.resolve(&roots(&["need"])).unwrap();
    assert_eq!(plan.bindings[0].provider_id, "prov.two");
    // p1 cannot even address p2's descriptor by id.
    let p1_before: Vec<CatalogManifestV1> = p1.descriptors().into_iter().cloned().collect();
    assert!(matches!(
        p1.remove_descriptor("prov.two"),
        Err(CatalogError::UnknownPlugin { .. })
    ));
    p2.remove_descriptor("prov.two").unwrap();
    assert!(matches!(
        p2.resolve(&roots(&["need"])),
        Err(CatalogError::MissingDependency { .. })
    ));
    let p1_after: Vec<CatalogManifestV1> = p1.descriptors().into_iter().cloned().collect();
    assert_eq!(p1_before, p1_after, "p1 must be unaffected by p2");

    // Two Session catalogs on the same project are equally isolated.
    let mut s1 = PluginCatalog::new(ScopeKey::Session {
        project_id: "prj-one".to_string(),
        session_id: "ses-one".to_string(),
    })
    .unwrap();
    s1.register(manifest(
        "ses.prov",
        (1, 0, 0),
        ScopeKind::Session,
        vec![svc("shared.cap", (1, 0, 0))],
        vec![],
    ))
    .unwrap();
    let mut s2 = PluginCatalog::new(ScopeKey::Session {
        project_id: "prj-one".to_string(),
        session_id: "ses-two".to_string(),
    })
    .unwrap();
    s2.register(manifest(
        "ses.need",
        (1, 0, 0),
        ScopeKind::Session,
        vec![],
        vec![svc("shared.cap", (1, 0, 0))],
    ))
    .unwrap();
    assert!(
        matches!(
            s2.resolve(&roots(&["ses.need"])),
            Err(CatalogError::MissingDependency { .. })
        ),
        "a provider in another session catalog must not satisfy requirements"
    );
}

/// D10: removing a descriptor makes the next resolve report the dependency as
/// missing; a replacement provider is a new binding, never a display-name
/// continuation.
#[test]
fn d10_remove_and_replace_provider() {
    let mut catalog = session_catalog();
    catalog
        .register(session_manifest(
            "old-prov",
            (1, 0, 0),
            vec![svc("iface", (1, 0, 0))],
            vec![],
        ))
        .unwrap();
    catalog
        .register(session_manifest(
            "app",
            (1, 0, 0),
            vec![],
            vec![svc("iface", (1, 0, 0))],
        ))
        .unwrap();

    let old_plan = catalog.resolve(&roots(&["app"])).unwrap();
    assert_eq!(plugin_ids(&old_plan), vec!["old-prov", "app"]);
    assert_eq!(
        old_plan.bindings,
        vec![DependencyBinding {
            consumer_id: "app".to_string(),
            requirement: svc("iface", (1, 0, 0)),
            provider_id: "old-prov".to_string(),
            provider_version: v(1, 0, 0),
        }]
    );

    let removed = catalog.remove_descriptor("old-prov").unwrap();
    assert_eq!(removed.id, "old-prov");
    match catalog.resolve(&roots(&["app"])) {
        Err(CatalogError::MissingDependency { consumer_id, .. }) => {
            assert_eq!(consumer_id, "app");
        }
        other => panic!("expected MissingDependency after provider removal, got {other:?}"),
    }

    // Same display_name, new id: rebinding targets the new stable id.
    let mut replacement =
        session_manifest("new-prov", (3, 1, 0), vec![svc("iface", (1, 0, 0))], vec![]);
    replacement.display_name = "old-prov plugin".to_string();
    catalog.register(replacement).unwrap();
    let new_plan = catalog.resolve(&roots(&["app"])).unwrap();
    assert_eq!(new_plan.bindings[0].provider_id, "new-prov");
    assert_eq!(new_plan.bindings[0].provider_version, v(3, 1, 0));
    assert_eq!(plugin_ids(&new_plan), vec!["new-prov", "app"]);
    // The earlier plan is an immutable historical value: fully unchanged.
    assert_eq!(old_plan.bindings[0].provider_id, "old-prov");
    assert_eq!(plugin_ids(&old_plan), vec!["old-prov", "app"]);

    // Unknown removal is a typed no-op.
    let before: Vec<CatalogManifestV1> = catalog.descriptors().into_iter().cloned().collect();
    assert!(matches!(
        catalog.remove_descriptor("ghost"),
        Err(CatalogError::UnknownPlugin { .. })
    ));
    let after: Vec<CatalogManifestV1> = catalog.descriptors().into_iter().cloned().collect();
    assert_eq!(before, after);
}

/// D11: runtime/permissions/config_schema are declarations only — they never
/// execute, and failures never echo raw JSON or config_schema content.
#[test]
fn d11_declarations_do_not_execute_and_errors_do_not_echo() {
    for runtime in ["wasm", "process"] {
        // manifest_json emits compact JSON (no space after ':').
        let text = manifest_json("rt.decl", (1, 0, 0), ScopeKind::Session, vec![], vec![]).replace(
            "\"runtime\":\"builtin\"",
            &format!("\"runtime\":\"{runtime}\""),
        );
        let mut catalog = session_catalog();
        let manifest = parse_manifest(&text).unwrap();
        catalog.register(manifest).unwrap();
        // The runtime value is stored verbatim — declared, not executed.
        let stored = catalog
            .descriptors()
            .into_iter()
            .find(|m| m.id == "rt.decl")
            .unwrap();
        let expected = if runtime == "wasm" {
            RuntimeKind::Wasm
        } else {
            RuntimeKind::Process
        };
        assert_eq!(stored.runtime, expected);
    }

    // Permissions and a sentinel-bearing config_schema stay inert data.
    let sentinel = "SENTINEL-DO-NOT-ECHO-7f3";
    let mut catalog = session_catalog();
    let mut declared = session_manifest(
        "decl.only",
        (1, 0, 0),
        vec![svc("iface", (1, 0, 0))],
        vec![],
    );
    declared.runtime = RuntimeKind::Wasm;
    declared.permissions = vec!["fs.read".to_string(), "net.egress".to_string()];
    declared.config_schema = json!({
        "properties": {"token": {"description": sentinel}},
        "nested": [{"marker": sentinel}],
    });
    catalog.register(declared).unwrap();
    let plan = catalog.resolve(&roots(&["decl.only"])).unwrap();
    assert_eq!(plugin_ids(&plan), vec!["decl.only"]);
    // Declarations persist verbatim; nothing was executed or granted.
    let stored = catalog
        .descriptors()
        .into_iter()
        .find(|m| m.id == "decl.only")
        .unwrap();
    assert_eq!(stored.runtime, RuntimeKind::Wasm);
    assert_eq!(stored.permissions, ["fs.read", "net.egress"]);
    assert_eq!(
        stored.config_schema["properties"]["token"]["description"],
        json!(sentinel)
    );

    // Error paths carry no sentinel content: failing manifest that embeds the
    // sentinel in config_schema, and a sentinel as a mistyped field value.
    let bad_id_json = manifest_json("BAD ID", (1, 0, 0), ScopeKind::Session, vec![], vec![])
        .replace(
            "\"config_schema\":{}",
            &format!("\"config_schema\":{{\"leak\":{sentinel:?}}}"),
        );
    let err = parse_manifest(&bad_id_json).unwrap_err();
    let rendered = format!("{err:?} {err}");
    assert!(
        !rendered.contains(sentinel),
        "error echoed config_schema content: {rendered}"
    );

    let dup_with_sentinel = manifest_json("ok.id", (1, 0, 0), ScopeKind::Session, vec![], vec![])
        .replace(
            "\"config_schema\":{}",
            &format!("\"config_schema\":{{\"dup\":1,\"dup\":2,\"leak\":{sentinel:?}}}"),
        );
    let err = parse_manifest(&dup_with_sentinel).unwrap_err();
    let rendered = format!("{err:?} {err}");
    assert!(
        !rendered.contains(sentinel),
        "duplicate error echoed content: {rendered}"
    );

    let bad_scope_json = manifest_json("ok.id", (1, 0, 0), ScopeKind::Session, vec![], vec![])
        .replace("\"scope\":\"session\"", &format!("\"scope\":{sentinel:?}"));
    let err = parse_manifest(&bad_scope_json).unwrap_err();
    let rendered = format!("{err:?} {err}");
    assert!(
        !rendered.contains(sentinel),
        "enum error echoed value: {rendered}"
    );
}

/// D12: the new module compiles alongside the existing crate; the legacy WASM
/// ABI is untouched.
#[test]
fn d12_existing_abi_and_modules_unaffected() {
    assert_eq!(peachsh::wasm::ABI_VERSION, "peachsh.wasm.v1");
    // The catalog API is reachable through the public crate root.
    let catalog = PluginCatalog::new(ScopeKey::Host).unwrap();
    assert!(catalog.descriptors().is_empty());
    assert_eq!(catalog.scope(), &ScopeKey::Host);
}
