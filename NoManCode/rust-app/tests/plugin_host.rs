//! H01–H12 integration tests for `plugin_host` — public API only.
//!
//! Built-in fixtures: a real counter service consumed through `ctx.bound`,
//! event subscribers recording delivery order, and controlled
//! failure/panic plugins. No IO, no real credentials, no paid calls.

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use peachsh::plugin_catalog::{
    CatalogError, ExactVersion, InterfaceKey, InterfaceKind, PluginRef, ScopeKey, parse_manifest,
};
use peachsh::plugin_host::{
    BuiltinPlugin, EffectId, FailureStage, HostError, InstanceState, PluginContext, PluginError,
    PluginFailureCause, PluginHost,
};

// --- Fixtures ----------------------------------------------------------------

type SharedLog = Arc<Mutex<Vec<String>>>;

fn log_push(log: &SharedLog, entry: String) {
    log.lock().unwrap().push(entry);
}

fn log_read(log: &SharedLog) -> Vec<String> {
    log.lock().unwrap().clone()
}

/// The real service object handed from provider to consumer through
/// `ctx.bound` payloads.
struct CounterService {
    hits: AtomicUsize,
}

impl CounterService {
    fn hit(&self) -> usize {
        self.hits.fetch_add(1, Ordering::SeqCst) + 1
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn manifest(
    id: &str,
    display_name: &str,
    scope: &str,
    runtime: &str,
    provides: serde_json::Value,
    requires: serde_json::Value,
) -> String {
    serde_json::json!({
        "manifest_version": 1,
        "id": id,
        "display_name": display_name,
        "version": {"major": 1, "minor": 0, "patch": 0},
        "scope": scope,
        "runtime": runtime,
        "lifecycle": "host_managed",
        "surfaces": ["headless"],
        "permissions": [],
        "config_schema": {},
        "provides": provides,
        "requires": requires,
    })
    .to_string()
}

fn v1() -> ExactVersion {
    ExactVersion {
        major: 1,
        minor: 0,
        patch: 0,
    }
}

fn svc(name: &str) -> InterfaceKey {
    InterfaceKey {
        kind: InterfaceKind::Service,
        name: name.to_string(),
        version: v1(),
    }
}

fn evt(name: &str) -> InterfaceKey {
    InterfaceKey {
        kind: InterfaceKind::Event,
        name: name.to_string(),
        version: v1(),
    }
}

fn provides_json(entries: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::Value::Array(entries)
}

fn key_json(kind: &str, name: &str) -> serde_json::Value {
    serde_json::json!({"kind": kind, "name": name, "version": {"major":1,"minor":0,"patch":0}})
}

fn builtin_host() -> PluginHost {
    PluginHost::new(ScopeKey::Host).unwrap()
}

fn register(host: &mut PluginHost, manifest_json: &str) {
    host.catalog_mut()
        .register(parse_manifest(manifest_json).unwrap())
        .unwrap();
}

fn ids(refs: &[PluginRef]) -> Vec<String> {
    refs.iter().map(|r| r.id.clone()).collect()
}

fn states(host: &PluginHost) -> Vec<(String, InstanceState)> {
    host.instances()
        .iter()
        .map(|s| (s.id.clone(), s.state))
        .collect()
}

fn state_of(host: &PluginHost, id: &str) -> InstanceState {
    host.instances()
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.state)
        .expect("instance must exist")
}

// Provider that registers a CounterService for `key` on activate.
struct SvcProvider {
    key: InterfaceKey,
    service: Arc<CounterService>,
    log: SharedLog,
}

impl BuiltinPlugin for SvcProvider {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("act:{}", ctx.plugin_id()));
        ctx.register_effect(&self.key, Box::new(self.service.clone()))
            .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("deact:{}", _ctx.plugin_id()));
        Ok(())
    }
}

// Consumer that really calls the bound CounterService during activate.
struct SvcConsumer {
    need: InterfaceKey,
    log: SharedLog,
    captured: Arc<Mutex<Vec<HostError>>>,
}

impl BuiltinPlugin for SvcConsumer {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("act:{}", ctx.plugin_id()));
        match ctx.bound(&self.need) {
            Ok(bound) => {
                let service = bound
                    .payload()
                    .downcast_ref::<Arc<CounterService>>()
                    .ok_or_else(|| PluginError::new("payload type mismatch"))?;
                service.hit();
                assert_eq!(bound.provider_id(), "svc.provider");
            }
            Err(error) => {
                self.captured.lock().unwrap().push(error);
                return Err(PluginError::new("bound failed"));
            }
        }
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("deact:{}", _ctx.plugin_id()));
        Ok(())
    }
}

// Provider that registers an event-kind effect (the "sink") on activate.
struct EventProvider {
    key: InterfaceKey,
    log: SharedLog,
}

impl BuiltinPlugin for EventProvider {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("act:{}", ctx.plugin_id()));
        ctx.register_effect(&self.key, Box::new(()))
            .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("deact:{}", _ctx.plugin_id()));
        Ok(())
    }
}

// Consumer that subscribes to a bound event key on activate and records each
// delivered value plus the delivery order.
struct Subscriber {
    need: InterfaceKey,
    inbox: Arc<Mutex<Vec<u64>>>,
    order: SharedLog,
    log: SharedLog,
}

impl BuiltinPlugin for Subscriber {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("act:{}", ctx.plugin_id()));
        let inbox = self.inbox.clone();
        let order = self.order.clone();
        let id = ctx.plugin_id().to_string();
        ctx.subscribe(
            &self.need,
            Box::new(move |value: &dyn Any| {
                if let Some(v) = value.downcast_ref::<u64>() {
                    inbox.lock().unwrap().push(*v);
                }
                order.lock().unwrap().push(format!("deliver:{id}"));
            }),
        )
        .map_err(|e| PluginError::new(format!("subscribe failed: {e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("deact:{}", _ctx.plugin_id()));
        Ok(())
    }
}

struct ErrOnActivate;
impl BuiltinPlugin for ErrOnActivate {
    fn activate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Err(PluginError::new("deliberate activate failure"))
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

struct PanicOnActivate;
impl BuiltinPlugin for PanicOnActivate {
    fn activate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        panic!("deliberate activate panic");
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

// Tries to register an undeclared key first, captures the typed error, then
// registers its declared provides and succeeds — proving the rejection is at
// the context boundary, not a global failure.
struct TriesUndeclared {
    declared: InterfaceKey,
    undeclared: InterfaceKey,
    captured: Arc<Mutex<Vec<HostError>>>,
}

impl BuiltinPlugin for TriesUndeclared {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        match ctx.register_effect(&self.undeclared, Box::new(())) {
            Err(error) => self.captured.lock().unwrap().push(error),
            Ok(_) => panic!("undeclared key must not register"),
        }
        ctx.register_effect(&self.declared, Box::new(()))
            .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

// Declares provides [declared, missing] but only registers `declared`.
struct LeavesProvidesUncovered {
    declared: InterfaceKey,
}

impl BuiltinPlugin for LeavesProvidesUncovered {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_effect(&self.declared, Box::new(()))
            .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

// Emits on its declared event key inside activate (nobody can be subscribed
// yet — proves no back-fill for late subscriptions).
struct EarlyEmitter {
    key: InterfaceKey,
    delivered: Arc<Mutex<Vec<usize>>>,
}

impl BuiltinPlugin for EarlyEmitter {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_effect(&self.key, Box::new(()))
            .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        let n = ctx
            .emit(&self.key, Box::new(99u64))
            .map_err(|e| PluginError::new(format!("emit failed: {e}")))?;
        self.delivered.lock().unwrap().push(n);
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

// --- H01 完整链 ---------------------------------------------------------------

#[test]
fn h01_full_chain_register_start_bound_call() {
    let mut host = builtin_host();
    let log: SharedLog = Default::default();
    let service = Arc::new(CounterService {
        hits: AtomicUsize::new(0),
    });
    let captured: Arc<Mutex<Vec<HostError>>> = Default::default();

    register(
        &mut host,
        &manifest(
            "svc.provider",
            "Provider",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "counter")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "svc.consumer",
            "Consumer",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "counter")]),
        ),
    );
    {
        let service = service.clone();
        let log = log.clone();
        host.with_factory("svc.provider", move || {
            Box::new(SvcProvider {
                key: svc("counter"),
                service: service.clone(),
                log: log.clone(),
            })
        });
    }
    {
        let log = log.clone();
        let captured = captured.clone();
        host.with_factory("svc.consumer", move || {
            Box::new(SvcConsumer {
                need: svc("counter"),
                log: log.clone(),
                captured: captured.clone(),
            })
        });
    }

    let report = host.start(&["svc.consumer".to_string()]).unwrap();
    // Provider activates before its consumer (plan order, not id order:
    // "svc.consumer" < "svc.provider" alphabetically, so this also proves the
    // order is topological rather than lexical).
    assert_eq!(
        ids(&report.activated),
        vec!["svc.provider".to_string(), "svc.consumer".to_string()]
    );
    assert!(report.reused.is_empty());
    // The consumer really invoked the provider's payload through bound.
    assert_eq!(service.hits(), 1);
    assert!(captured.lock().unwrap().is_empty());
    assert_eq!(
        log_read(&log),
        vec![
            "act:svc.provider".to_string(),
            "act:svc.consumer".to_string()
        ]
    );

    // Deterministic snapshots: instances sorted by id, effects by owner.
    let snapshot = states(&host);
    assert_eq!(
        snapshot,
        vec![
            ("svc.consumer".to_string(), InstanceState::Active),
            ("svc.provider".to_string(), InstanceState::Active),
        ]
    );
    let effects = host.effects();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].plugin_id, "svc.provider");
    assert_eq!(effects[0].key, svc("counter"));
    assert!(!effects[0].is_subscription);
    assert_eq!(host.instances()[1].effect_ids, vec![effects[0].effect_id]);
}

// --- H02 拓扑序 ---------------------------------------------------------------

fn build_layered_host(order: &[&str], log: &SharedLog) -> PluginHost {
    // base --x--> mid --y--> top; dia --d--> {left,right} --{l,r}--> apex
    let mut host = builtin_host();
    let specs: Vec<(&str, Vec<serde_json::Value>, Vec<serde_json::Value>)> = vec![
        ("base", vec![key_json("service", "x")], vec![]),
        (
            "mid",
            vec![key_json("service", "y")],
            vec![key_json("service", "x")],
        ),
        ("top", vec![], vec![key_json("service", "y")]),
        ("dia", vec![key_json("service", "d")], vec![]),
        (
            "left",
            vec![key_json("service", "l")],
            vec![key_json("service", "d")],
        ),
        (
            "right",
            vec![key_json("service", "r")],
            vec![key_json("service", "d")],
        ),
        (
            "apex",
            vec![],
            vec![key_json("service", "l"), key_json("service", "r")],
        ),
    ];
    for id in order {
        let (_, provides, requires) = specs.iter().find(|entry| entry.0 == *id).unwrap();
        register(
            &mut host,
            &manifest(
                id,
                id,
                "host",
                "builtin",
                provides_json(provides.clone()),
                serde_json::Value::Array(requires.clone()),
            ),
        );
        let log = log.clone();
        let register_keys: Vec<InterfaceKey> = provides
            .iter()
            .map(|v| InterfaceKey {
                kind: InterfaceKind::Service,
                name: v["name"].as_str().unwrap().to_string(),
                version: v1(),
            })
            .collect();
        host.with_factory(*id, move || {
            Box::new(GenericPlugin {
                register_keys: register_keys.clone(),
                log: log.clone(),
            })
        });
    }
    host
}

// Generic provider: registers each declared provides key with a unit payload.
struct GenericPlugin {
    register_keys: Vec<InterfaceKey>,
    log: SharedLog,
}

impl BuiltinPlugin for GenericPlugin {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("act:{}", ctx.plugin_id()));
        for key in &self.register_keys {
            ctx.register_effect(key, Box::new(()))
                .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        }
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("deact:{}", _ctx.plugin_id()));
        Ok(())
    }
}

#[test]
fn h02_topology_order_and_reverse_teardown() {
    let log: SharedLog = Default::default();
    let mut host = build_layered_host(
        &["base", "mid", "top", "dia", "left", "right", "apex"],
        &log,
    );
    let roots = vec!["top".to_string(), "apex".to_string()];
    let plan = host.resolve(&roots).unwrap();
    let report = host.start(&roots).unwrap();
    // Activation order equals the plan order exactly.
    assert_eq!(ids(&report.activated), ids(&plan.ordered_plugins));
    // And it is provider-before-consumer (base before mid before top).
    let order = ids(&report.activated);
    let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
    assert!(pos("base") < pos("mid") && pos("mid") < pos("top"));
    assert!(pos("dia") < pos("left") && pos("left") < pos("apex"));
    assert!(pos("right") < pos("apex"));

    // Input order does not matter: reversed registration + reversed roots
    // produce the identical plan and activation order.
    let log2: SharedLog = Default::default();
    let mut host2 = build_layered_host(
        &["apex", "right", "left", "dia", "top", "mid", "base"],
        &log2,
    );
    let roots_rev = vec!["apex".to_string(), "top".to_string()];
    let plan2 = host2.resolve(&roots_rev).unwrap();
    let report2 = host2.start(&roots_rev).unwrap();
    assert_eq!(plan, plan2);
    assert_eq!(ids(&report.activated), ids(&report2.activated));

    // stop_subtree(base): Active dependents first — exact reverse of the
    // activation subsequence for {base,mid,top}.
    let stopped = host.stop_subtree("base").unwrap();
    assert_eq!(
        ids(&stopped),
        vec!["top".to_string(), "mid".to_string(), "base".to_string()]
    );
    assert_eq!(
        states(&host)
            .into_iter()
            .filter(|(id, _)| ["base", "mid", "top"].contains(&id.as_str()))
            .map(|(_, s)| s)
            .collect::<Vec<_>>(),
        vec![
            InstanceState::Stopped,
            InstanceState::Stopped,
            InstanceState::Stopped
        ]
    );
    // Untouched diamond stays Active.
    assert_eq!(state_of(&host, "apex"), InstanceState::Active);
}

// --- H03 撤销 -----------------------------------------------------------------

#[test]
fn h03_revocation_leaves_no_residue() {
    let mut host = builtin_host();
    let log: SharedLog = Default::default();
    let inbox: Arc<Mutex<Vec<u64>>> = Default::default();
    let order: SharedLog = Default::default();

    register(
        &mut host,
        &manifest(
            "ev.provider",
            "P",
            "host",
            "builtin",
            provides_json(vec![key_json("event", "ticks"), key_json("service", "svc")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "ev.consumer",
            "C",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("event", "ticks")]),
        ),
    );
    {
        let log = log.clone();
        host.with_factory("ev.provider", move || {
            Box::new(MultiProvider {
                keys: vec![evt("ticks"), svc("svc")],
                log: log.clone(),
            })
        });
    }
    {
        let (inbox, order, log) = (inbox.clone(), order.clone(), log.clone());
        host.with_factory("ev.consumer", move || {
            Box::new(Subscriber {
                need: evt("ticks"),
                inbox: inbox.clone(),
                order: order.clone(),
                log: log.clone(),
            })
        });
    }
    host.start(&["ev.consumer".to_string()]).unwrap();
    // 2 provider effects + 1 consumer subscription.
    assert_eq!(host.effects().len(), 3);
    assert_eq!(
        host.effects().iter().filter(|e| e.is_subscription).count(),
        1
    );

    let stopped = host.stop_subtree("ev.provider").unwrap();
    assert_eq!(
        ids(&stopped),
        vec!["ev.consumer".to_string(), "ev.provider".to_string()]
    );
    // No residue: every effect — provides AND the consumer's subscription —
    // is gone, both instances Stopped.
    assert!(host.effects().is_empty());
    assert_eq!(
        states(&host),
        vec![
            ("ev.consumer".to_string(), InstanceState::Stopped),
            ("ev.provider".to_string(), InstanceState::Stopped),
        ]
    );
    assert_eq!(
        host.instances()
            .iter()
            .map(|s| s.effect_ids.len())
            .sum::<usize>(),
        0
    );

    // Unload removes the instance records entirely.
    host.unload("ev.consumer").unwrap();
    host.unload("ev.provider").unwrap();
    assert!(host.instances().is_empty());
    assert!(host.pending_recovery().is_empty());
}

// Provider registering several keys.
struct MultiProvider {
    keys: Vec<InterfaceKey>,
    log: SharedLog,
}

impl BuiltinPlugin for MultiProvider {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("act:{}", ctx.plugin_id()));
        for key in &self.keys {
            ctx.register_effect(key, Box::new(()))
                .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        }
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        log_push(&self.log, format!("deact:{}", _ctx.plugin_id()));
        Ok(())
    }
}

// --- H04 依赖保护 --------------------------------------------------------------

#[test]
fn h04_dependents_active_protects_provider() {
    let mut host = builtin_host();
    let log: SharedLog = Default::default();
    register(
        &mut host,
        &manifest(
            "core",
            "Core",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    for consumer in ["c.b", "c.a"] {
        register(
            &mut host,
            &manifest(
                consumer,
                consumer,
                "host",
                "builtin",
                provides_json(vec![]),
                serde_json::Value::Array(vec![key_json("service", "s")]),
            ),
        );
    }
    {
        let log = log.clone();
        host.with_factory("core", move || {
            Box::new(MultiProvider {
                keys: vec![svc("s")],
                log: log.clone(),
            })
        });
    }
    for consumer in ["c.b", "c.a"] {
        let log = log.clone();
        host.with_factory(consumer, move || {
            Box::new(GenericPlugin {
                register_keys: vec![],
                log: log.clone(),
            })
        });
    }
    host.start(&["c.a".to_string(), "c.b".to_string()]).unwrap();

    // Direct stop is refused with sorted dependents; nothing changes.
    let err = host.stop("core").unwrap_err();
    match err {
        HostError::DependentsActive {
            plugin_id,
            dependents,
        } => {
            assert_eq!(plugin_id, "core");
            assert_eq!(dependents, vec!["c.a".to_string(), "c.b".to_string()]);
        }
        other => panic!("expected DependentsActive, got {other:?}"),
    }
    assert_eq!(state_of(&host, "core"), InstanceState::Active);
    assert_eq!(state_of(&host, "c.a"), InstanceState::Active);
    assert_eq!(state_of(&host, "c.b"), InstanceState::Active);
    assert_eq!(host.effects().len(), 1);

    // After the consumers stop individually, the provider can stop.
    host.stop("c.a").unwrap();
    host.stop("c.b").unwrap();
    host.stop("core").unwrap();
    assert_eq!(state_of(&host, "core"), InstanceState::Stopped);
}

// --- H05 激活失败回滚 ------------------------------------------------------------

#[test]
fn h05_activation_failure_rolls_back_in_plan_reverse() {
    // Err case.
    let mut host = builtin_host();
    let log: SharedLog = Default::default();
    register(
        &mut host,
        &manifest(
            "prov",
            "P",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "cons",
            "C",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "s")]),
        ),
    );
    {
        let log = log.clone();
        host.with_factory("prov", move || {
            Box::new(MultiProvider {
                keys: vec![svc("s")],
                log: log.clone(),
            })
        });
    }
    host.with_factory("cons", || Box::new(ErrOnActivate));

    let err = host.start(&["cons".to_string()]).unwrap_err();
    match err {
        HostError::PluginStartError {
            plugin_id,
            stage,
            cause,
            unwound,
        } => {
            assert_eq!(plugin_id, "cons");
            assert_eq!(stage, FailureStage::Activate);
            assert_eq!(
                *cause,
                peachsh::plugin_host::PluginFailureCause::PluginError {
                    message: "deliberate activate failure".to_string()
                }
            );
            assert_eq!(ids(&unwound), vec!["prov".to_string()]);
        }
        other => panic!("expected PluginStartError, got {other:?}"),
    }
    // Rolled back: provider is Stopped (deactivate ran), no effect residue.
    assert_eq!(state_of(&host, "prov"), InstanceState::Stopped);
    assert_eq!(
        state_of(&host, "cons"),
        InstanceState::Failed {
            stage: FailureStage::Activate
        }
    );
    assert!(host.effects().is_empty());
    assert_eq!(
        log_read(&log),
        vec!["act:prov".to_string(), "deact:prov".to_string()]
    );

    // Panic case on a second host.
    let mut host2 = builtin_host();
    register(
        &mut host2,
        &manifest(
            "prov",
            "P",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host2,
        &manifest(
            "cons",
            "C",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "s")]),
        ),
    );
    host2.with_factory("prov", || {
        Box::new(MultiProvider {
            keys: vec![svc("s")],
            log: Default::default(),
        })
    });
    host2.with_factory("cons", || Box::new(PanicOnActivate));
    let err = host2.start(&["cons".to_string()]).unwrap_err();
    match err {
        HostError::PluginPanic { plugin_id, stage } => {
            assert_eq!(plugin_id, "cons");
            assert_eq!(stage, FailureStage::Activate);
        }
        other => panic!("expected PluginPanic, got {other:?}"),
    }
    assert_eq!(state_of(&host2, "prov"), InstanceState::Stopped);
    assert_eq!(
        state_of(&host2, "cons"),
        InstanceState::Failed {
            stage: FailureStage::Activate
        }
    );
    assert!(host2.effects().is_empty());
}

// --- H06 声明违背 ---------------------------------------------------------------

#[test]
fn h06_declaration_violations_are_typed_and_rolled_back() {
    // (a) register_effect on an undeclared provides key -> UndeclaredInterface
    // at the context boundary (captured by the plugin itself).
    let mut host = builtin_host();
    let captured: Arc<Mutex<Vec<HostError>>> = Default::default();
    register(
        &mut host,
        &manifest(
            "bad.reg",
            "B",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "declared")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    {
        let captured = captured.clone();
        host.with_factory("bad.reg", move || {
            Box::new(TriesUndeclared {
                declared: svc("declared"),
                undeclared: svc("undeclared"),
                captured: captured.clone(),
            })
        });
    }
    host.start(&["bad.reg".to_string()]).unwrap();
    let captured = captured.lock().unwrap().clone();
    assert_eq!(
        captured,
        vec![HostError::UndeclaredInterface {
            plugin_id: "bad.reg".to_string(),
            key: svc("undeclared"),
        }]
    );
    assert_eq!(host.effects().len(), 1); // the declared one registered fine

    // (b) Missing coverage -> ProvidesNotCovered with the missing key, full
    // rollback of the earlier provider.
    let mut host2 = builtin_host();
    let log: SharedLog = Default::default();
    register(
        &mut host2,
        &manifest(
            "prov",
            "P",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host2,
        &manifest(
            "gap",
            "G",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "a"), key_json("service", "b")]),
            serde_json::Value::Array(vec![key_json("service", "s")]),
        ),
    );
    {
        let log = log.clone();
        host2.with_factory("prov", move || {
            Box::new(MultiProvider {
                keys: vec![svc("s")],
                log: log.clone(),
            })
        });
    }
    host2.with_factory("gap", || {
        Box::new(LeavesProvidesUncovered { declared: svc("a") })
    });
    let err = host2.start(&["gap".to_string()]).unwrap_err();
    match err {
        HostError::ProvidesNotCovered {
            plugin_id,
            missing,
            unwound,
        } => {
            assert_eq!(plugin_id, "gap");
            assert_eq!(missing, vec![svc("b")]);
            assert_eq!(ids(&unwound), vec!["prov".to_string()]);
        }
        other => panic!("expected ProvidesNotCovered, got {other:?}"),
    }
    assert!(host2.effects().is_empty());
    assert_eq!(state_of(&host2, "prov"), InstanceState::Stopped);
    assert_eq!(
        state_of(&host2, "gap"),
        InstanceState::Failed {
            stage: FailureStage::Activate
        }
    );
}

// --- H07 运行时 ----------------------------------------------------------------

#[test]
fn h07_runtime_gates() {
    // wasm / process manifests participate in the plan but never instantiate.
    for runtime in ["wasm", "process"] {
        let mut host = builtin_host();
        register(
            &mut host,
            &manifest(
                "ext",
                "E",
                "host",
                runtime,
                provides_json(vec![key_json("service", "s")]),
                serde_json::Value::Array(vec![]),
            ),
        );
        let plan = host.resolve(&["ext".to_string()]).unwrap();
        assert_eq!(ids(&plan.ordered_plugins), vec!["ext".to_string()]);
        let err = host.start(&["ext".to_string()]).unwrap_err();
        match err {
            HostError::UnsupportedRuntime { plugin_id, unwound } => {
                assert_eq!(plugin_id, "ext");
                assert!(unwound.is_empty());
            }
            other => panic!("expected UnsupportedRuntime, got {other:?}"),
        }
        assert!(host.instances().is_empty());
        assert!(host.effects().is_empty());
    }

    // builtin without an injected factory -> MissingFactory, earlier
    // activations rolled back.
    let mut host = builtin_host();
    register(
        &mut host,
        &manifest(
            "prov",
            "P",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "nofac",
            "N",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "s")]),
        ),
    );
    host.with_factory("prov", || {
        Box::new(MultiProvider {
            keys: vec![svc("s")],
            log: Default::default(),
        })
    });
    let err = host.start(&["nofac".to_string()]).unwrap_err();
    match err {
        HostError::MissingFactory { plugin_id, unwound } => {
            assert_eq!(plugin_id, "nofac");
            assert_eq!(ids(&unwound), vec!["prov".to_string()]);
        }
        other => panic!("expected MissingFactory, got {other:?}"),
    }
    assert_eq!(state_of(&host, "prov"), InstanceState::Stopped);
    assert!(host.effects().is_empty());
}

// --- H08 状态与作用域 ------------------------------------------------------------

#[test]
fn h08_state_rules_and_scope_isolation() {
    let mut host = builtin_host();
    register(
        &mut host,
        &manifest(
            "solo",
            "Solo",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    host.with_factory("solo", || {
        Box::new(MultiProvider {
            keys: vec![svc("s")],
            log: Default::default(),
        })
    });
    host.start(&["solo".to_string()]).unwrap();

    // Repeating start on an Active root is InvalidState and changes nothing.
    assert!(matches!(
        host.start(&["solo".to_string()]),
        Err(HostError::InvalidState {
            current: InstanceState::Active,
            ..
        })
    ));
    assert_eq!(state_of(&host, "solo"), InstanceState::Active);

    // unload on Active is InvalidState.
    assert!(matches!(
        host.unload("solo"),
        Err(HostError::InvalidState {
            current: InstanceState::Active,
            ..
        })
    ));

    // stop → Stopped; stopping again is InvalidState.
    host.stop("solo").unwrap();
    assert!(matches!(
        host.stop("solo"),
        Err(HostError::InvalidState {
            current: InstanceState::Stopped,
            ..
        })
    ));

    // Unknown ids are typed on every entry point.
    assert!(matches!(
        host.stop("ghost"),
        Err(HostError::UnknownPlugin { .. })
    ));
    assert!(matches!(
        host.unload("ghost"),
        Err(HostError::UnknownPlugin { .. })
    ));
    assert!(matches!(
        host.stop_subtree("ghost"),
        Err(HostError::UnknownPlugin { .. })
    ));

    // Two same-kind scopes do not satisfy each other: a consumer in project
    // p2 cannot see p1's provider.
    let mut p1 = PluginHost::new(ScopeKey::Project {
        project_id: "p1".to_string(),
    })
    .unwrap();
    let mut p2 = PluginHost::new(ScopeKey::Project {
        project_id: "p2".to_string(),
    })
    .unwrap();
    register(
        &mut p1,
        &manifest(
            "prov",
            "P",
            "project",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut p2,
        &manifest(
            "cons",
            "C",
            "project",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "s")]),
        ),
    );
    let err = p2.start(&["cons".to_string()]).unwrap_err();
    assert!(matches!(
        err,
        HostError::Catalog(CatalogError::MissingDependency { .. })
    ));
    assert!(p2.instances().is_empty());
    // p1 unaffected.
    assert!(p1.instances().is_empty());
}

// --- H09 事件 ------------------------------------------------------------------

#[test]
fn h09_events_deliver_in_subscriber_id_order() {
    let mut host = builtin_host();
    let log: SharedLog = Default::default();
    let order: SharedLog = Default::default();
    let inbox_a: Arc<Mutex<Vec<u64>>> = Default::default();
    let inbox_b: Arc<Mutex<Vec<u64>>> = Default::default();
    let inbox_c: Arc<Mutex<Vec<u64>>> = Default::default();

    register(
        &mut host,
        &manifest(
            "ev.pub",
            "Pub",
            "host",
            "builtin",
            provides_json(vec![key_json("event", "ticks")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    // Register subscribers in reverse id order; delivery must still follow
    // ascending subscriber id.
    for sub in ["sub.b", "sub.a"] {
        register(
            &mut host,
            &manifest(
                sub,
                sub,
                "host",
                "builtin",
                provides_json(vec![]),
                serde_json::Value::Array(vec![key_json("event", "ticks")]),
            ),
        );
    }
    {
        let log = log.clone();
        host.with_factory("ev.pub", move || {
            Box::new(EventProvider {
                key: evt("ticks"),
                log: log.clone(),
            })
        });
    }
    for (sub, inbox) in [("sub.a", &inbox_a), ("sub.b", &inbox_b)] {
        let (inbox, order, log) = (inbox.clone(), order.clone(), log.clone());
        host.with_factory(sub, move || {
            Box::new(Subscriber {
                need: evt("ticks"),
                inbox: inbox.clone(),
                order: order.clone(),
                log: log.clone(),
            })
        });
    }
    host.start(&["sub.a".to_string(), "sub.b".to_string()])
        .unwrap();

    let delivered = host.emit("ev.pub", &evt("ticks"), Box::new(7u64)).unwrap();
    assert_eq!(delivered, 2);
    // Ascending subscriber id order regardless of registration order.
    assert_eq!(
        log_read(&order),
        vec!["deliver:sub.a".to_string(), "deliver:sub.b".to_string()]
    );
    assert_eq!(*inbox_a.lock().unwrap(), vec![7u64]);
    assert_eq!(*inbox_b.lock().unwrap(), vec![7u64]);

    // A subscription created after an emit is not back-filled.
    register(
        &mut host,
        &manifest(
            "sub.c",
            "C",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("event", "ticks")]),
        ),
    );
    {
        let (inbox, order, log) = (inbox_c.clone(), order.clone(), log.clone());
        host.with_factory("sub.c", move || {
            Box::new(Subscriber {
                need: evt("ticks"),
                inbox: inbox.clone(),
                order: order.clone(),
                log: log.clone(),
            })
        });
    }
    host.start(&["sub.c".to_string()]).unwrap();
    assert!(inbox_c.lock().unwrap().is_empty());

    // After sub.a stops, its subscription is revoked and emit skips it.
    host.stop("sub.a").unwrap();
    assert_eq!(
        host.effects()
            .iter()
            .filter(|e| e.is_subscription)
            .map(|e| e.plugin_id.clone())
            .collect::<Vec<_>>(),
        vec!["sub.b".to_string(), "sub.c".to_string()]
    );
    let delivered = host.emit("ev.pub", &evt("ticks"), Box::new(8u64)).unwrap();
    assert_eq!(delivered, 2);
    assert_eq!(*inbox_a.lock().unwrap(), vec![7u64]); // no new delivery
    assert_eq!(*inbox_b.lock().unwrap(), vec![7u64, 8u64]);
    assert_eq!(*inbox_c.lock().unwrap(), vec![8u64]);

    // After the provider stops, emit is a typed error — no delivery at all.
    host.stop_subtree("ev.pub").unwrap();
    assert!(matches!(
        host.emit("ev.pub", &evt("ticks"), Box::new(9u64)),
        Err(HostError::InvalidState { .. })
    ));
    assert_eq!(*inbox_b.lock().unwrap(), vec![7u64, 8u64]);
    assert!(host.effects().is_empty());
}

// --- H10 恢复清单 ---------------------------------------------------------------

#[test]
fn h10_recovery_list_records_incomplete_cleanup() {
    // deactivate returns Err.
    let mut host = builtin_host();
    register(
        &mut host,
        &manifest(
            "bad.stop",
            "B",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    host.with_factory("bad.stop", || Box::new(ErrStopProvider));
    host.start(&["bad.stop".to_string()]).unwrap();
    let err = host.stop("bad.stop").unwrap_err();
    assert!(matches!(
        err,
        HostError::PluginDeactivateError { ref plugin_id } if plugin_id == "bad.stop"
    ));
    assert_eq!(
        state_of(&host, "bad.stop"),
        InstanceState::Failed {
            stage: FailureStage::Deactivate
        }
    );
    // Registry still residue-free; the failure is on the recovery list.
    assert!(host.effects().is_empty());
    let recovery = host.pending_recovery();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].plugin_id, "bad.stop");
    assert_eq!(recovery[0].stage, FailureStage::Deactivate);
    assert!(matches!(
        recovery[0].cause,
        peachsh::plugin_host::PluginFailureCause::PluginError { .. }
    ));

    // unload(Failed) removes the instance but keeps the recovery entry.
    host.unload("bad.stop").unwrap();
    assert!(host.instances().is_empty());
    assert_eq!(host.pending_recovery().len(), 1);

    // deactivate panics -> PluginPanic + Failed + recovery entry.
    let mut host2 = builtin_host();
    register(
        &mut host2,
        &manifest(
            "panic.stop",
            "P",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    host2.with_factory("panic.stop", || Box::new(PanicStopProvider));
    host2.start(&["panic.stop".to_string()]).unwrap();
    let err = host2.stop("panic.stop").unwrap_err();
    assert!(matches!(
        err,
        HostError::PluginPanic {
            stage: FailureStage::Deactivate,
            ..
        }
    ));
    assert_eq!(
        state_of(&host2, "panic.stop"),
        InstanceState::Failed {
            stage: FailureStage::Deactivate
        }
    );
    assert!(host2.effects().is_empty());
    let recovery = host2.pending_recovery();
    assert_eq!(recovery.len(), 1);
    assert_eq!(
        recovery[0].cause,
        peachsh::plugin_host::PluginFailureCause::Panic
    );
}

// Provider that registers an effect and fails inside deactivate.
struct ErrStopProvider;
impl BuiltinPlugin for ErrStopProvider {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_effect(&svc("s"), Box::new(()))
            .map_err(|e| PluginError::new(format!("{e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Err(PluginError::new("cleanup incomplete"))
    }
}

struct PanicStopProvider;
impl BuiltinPlugin for PanicStopProvider {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_effect(&svc("s"), Box::new(()))
            .map_err(|e| PluginError::new(format!("{e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        panic!("deliberate deactivate panic");
    }
}

// --- H11 纯度/身份 ---------------------------------------------------------------

#[test]
fn h11_purity_determinism_and_display_name_not_identity() {
    let mut host = builtin_host();
    // Two plugins share a display_name; ids remain the identity.
    register(
        &mut host,
        &manifest(
            "same.one",
            "Identical",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "same.two",
            "Identical",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "s")]),
        ),
    );
    let roots = vec!["same.two".to_string()];
    let plan1 = host.resolve(&roots).unwrap();
    let plan2 = host.resolve(&roots).unwrap();
    assert_eq!(plan1, plan2);

    let counter = Arc::new(CounterService {
        hits: AtomicUsize::new(0),
    });
    {
        let counter = counter.clone();
        host.with_factory("same.one", move || {
            Box::new(SvcProvider {
                key: svc("s"),
                service: counter.clone(),
                log: Default::default(),
            })
        });
    }
    let captured: Arc<Mutex<Vec<HostError>>> = Default::default();
    {
        let captured = captured.clone();
        host.with_factory("same.two", move || {
            Box::new(ProbeThenConsume {
                probe: svc("not-required"),
                need: svc("s"),
                captured: captured.clone(),
            })
        });
    }
    host.start(&roots).unwrap();
    // The declared key bound and really invoked the provider's service.
    assert_eq!(counter.hits(), 1);
    // bound on an undeclared requires key -> UnboundInterface at the boundary.
    let captured = captured.lock().unwrap().clone();
    assert_eq!(
        captured,
        vec![HostError::UnboundInterface {
            plugin_id: "same.two".to_string(),
            key: svc("not-required"),
        }]
    );
    // Same display_name, different ids: both active as distinct instances.
    assert_eq!(
        states(&host),
        vec![
            ("same.one".to_string(), InstanceState::Active),
            ("same.two".to_string(), InstanceState::Active),
        ]
    );
}

// Consumer that probes an undeclared key (captures the typed error) and then
// really calls the bound service — proving rejection is key-scoped.
struct ProbeThenConsume {
    probe: InterfaceKey,
    need: InterfaceKey,
    captured: Arc<Mutex<Vec<HostError>>>,
}

impl BuiltinPlugin for ProbeThenConsume {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        match ctx.bound(&self.probe) {
            Err(error) => self.captured.lock().unwrap().push(error),
            Ok(_) => panic!("undeclared key must not bound"),
        }
        let bound = ctx
            .bound(&self.need)
            .map_err(|e| PluginError::new(format!("bound failed: {e}")))?;
        let service = bound
            .payload()
            .downcast_ref::<Arc<CounterService>>()
            .ok_or_else(|| PluginError::new("payload type mismatch"))?;
        service.hit();
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

// --- H12 回归 ------------------------------------------------------------------

#[test]
fn h12_existing_surfaces_unaffected() {
    // The wasm ABI marker is untouched by this slice.
    assert_eq!(peachsh::wasm::ABI_VERSION, "peachsh.wasm.v1");
    // The catalog module behind the host still behaves per NEXT-02A.
    let host = builtin_host();
    assert!(matches!(
        host.resolve(&["ghost".to_string()]),
        Err(HostError::Catalog(CatalogError::UnknownRoot { .. }))
    ));
}

// --- 附加：context 边界与补投 ----------------------------------------------------

#[test]
fn emit_inside_activate_is_allowed_and_never_backfills() {
    let mut host = builtin_host();
    let delivered: Arc<Mutex<Vec<usize>>> = Default::default();
    register(
        &mut host,
        &manifest(
            "early.pub",
            "E",
            "host",
            "builtin",
            provides_json(vec![key_json("event", "ticks")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    {
        let delivered = delivered.clone();
        host.with_factory("early.pub", move || {
            Box::new(EarlyEmitter {
                key: evt("ticks"),
                delivered: delivered.clone(),
            })
        });
    }
    host.start(&["early.pub".to_string()]).unwrap();
    // Nobody could be subscribed during activate: 0 deliveries recorded.
    assert_eq!(*delivered.lock().unwrap(), vec![0usize]);
}

// --- 复核补充：规则反例与边界 ----------------------------------------------------

// Does nothing on activate; used where only the lifecycle matters.
struct IdlePlugin;
impl BuiltinPlugin for IdlePlugin {
    fn activate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

// Registers `key` on activate and publishes the new EffectId so another
// plugin can try (and fail) to unregister it.
struct EffectIdSharer {
    key: InterfaceKey,
    shared: Arc<Mutex<Option<EffectId>>>,
}

impl BuiltinPlugin for EffectIdSharer {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let id = ctx
            .register_effect(&self.key, Box::new(()))
            .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        *self.shared.lock().unwrap() = Some(id);
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

// One scripted context probe; returns the call's outcome for capture.
type CtxProbe = Box<dyn Fn(&mut PluginContext) -> Result<(), HostError> + Send>;

// Runs scripted probes against the restricted context inside activate; each
// outcome is captured so typed rejections can be asserted without aborting
// activation. The prober registers `declared` at the end so its provides
// coverage stays satisfied.
struct RuleProber {
    declared: InterfaceKey,
    probes: Vec<CtxProbe>,
    outcomes: Arc<Mutex<Vec<Result<(), HostError>>>>,
    seen_scope: Arc<Mutex<Option<ScopeKey>>>,
    seen_manifest_id: Arc<Mutex<Option<String>>>,
}

impl BuiltinPlugin for RuleProber {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        *self.seen_scope.lock().unwrap() = Some(ctx.scope().clone());
        *self.seen_manifest_id.lock().unwrap() = Some(ctx.manifest().id.clone());
        for probe in &self.probes {
            let outcome = probe(ctx);
            self.outcomes.lock().unwrap().push(outcome);
        }
        ctx.register_effect(&self.declared, Box::new(()))
            .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

// Registers `key` on activate; deactivate returns a typed error.
struct ErrOnDeactivate {
    key: InterfaceKey,
}

impl BuiltinPlugin for ErrOnDeactivate {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_effect(&self.key, Box::new(()))
            .map_err(|e| PluginError::new(format!("register failed: {e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Err(PluginError::new("cleanup incomplete"))
    }
}

// Requires a provider (so it sits after it in the plan) but fails deactivate.
struct ErrOnDeactivateConsumer;

impl BuiltinPlugin for ErrOnDeactivateConsumer {
    fn activate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Err(PluginError::new("consumer cleanup incomplete"))
    }
}

// Subscriber whose delivery callback panics.
struct PanicSubscriber {
    need: InterfaceKey,
}

impl BuiltinPlugin for PanicSubscriber {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.subscribe(
            &self.need,
            Box::new(|_value: &dyn Any| panic!("deliberate subscription panic")),
        )
        .map_err(|e| PluginError::new(format!("subscribe failed: {e}")))?;
        Ok(())
    }
    fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

#[test]
fn ctx_rule_boundaries_reject_with_typed_errors() {
    let mut host = builtin_host();
    let stolen: Arc<Mutex<Option<EffectId>>> = Default::default();
    let outcomes: Arc<Mutex<Vec<Result<(), HostError>>>> = Default::default();
    let seen_scope: Arc<Mutex<Option<ScopeKey>>> = Default::default();
    let seen_id: Arc<Mutex<Option<String>>> = Default::default();

    // The peer provides the service the prober requires, and publishes the
    // effect id the prober will try to unregister.
    register(
        &mut host,
        &manifest(
            "peer",
            "Peer",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "peer.svc")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    {
        let stolen = stolen.clone();
        host.with_factory("peer", move || {
            Box::new(EffectIdSharer {
                key: svc("peer.svc"),
                shared: stolen.clone(),
            })
        });
    }
    register(
        &mut host,
        &manifest(
            "prober",
            "Prober",
            "host",
            "builtin",
            provides_json(vec![
                key_json("service", "probe.ok"),
                key_json("service", "probe.extra"),
            ]),
            serde_json::Value::Array(vec![key_json("service", "peer.svc")]),
        ),
    );
    {
        let (outcomes, seen_scope, seen_id, stolen) = (
            outcomes.clone(),
            seen_scope.clone(),
            seen_id.clone(),
            stolen.clone(),
        );
        host.with_factory("prober", move || {
            let stolen_probe = stolen.clone();
            let probes: Vec<CtxProbe> = vec![
                // 0: registering a second declared key is allowed.
                Box::new(|ctx| {
                    ctx.register_effect(&svc("probe.extra"), Box::new(()))
                        .map(|_| ())
                }),
                // 1: same key twice — DuplicateEffect.
                Box::new(|ctx| {
                    ctx.register_effect(&svc("probe.extra"), Box::new(()))
                        .map(|_| ())
                }),
                // 2: unregistering the peer's effect — UnknownEffect (foreign).
                Box::new(move |ctx: &mut PluginContext| {
                    let id = stolen_probe
                        .lock()
                        .unwrap()
                        .expect("peer must publish its effect id first");
                    ctx.unregister_effect(id)
                }),
                // 3: unregistering a bogus id — UnknownEffect.
                Box::new(|ctx| ctx.unregister_effect(EffectId(u64::MAX))),
                // 4: subscribe on a bound *service* key — UndeclaredInterface.
                Box::new(|ctx| {
                    ctx.subscribe(&svc("peer.svc"), Box::new(|_| ()))
                        .map(|_| ())
                }),
                // 5: subscribe on an undeclared event key — UndeclaredInterface.
                Box::new(|ctx| ctx.subscribe(&evt("ghost"), Box::new(|_| ())).map(|_| ())),
                // 6: bound on an undeclared key — UnboundInterface.
                Box::new(|ctx| ctx.bound(&evt("ghost")).map(|_| ())),
            ];
            Box::new(RuleProber {
                declared: svc("probe.ok"),
                probes,
                outcomes: outcomes.clone(),
                seen_scope: seen_scope.clone(),
                seen_manifest_id: seen_id.clone(),
            })
        });
    }
    host.start(&["prober".to_string()]).unwrap();

    {
        let out = outcomes.lock().unwrap();
        assert_eq!(out.len(), 7);
        assert!(out[0].is_ok());
        assert!(matches!(out[1], Err(HostError::DuplicateEffect { .. })));
        assert!(matches!(out[2], Err(HostError::UnknownEffect { .. })));
        assert!(matches!(out[3], Err(HostError::UnknownEffect { .. })));
        assert!(matches!(out[4], Err(HostError::UndeclaredInterface { .. })));
        assert!(matches!(out[5], Err(HostError::UndeclaredInterface { .. })));
        assert!(matches!(out[6], Err(HostError::UnboundInterface { .. })));
    }

    // ctx.scope()/manifest() expose the host scope and the plugin's own
    // declaration snapshot.
    assert!(matches!(
        seen_scope.lock().unwrap().as_ref(),
        Some(ScopeKey::Host)
    ));
    assert_eq!(seen_id.lock().unwrap().as_deref(), Some("prober"));
    // The peer's effect survived the unregister attempt; exactly the three
    // expected effects are registered (peer.svc + probe.extra + probe.ok).
    assert_eq!(host.effects().len(), 3);
    assert_eq!(state_of(&host, "peer"), InstanceState::Active);
}

#[test]
fn emit_rejects_non_event_undeclared_and_unknown_emitter() {
    let mut host = builtin_host();
    register(
        &mut host,
        &manifest(
            "svc.only",
            "S",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    host.with_factory("svc.only", || {
        Box::new(EffectIdSharer {
            key: svc("s"),
            shared: Default::default(),
        })
    });
    host.start(&["svc.only".to_string()]).unwrap();

    // A declared provides key of kind service is not emittable.
    assert!(matches!(
        host.emit("svc.only", &svc("s"), Box::new(())),
        Err(HostError::UndeclaredInterface { .. })
    ));
    // An undeclared key is not emittable either.
    assert!(matches!(
        host.emit("svc.only", &evt("ghost"), Box::new(())),
        Err(HostError::UndeclaredInterface { .. })
    ));
    // Unknown emitter id.
    assert!(matches!(
        host.emit("ghost", &evt("ticks"), Box::new(())),
        Err(HostError::UnknownPlugin { .. })
    ));
}

#[test]
fn remove_descriptor_does_not_unload_running_instances() {
    let mut host = builtin_host();
    let service = Arc::new(CounterService {
        hits: AtomicUsize::new(0),
    });
    register(
        &mut host,
        &manifest(
            "svc.provider",
            "P",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "counter")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    {
        let service = service.clone();
        host.with_factory("svc.provider", move || {
            Box::new(SvcProvider {
                key: svc("counter"),
                service: service.clone(),
                log: Default::default(),
            })
        });
    }
    host.start(&["svc.provider".to_string()]).unwrap();
    let effects_before = host.effects();

    // Removing the catalog descriptor is a declaration change only — the
    // running instance keeps its effects and stays Active. It is not a real
    // unload.
    host.catalog_mut()
        .remove_descriptor("svc.provider")
        .unwrap();
    assert_eq!(state_of(&host, "svc.provider"), InstanceState::Active);
    assert_eq!(host.effects(), effects_before);

    // Resolution now reflects the missing declaration — the catalog change
    // is real, it just never touched the live registry.
    assert!(matches!(
        host.resolve(&["svc.provider".to_string()]),
        Err(HostError::Catalog(CatalogError::UnknownRoot { .. }))
    ));

    // Lifecycle still applies to the live instance.
    host.stop("svc.provider").unwrap();
    assert_eq!(state_of(&host, "svc.provider"), InstanceState::Stopped);
}

#[test]
fn failed_instances_reject_being_plan_members() {
    let mut host = builtin_host();
    register(
        &mut host,
        &manifest(
            "bad.act",
            "B",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "bad.svc")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "user",
            "U",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "bad.svc")]),
        ),
    );
    host.with_factory("bad.act", || Box::new(ErrOnActivate));
    host.with_factory("user", || Box::new(IdlePlugin));

    // The failing root lands Failed{Activate}.
    assert!(matches!(
        host.start(&["bad.act".to_string()]),
        Err(HostError::PluginStartError { .. })
    ));
    assert_eq!(
        state_of(&host, "bad.act"),
        InstanceState::Failed {
            stage: FailureStage::Activate
        }
    );

    // A Failed root cannot be restarted.
    assert!(matches!(
        host.start(&["bad.act".to_string()]),
        Err(HostError::InvalidState { .. })
    ));

    // A Failed *non-root* plan member is rejected the same way, before
    // anything else in that start ran.
    assert!(matches!(
        host.start(&["user".to_string()]),
        Err(HostError::InvalidState { .. })
    ));
    assert!(host.instances().iter().all(|s| s.id != "user"));
}

#[test]
fn restart_after_stop_builds_a_fresh_object() {
    let mut host = builtin_host();
    let builds = Arc::new(AtomicUsize::new(0));
    register(
        &mut host,
        &manifest(
            "solo",
            "S",
            "host",
            "builtin",
            provides_json(vec![key_json("event", "ticks")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    {
        let builds = builds.clone();
        host.with_factory("solo", move || {
            builds.fetch_add(1, Ordering::SeqCst);
            Box::new(EventProvider {
                key: evt("ticks"),
                log: Default::default(),
            })
        });
    }
    host.start(&["solo".to_string()]).unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    host.stop("solo").unwrap();
    assert_eq!(state_of(&host, "solo"), InstanceState::Stopped);
    // Stopped instances never reuse the dropped object: the factory runs
    // again on restart.
    host.start(&["solo".to_string()]).unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert_eq!(state_of(&host, "solo"), InstanceState::Active);
}

#[test]
fn start_reuses_active_non_root_members() {
    let mut host = builtin_host();
    let service = Arc::new(CounterService {
        hits: AtomicUsize::new(0),
    });
    register(
        &mut host,
        &manifest(
            "svc.provider",
            "P",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "counter")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    for consumer in ["cons.a", "cons.b"] {
        register(
            &mut host,
            &manifest(
                consumer,
                consumer,
                "host",
                "builtin",
                provides_json(vec![]),
                serde_json::Value::Array(vec![key_json("service", "counter")]),
            ),
        );
    }
    {
        let service = service.clone();
        host.with_factory("svc.provider", move || {
            Box::new(SvcProvider {
                key: svc("counter"),
                service: service.clone(),
                log: Default::default(),
            })
        });
    }
    for consumer in ["cons.a", "cons.b"] {
        let captured: Arc<Mutex<Vec<HostError>>> = Default::default();
        let log: SharedLog = Default::default();
        host.with_factory(consumer, move || {
            Box::new(SvcConsumer {
                need: svc("counter"),
                log: log.clone(),
                captured: captured.clone(),
            })
        });
    }

    let first = host.start(&["cons.a".to_string()]).unwrap();
    assert_eq!(
        ids(&first.activated),
        vec!["svc.provider".to_string(), "cons.a".to_string()]
    );
    assert!(first.reused.is_empty());
    let provider_effects: Vec<EffectId> = host
        .effects()
        .iter()
        .filter(|e| e.plugin_id == "svc.provider")
        .map(|e| e.effect_id)
        .collect();
    assert_eq!(provider_effects.len(), 1);

    // The second start resolves a plan that still contains the provider —
    // as a non-root it is reused, not re-activated.
    let second = host.start(&["cons.b".to_string()]).unwrap();
    assert_eq!(ids(&second.activated), vec!["cons.b".to_string()]);
    assert_eq!(ids(&second.reused), vec!["svc.provider".to_string()]);

    // The reused provider kept its live object and effect ids; both
    // consumers really called the same CounterService instance.
    let after: Vec<EffectId> = host
        .effects()
        .iter()
        .filter(|e| e.plugin_id == "svc.provider")
        .map(|e| e.effect_id)
        .collect();
    assert_eq!(provider_effects, after);
    assert_eq!(service.hits(), 2);
}

#[test]
fn stop_subtree_records_every_deactivate_failure_in_teardown_order() {
    let mut host = builtin_host();
    register(
        &mut host,
        &manifest(
            "base.p",
            "B",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "x")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "top.c",
            "T",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "x")]),
        ),
    );
    host.with_factory("base.p", || Box::new(ErrOnDeactivate { key: svc("x") }));
    host.with_factory("top.c", || Box::new(ErrOnDeactivateConsumer));
    host.start(&["top.c".to_string()]).unwrap();

    // Both teardowns fail; the error reports the first failure (the
    // consumer, unwound first) while the recovery list keeps the full
    // teardown order.
    let err = host.stop_subtree("base.p").unwrap_err();
    assert!(matches!(
        err,
        HostError::PluginDeactivateError { ref plugin_id } if plugin_id == "top.c"
    ));
    assert_eq!(
        state_of(&host, "top.c"),
        InstanceState::Failed {
            stage: FailureStage::Deactivate
        }
    );
    assert_eq!(
        state_of(&host, "base.p"),
        InstanceState::Failed {
            stage: FailureStage::Deactivate
        }
    );
    let recovery = host.pending_recovery();
    assert_eq!(
        recovery
            .iter()
            .map(|item| item.plugin_id.as_str())
            .collect::<Vec<_>>(),
        vec!["top.c", "base.p"]
    );
    assert!(host.effects().is_empty());
}

#[test]
fn rollback_records_deactivate_failures_without_replacing_the_start_error() {
    let mut host = builtin_host();
    register(
        &mut host,
        &manifest(
            "good.p",
            "G",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "s")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "bad.c",
            "B",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("service", "s")]),
        ),
    );
    host.with_factory("good.p", || Box::new(ErrOnDeactivate { key: svc("s") }));
    host.with_factory("bad.c", || Box::new(ErrOnActivate));

    let err = host.start(&["bad.c".to_string()]).unwrap_err();
    match err {
        HostError::PluginStartError {
            plugin_id, unwound, ..
        } => {
            assert_eq!(plugin_id, "bad.c");
            assert_eq!(ids(&unwound), vec!["good.p".to_string()]);
        }
        other => panic!("expected PluginStartError, got {other:?}"),
    }
    // The rollback teardown failure is still visible: recovery entry plus
    // Failed{Deactivate} state — never silently Stopped.
    assert_eq!(
        state_of(&host, "good.p"),
        InstanceState::Failed {
            stage: FailureStage::Deactivate
        }
    );
    let recovery = host.pending_recovery();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].plugin_id, "good.p");
    assert_eq!(recovery[0].stage, FailureStage::Deactivate);
    assert!(matches!(
        recovery[0].cause,
        PluginFailureCause::PluginError { .. }
    ));
    assert!(host.effects().is_empty());
}

#[test]
fn panicking_subscriber_fails_alone_and_delivery_continues() {
    let mut host = builtin_host();
    let inbox_a: Arc<Mutex<Vec<u64>>> = Default::default();
    let inbox_z: Arc<Mutex<Vec<u64>>> = Default::default();
    register(
        &mut host,
        &manifest(
            "ev.pub",
            "Pub",
            "host",
            "builtin",
            provides_json(vec![key_json("event", "ticks")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    for sub in ["sub.a", "sub.m", "sub.z"] {
        register(
            &mut host,
            &manifest(
                sub,
                sub,
                "host",
                "builtin",
                provides_json(vec![]),
                serde_json::Value::Array(vec![key_json("event", "ticks")]),
            ),
        );
    }
    host.with_factory("ev.pub", || {
        Box::new(EventProvider {
            key: evt("ticks"),
            log: Default::default(),
        })
    });
    {
        let inbox = inbox_a.clone();
        host.with_factory("sub.a", move || {
            Box::new(Subscriber {
                need: evt("ticks"),
                inbox: inbox.clone(),
                order: Default::default(),
                log: Default::default(),
            })
        });
    }
    host.with_factory("sub.m", || Box::new(PanicSubscriber { need: evt("ticks") }));
    {
        let inbox = inbox_z.clone();
        host.with_factory("sub.z", move || {
            Box::new(Subscriber {
                need: evt("ticks"),
                inbox: inbox.clone(),
                order: Default::default(),
                log: Default::default(),
            })
        });
    }
    host.start(&[
        "sub.a".to_string(),
        "sub.m".to_string(),
        "sub.z".to_string(),
    ])
    .unwrap();

    // Delivery order is ascending subscriber id: sub.a, then sub.m panics,
    // then sub.z still receives — the panic is isolated per subscriber.
    let delivered = host.emit("ev.pub", &evt("ticks"), Box::new(1u64)).unwrap();
    assert_eq!(delivered, 2);
    assert_eq!(*inbox_a.lock().unwrap(), vec![1u64]);
    assert_eq!(*inbox_z.lock().unwrap(), vec![1u64]);

    // The panicking subscriber alone is Failed{Deliver}, on the recovery
    // list, and stripped of its effects. Emitter and healthy subscribers
    // stay Active.
    assert_eq!(
        state_of(&host, "sub.m"),
        InstanceState::Failed {
            stage: FailureStage::Deliver
        }
    );
    assert_eq!(state_of(&host, "ev.pub"), InstanceState::Active);
    assert_eq!(state_of(&host, "sub.a"), InstanceState::Active);
    assert_eq!(state_of(&host, "sub.z"), InstanceState::Active);
    let recovery = host.pending_recovery();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].plugin_id, "sub.m");
    assert_eq!(recovery[0].stage, FailureStage::Deliver);
    assert_eq!(recovery[0].cause, PluginFailureCause::Panic);
    assert!(
        host.effects()
            .iter()
            .all(|entry| entry.plugin_id != "sub.m")
    );

    // Later emits still reach the healthy subscribers only.
    let delivered = host.emit("ev.pub", &evt("ticks"), Box::new(2u64)).unwrap();
    assert_eq!(delivered, 2);
    assert_eq!(*inbox_a.lock().unwrap(), vec![1u64, 2u64]);
    assert_eq!(*inbox_z.lock().unwrap(), vec![1u64, 2u64]);

    // A Failed subscriber cannot restart in place; it unloads cleanly.
    assert!(matches!(
        host.start(&["sub.m".to_string()]),
        Err(HostError::InvalidState { .. })
    ));
    host.unload("sub.m").unwrap();
    assert!(host.instances().iter().all(|s| s.id != "sub.m"));
    assert_eq!(host.pending_recovery().len(), 1);
}

#[test]
fn subscribers_receive_only_their_bound_providers_events() {
    let mut host = builtin_host();
    let inbox: Arc<Mutex<Vec<u64>>> = Default::default();
    register(
        &mut host,
        &manifest(
            "ev.a",
            "A",
            "host",
            "builtin",
            provides_json(vec![key_json("event", "ticks")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "sub.c",
            "C",
            "host",
            "builtin",
            provides_json(vec![]),
            serde_json::Value::Array(vec![key_json("event", "ticks")]),
        ),
    );
    host.with_factory("ev.a", || {
        Box::new(EventProvider {
            key: evt("ticks"),
            log: Default::default(),
        })
    });
    {
        let inbox = inbox.clone();
        host.with_factory("sub.c", move || {
            Box::new(Subscriber {
                need: evt("ticks"),
                inbox: inbox.clone(),
                order: Default::default(),
                log: Default::default(),
            })
        });
    }
    // C binds to ev.a while ev.b does not exist yet.
    host.start(&["sub.c".to_string()]).unwrap();

    // A second provider of the same event key registers and activates on its
    // own root — its emits must not reach C's A-bound subscription.
    register(
        &mut host,
        &manifest(
            "ev.b",
            "B",
            "host",
            "builtin",
            provides_json(vec![key_json("event", "ticks")]),
            serde_json::Value::Array(vec![]),
        ),
    );
    host.with_factory("ev.b", || {
        Box::new(EventProvider {
            key: evt("ticks"),
            log: Default::default(),
        })
    });
    host.start(&["ev.b".to_string()]).unwrap();

    let delivered = host.emit("ev.b", &evt("ticks"), Box::new(1u64)).unwrap();
    assert_eq!(delivered, 0);
    assert!(inbox.lock().unwrap().is_empty());
    let delivered = host.emit("ev.a", &evt("ticks"), Box::new(2u64)).unwrap();
    assert_eq!(delivered, 1);
    assert_eq!(*inbox.lock().unwrap(), vec![2u64]);
}

#[test]
fn n08_host_typed_resolver_obeys_live_activation_binding_and_payload_type() {
    let mut host = builtin_host();
    let service = Arc::new(CounterService {
        hits: AtomicUsize::new(0),
    });
    let log = Arc::new(Mutex::new(vec![]));
    register(
        &mut host,
        &manifest(
            "svc.provider",
            "provider",
            "host",
            "builtin",
            provides_json(vec![key_json("service", "counter")]),
            serde_json::json!([]),
        ),
    );
    register(
        &mut host,
        &manifest(
            "svc.consumer",
            "consumer",
            "host",
            "builtin",
            serde_json::json!([]),
            provides_json(vec![key_json("service", "counter")]),
        ),
    );
    let provider_service = service.clone();
    let provider_log = log.clone();
    host.with_factory("svc.provider", move || {
        Box::new(SvcProvider {
            key: svc("counter"),
            service: provider_service.clone(),
            log: provider_log.clone(),
        })
    });
    let consumer_log = log.clone();
    host.with_factory("svc.consumer", move || {
        Box::new(SvcConsumer {
            need: svc("counter"),
            log: consumer_log.clone(),
            captured: Arc::new(Mutex::new(vec![])),
        })
    });
    host.start(&["svc.consumer".into()]).unwrap();
    let resolved = host
        .resolve_bound::<Arc<CounterService>>("svc.consumer", &svc("counter"))
        .unwrap();
    assert_eq!(resolved.provider_id, "svc.provider");
    assert_eq!(resolved.key, svc("counter"));
    assert!(Arc::ptr_eq(&resolved.payload, &service));
    assert!(
        host.effects()
            .iter()
            .any(|effect| effect.effect_id == resolved.effect_id)
    );
    resolved.payload.hit();
    assert_eq!(service.hits(), 2);
    assert!(
        matches!(host.resolve_bound::<String>("svc.consumer", &svc("counter")),
        Err(HostError::PayloadTypeMismatch { plugin_id, .. }) if plugin_id == "svc.provider")
    );
    let mut wrong_version = svc("counter");
    wrong_version.version.major = 2;
    for key in [wrong_version, svc("undeclared")] {
        assert!(matches!(
            host.resolve_bound::<Arc<CounterService>>("svc.consumer", &key),
            Err(HostError::UnboundInterface { .. })
        ));
    }
    // Removing declarations does not rebind the live activation snapshot.
    host.catalog_mut()
        .remove_descriptor("svc.provider")
        .unwrap();
    assert!(Arc::ptr_eq(
        &host
            .resolve_bound::<Arc<CounterService>>("svc.consumer", &svc("counter"))
            .unwrap()
            .payload,
        &service
    ));
    host.stop_subtree("svc.provider").unwrap();
    assert!(matches!(
        host.resolve_bound::<Arc<CounterService>>("svc.consumer", &svc("counter")),
        Err(HostError::InvalidState {
            current: InstanceState::Stopped,
            ..
        })
    ));
    assert!(host.effects().is_empty());
    assert!(matches!(
        builtin_host().resolve_bound::<Arc<CounterService>>("svc.consumer", &svc("counter")),
        Err(HostError::UnknownPlugin { .. })
    ));
}
