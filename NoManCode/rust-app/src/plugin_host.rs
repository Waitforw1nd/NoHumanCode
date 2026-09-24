//! In-memory plugin host: scoped registry, builtin lifecycle, and revocable
//! effects.
//!
//! A [`PluginHost`] owns one [`PluginCatalog`] bound to a [`ScopeKey`]. `start`
//! resolves the catalog at call time and activates `builtin` plugins in plan
//! order through caller-injected factories; each plugin receives a restricted
//! [`PluginContext`] valid only for the duration of `activate`/`deactivate`.
//! Effects a plugin registers are tracked per owner and revoked by the host on
//! deactivate, rollback, and unload. Failures are typed [`HostError`] values;
//! plugins that report incomplete cleanup land on an explicit recovery list
//! while the registry itself is always left residue-free.
//!
//! Scope of this slice: only `builtin` runtimes execute (wasm/process are
//! declared but rejected as [`HostError::UnsupportedRuntime`]). The host never
//! touches files, network, databases, processes, or the environment, and
//! `display_name` is never identity.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::plugin_catalog::{
    CatalogError, CatalogManifestV1, DependencyBinding, ExactVersion, InterfaceKey, InterfaceKind,
    PluginCatalog, PluginRef, ResolutionPlan, RuntimeKind, ScopeKey,
};

// --- Errors ------------------------------------------------------------------

/// Why a plugin instance failed. Carried inside
/// `HostError::PluginStartError::cause` and recovery entries.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum PluginFailureCause {
    /// The plugin's own `activate`/`deactivate` returned `Err`.
    PluginError { message: String },
    /// The plugin panicked inside a guarded call (lifecycle hook or event
    /// subscription callback).
    Panic,
    /// `activate` ended without registering every declared provides key.
    ProvidesNotCovered { missing: Vec<InterfaceKey> },
}

impl fmt::Display for PluginFailureCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginFailureCause::PluginError { message } => {
                write!(f, "插件返回错误：{message}")
            }
            PluginFailureCause::Panic => write!(f, "插件调用发生 panic"),
            PluginFailureCause::ProvidesNotCovered { missing } => {
                write!(f, "provides 未全部注册 effect：{missing:?}")
            }
        }
    }
}

/// Which lifecycle stage a failure/panic occurred in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureStage {
    Activate,
    Deactivate,
    /// An event subscription callback panicked during delivery.
    Deliver,
}

impl fmt::Display for FailureStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            FailureStage::Activate => "activate",
            FailureStage::Deactivate => "deactivate",
            FailureStage::Deliver => "deliver",
        };
        f.write_str(name)
    }
}

/// Lifecycle states. `Failed { stage }` records where the failure happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceState {
    /// Instance record created, object not yet obtained.
    Registered,
    /// Factory produced an object; not yet activated.
    Loaded,
    /// `activate` completed and provides coverage verified.
    Active,
    /// Deactivated and effects revoked; object dropped.
    Stopped,
    /// Instance record removed.
    Unloaded,
    /// Guarded call failed; cleanup may be incomplete (see recovery list).
    Failed { stage: FailureStage },
}

impl fmt::Display for InstanceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            InstanceState::Registered => "registered",
            InstanceState::Loaded => "loaded",
            InstanceState::Active => "active",
            InstanceState::Stopped => "stopped",
            InstanceState::Unloaded => "unloaded",
            InstanceState::Failed { .. } => "failed",
        };
        f.write_str(name)
    }
}

/// Typed host failures. Start-path failures additionally report which plugins
/// were rolled back (`unwound`, plan-reverse order).
#[derive(Clone, Debug, PartialEq)]
pub enum HostError {
    /// Resolution/validation failed inside the catalog; evidence preserved.
    Catalog(CatalogError),
    /// `runtime` is wasm/process — declared but not executable here.
    UnsupportedRuntime {
        plugin_id: String,
        /// Plugins activated earlier in this start, already rolled back.
        unwound: Vec<PluginRef>,
    },
    /// `runtime == builtin` but no factory was injected for this id.
    MissingFactory {
        plugin_id: String,
        unwound: Vec<PluginRef>,
    },
    /// `ctx.bound`/`ctx.subscribe` on a key that is not declared in the
    /// plugin's `requires` or has no binding in the activation plan.
    UnboundInterface {
        plugin_id: String,
        key: InterfaceKey,
    },
    /// `ctx.register_effect`/`ctx.emit`/`ctx.subscribe` on a key outside the
    /// declaration rules (not in `provides`, not in `requires`, or a
    /// non-event key used as an event).
    UndeclaredInterface {
        plugin_id: String,
        key: InterfaceKey,
    },
    /// Same plugin registering the same provides key twice.
    DuplicateEffect {
        plugin_id: String,
        key: InterfaceKey,
    },
    /// `ctx.unregister_effect` on an unknown or foreign effect id.
    UnknownEffect {
        plugin_id: String,
        effect_id: EffectId,
    },
    /// `activate` returned while some declared provides keys have no effect.
    ProvidesNotCovered {
        plugin_id: String,
        missing: Vec<InterfaceKey>,
        unwound: Vec<PluginRef>,
    },
    /// `stop` refused: the plugin still has Active consumers.
    DependentsActive {
        plugin_id: String,
        dependents: Vec<String>,
    },
    /// Lifecycle transition not legal from the current state.
    InvalidState {
        plugin_id: String,
        current: InstanceState,
    },
    /// No instance with this id is known to this host.
    UnknownPlugin { id: String },
    /// `activate` failed (plugin error or context violation); earlier
    /// activations were rolled back in plan-reverse order.
    PluginStartError {
        plugin_id: String,
        stage: FailureStage,
        cause: Box<PluginFailureCause>,
        unwound: Vec<PluginRef>,
    },
    /// `deactivate` returned `Err`; the instance is `Failed` and on the
    /// recovery list, but its registry effects were still revoked.
    PluginDeactivateError { plugin_id: String },
    /// A guarded plugin call panicked; the instance is `Failed`.
    PluginPanic {
        plugin_id: String,
        stage: FailureStage,
    },
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Catalog(error) => write!(f, "目录解析失败：{error}"),
            HostError::UnsupportedRuntime { plugin_id, .. } => {
                write!(f, "插件 {plugin_id} 声明的运行时不支持执行")
            }
            HostError::MissingFactory { plugin_id, .. } => {
                write!(f, "插件 {plugin_id} 缺少 builtin 工厂")
            }
            HostError::UnboundInterface { plugin_id, key } => {
                write!(f, "插件 {plugin_id} 的接口无绑定：{key}")
            }
            HostError::UndeclaredInterface { plugin_id, key } => {
                write!(f, "插件 {plugin_id} 使用了未声明的接口：{key}")
            }
            HostError::DuplicateEffect { plugin_id, key } => {
                write!(f, "插件 {plugin_id} 重复注册接口：{key}")
            }
            HostError::UnknownEffect {
                plugin_id,
                effect_id,
            } => {
                write!(f, "插件 {plugin_id} 注销了不属于它的 effect {effect_id:?}")
            }
            HostError::ProvidesNotCovered {
                plugin_id, missing, ..
            } => {
                write!(
                    f,
                    "插件 {plugin_id} 的 provides 未全部注册 effect：{missing:?}"
                )
            }
            HostError::DependentsActive {
                plugin_id,
                dependents,
            } => {
                write!(f, "插件 {plugin_id} 仍有活动的依赖者：{dependents:?}")
            }
            HostError::InvalidState { plugin_id, current } => {
                write!(f, "插件 {plugin_id} 当前状态 {current} 不允许该操作")
            }
            HostError::UnknownPlugin { id } => write!(f, "宿主中不存在插件实例：{id}"),
            HostError::PluginStartError {
                plugin_id,
                stage,
                cause,
                ..
            } => {
                write!(f, "插件 {plugin_id} 在 {stage} 阶段启动失败：{cause}")
            }
            HostError::PluginDeactivateError { plugin_id } => {
                write!(f, "插件 {plugin_id} 的 deactivate 返回错误")
            }
            HostError::PluginPanic { plugin_id, stage } => {
                write!(f, "插件 {plugin_id} 在 {stage} 阶段发生 panic")
            }
        }
    }
}

impl std::error::Error for HostError {}

impl From<CatalogError> for HostError {
    fn from(error: CatalogError) -> Self {
        HostError::Catalog(error)
    }
}

// --- Plugin-side surface -----------------------------------------------------

/// Typed error a builtin plugin returns from `activate`/`deactivate`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError {
    pub message: String,
}

impl PluginError {
    pub fn new(message: impl Into<String>) -> Self {
        PluginError {
            message: message.into(),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}

/// A builtin plugin object. Factories hand a fresh instance to the host on
/// every `Loaded → Active` transition; a stopped plugin's object is dropped
/// and never reused.
pub trait BuiltinPlugin: Send {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError>;
    fn deactivate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError>;
}

/// Deterministic effect identifier, monotonically increasing per host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct EffectId(pub u64);

/// Read-only view of a bound provider effect handed to `ctx.bound` callers.
/// The consumer downcasts `payload` to the concrete type it expects.
pub struct BoundInterface<'a> {
    key: &'a InterfaceKey,
    provider_id: &'a str,
    payload: &'a dyn Any,
}

impl<'a> BoundInterface<'a> {
    /// The requirement key this binding satisfies.
    pub fn key(&self) -> &'a InterfaceKey {
        self.key
    }

    /// Stable id of the providing plugin.
    pub fn provider_id(&self) -> &'a str {
        self.provider_id
    }

    /// The provider-registered payload; downcast via `Any`.
    pub fn payload(&self) -> &'a dyn Any {
        self.payload
    }
}

/// Event subscription callback; receives the emitted value as `&dyn Any`.
type EventCallback = Box<dyn Fn(&dyn Any) + Send>;

/// Opaque registered effect: either a provided capability payload or an
/// event subscription owned by a consumer plugin.
enum EffectKind {
    /// Provided capability payload.
    Provide(Box<dyn Any + Send>),
    /// Event subscription owned by the consumer plugin for one of its
    /// declared+bound requires keys. `provider_id` is the plugin this
    /// consumer was bound to at subscribe time — delivery is filtered by
    /// emitter so a later same-key provider cannot cross-talk into this
    /// subscription.
    Subscription {
        provider_id: String,
        callback: EventCallback,
    },
}

struct EffectEntry {
    id: EffectId,
    owner: String,
    key: InterfaceKey,
    kind: EffectKind,
}

/// Restricted handle valid only inside `activate`/`deactivate`. It is not a
/// global service locator: `bound` only resolves requires keys declared in the
/// plugin's own manifest and bound in the activation plan; `register_effect`
/// only accepts the plugin's own declared provides keys.
pub struct PluginContext<'a> {
    host: &'a mut PluginHost,
    plugin_id: String,
    /// Manifest snapshot captured at activation; later catalog edits do not
    /// leak into a running instance's rules.
    manifest: CatalogManifestV1,
    /// Plan snapshot captured at activation: bindings consulted by `bound`
    /// and `subscribe` are the ones this instance was activated against.
    plan: ResolutionPlan,
}

impl<'a> PluginContext<'a> {
    /// Stable plugin id of the owner of this context.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// The scope this host (and therefore this plugin) is bound to.
    pub fn scope(&self) -> &ScopeKey {
        self.host.catalog().scope()
    }

    /// The plugin's own declaration snapshot as of activation.
    pub fn manifest(&self) -> &CatalogManifestV1 {
        &self.manifest
    }

    fn binding_for(&self, key: &InterfaceKey) -> Option<&DependencyBinding> {
        self.plan
            .bindings
            .iter()
            .find(|binding| binding.consumer_id == self.plugin_id && binding.requirement == *key)
    }

    /// Read-only access to the provider effect satisfying a declared+bound
    /// requires key. `UnboundInterface` when the key is not declared in
    /// `requires`, has no binding in the activation plan, or the provider has
    /// no live effect for it.
    pub fn bound(&self, key: &InterfaceKey) -> Result<BoundInterface<'_>, HostError> {
        if !self.manifest.requires.contains(key) {
            return Err(HostError::UnboundInterface {
                plugin_id: self.plugin_id.clone(),
                key: key.clone(),
            });
        }
        let binding =
            self.binding_for(key)
                .cloned()
                .ok_or_else(|| HostError::UnboundInterface {
                    plugin_id: self.plugin_id.clone(),
                    key: key.clone(),
                })?;
        let entry = self
            .host
            .provides
            .values()
            .find(|entry| entry.owner == binding.provider_id && entry.key == *key)
            .ok_or_else(|| HostError::UnboundInterface {
                plugin_id: self.plugin_id.clone(),
                key: key.clone(),
            })?;
        let payload: &dyn Any = match &entry.kind {
            EffectKind::Provide(payload) => payload.as_ref(),
            // Subscriptions never surface as provide records.
            EffectKind::Subscription { .. } => {
                return Err(HostError::UnboundInterface {
                    plugin_id: self.plugin_id.clone(),
                    key: key.clone(),
                });
            }
        };
        Ok(BoundInterface {
            key: &entry.key,
            provider_id: &entry.owner,
            payload,
        })
    }

    /// Register an effect for a declared provides key.
    /// `UndeclaredInterface` if the key is not in `manifest.provides`;
    /// `DuplicateEffect` if this plugin already registered it.
    pub fn register_effect(
        &mut self,
        key: &InterfaceKey,
        payload: Box<dyn Any + Send>,
    ) -> Result<EffectId, HostError> {
        if !self.manifest.provides.contains(key) {
            return Err(HostError::UndeclaredInterface {
                plugin_id: self.plugin_id.clone(),
                key: key.clone(),
            });
        }
        if self
            .host
            .provides
            .values()
            .any(|entry| entry.owner == self.plugin_id && entry.key == *key)
        {
            return Err(HostError::DuplicateEffect {
                plugin_id: self.plugin_id.clone(),
                key: key.clone(),
            });
        }
        let id = self.host.next_effect_id();
        self.host.provides.insert(
            id,
            EffectEntry {
                id,
                owner: self.plugin_id.clone(),
                key: key.clone(),
                kind: EffectKind::Provide(payload),
            },
        );
        Ok(id)
    }

    /// Unregister an effect owned by this plugin. The host force-revokes any
    /// leftovers after `deactivate`, so this is for eager cleanup only.
    /// `UnknownEffect` for ids this plugin does not own.
    pub fn unregister_effect(&mut self, effect_id: EffectId) -> Result<(), HostError> {
        for owned in self
            .host
            .provides
            .values()
            .chain(self.host.subscriptions.values())
            .filter(|entry| entry.owner == self.plugin_id)
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
        {
            if owned == effect_id {
                self.host.provides.remove(&effect_id);
                self.host.subscriptions.remove(&effect_id);
                return Ok(());
            }
        }
        Err(HostError::UnknownEffect {
            plugin_id: self.plugin_id.clone(),
            effect_id,
        })
    }

    /// Subscribe to a bound event-kind requires key. The subscription is an
    /// effect owned by this (consumer) plugin, so it is revoked when the
    /// consumer stops. `UndeclaredInterface` when the key is not declared in
    /// `requires` or is not of kind `event`; `UnboundInterface` when it has
    /// no binding in the activation plan.
    pub fn subscribe(
        &mut self,
        key: &InterfaceKey,
        callback: EventCallback,
    ) -> Result<EffectId, HostError> {
        if !self.manifest.requires.contains(key) {
            return Err(HostError::UndeclaredInterface {
                plugin_id: self.plugin_id.clone(),
                key: key.clone(),
            });
        }
        let provider_id = match self.binding_for(key) {
            Some(binding) => binding.provider_id.clone(),
            None => {
                return Err(HostError::UnboundInterface {
                    plugin_id: self.plugin_id.clone(),
                    key: key.clone(),
                });
            }
        };
        if key.kind != InterfaceKind::Event {
            return Err(HostError::UndeclaredInterface {
                plugin_id: self.plugin_id.clone(),
                key: key.clone(),
            });
        }
        let id = self.host.next_effect_id();
        self.host.subscriptions.insert(
            id,
            EffectEntry {
                id,
                owner: self.plugin_id.clone(),
                key: key.clone(),
                kind: EffectKind::Subscription {
                    provider_id,
                    callback,
                },
            },
        );
        Ok(id)
    }

    /// Emit an event on a declared provides key of kind `event`. Subscribers
    /// are invoked synchronously in ascending subscriber plugin-id order;
    /// returns the delivery count. Subscriptions created after this emit are
    /// never back-filled.
    pub fn emit(
        &mut self,
        key: &InterfaceKey,
        value: Box<dyn Any + Send>,
    ) -> Result<usize, HostError> {
        if !self.manifest.provides.contains(key) {
            return Err(HostError::UndeclaredInterface {
                plugin_id: self.plugin_id.clone(),
                key: key.clone(),
            });
        }
        if key.kind != InterfaceKind::Event {
            return Err(HostError::UndeclaredInterface {
                plugin_id: self.plugin_id.clone(),
                key: key.clone(),
            });
        }
        let emitter = self.plugin_id.clone();
        Ok(self.host.deliver_event(&emitter, key, value.as_ref()))
    }
}

// --- Read-only snapshots ------------------------------------------------------

/// Deterministic snapshot of one instance (sorted by id in `instances()`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InstanceSnapshot {
    pub id: String,
    pub version: ExactVersion,
    pub state: InstanceState,
    /// Effect ids owned by this plugin (provides + subscriptions), sorted.
    pub effect_ids: Vec<EffectId>,
}

/// Snapshot of one registered effect (no payload internals).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EffectRecord {
    pub effect_id: EffectId,
    pub plugin_id: String,
    pub key: InterfaceKey,
    /// `true` for event subscriptions, `false` for provided capabilities.
    pub is_subscription: bool,
}

/// Entry on the explicit recovery list: a plugin reported (or panicked
/// through) incomplete cleanup. Registry effects were revoked regardless;
/// this list is bookkeeping for manual/later-slice recovery — the host never
/// retries automatically.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecoveryItem {
    pub plugin_id: String,
    pub stage: FailureStage,
    pub cause: PluginFailureCause,
}

/// Result of `start`: plugins activated this call, in activation order, plus
/// already-Active non-root plugins reused without re-activation.
#[derive(Clone, Debug, PartialEq)]
pub struct StartReport {
    pub activated: Vec<PluginRef>,
    pub reused: Vec<PluginRef>,
}

// --- Host internals -----------------------------------------------------------

struct Instance {
    manifest: CatalogManifestV1,
    plan: ResolutionPlan,
    state: InstanceState,
    plugin: Option<Box<dyn BuiltinPlugin>>,
}

/// In-memory host: one scope, one catalog, one registry. No IO.
pub struct PluginHost {
    catalog: PluginCatalog,
    factories: BTreeMap<String, Box<dyn Fn() -> Box<dyn BuiltinPlugin> + Send>>,
    instances: BTreeMap<String, Instance>,
    /// Provided capabilities, indexed by effect id.
    provides: BTreeMap<EffectId, EffectEntry>,
    /// Event subscriptions owned by consumer plugins, indexed by effect id.
    subscriptions: BTreeMap<EffectId, EffectEntry>,
    recovery: Vec<RecoveryItem>,
    next_effect: AtomicU64,
    /// Activation stack used for reverse-order teardown.
    activation_order: Vec<String>,
}

impl PluginHost {
    /// Create a host with an empty catalog bound to `scope` (validated by the
    /// same rules as `PluginCatalog::new`).
    pub fn new(scope: ScopeKey) -> Result<Self, HostError> {
        let catalog = PluginCatalog::new(scope).map_err(HostError::Catalog)?;
        Ok(PluginHost {
            catalog,
            factories: BTreeMap::new(),
            instances: BTreeMap::new(),
            provides: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            recovery: Vec::new(),
            next_effect: AtomicU64::new(1),
            activation_order: Vec::new(),
        })
    }

    pub fn catalog(&self) -> &PluginCatalog {
        &self.catalog
    }

    /// Mutable catalog access. Removing a descriptor is a declaration-level
    /// operation: running instances keep their activation-time snapshot and
    /// only the next `start` re-resolves.
    pub fn catalog_mut(&mut self) -> &mut PluginCatalog {
        &mut self.catalog
    }

    /// Inject the builtin factory for one stable plugin id.
    pub fn with_factory(
        &mut self,
        id: impl Into<String>,
        factory: impl Fn() -> Box<dyn BuiltinPlugin> + Send + 'static,
    ) -> &mut Self {
        self.factories.insert(id.into(), Box::new(factory));
        self
    }

    /// Resolve the current catalog snapshot without executing anything.
    pub fn resolve(&self, roots: &[String]) -> Result<ResolutionPlan, HostError> {
        self.catalog.resolve(roots).map_err(HostError::Catalog)
    }

    fn next_effect_id(&self) -> EffectId {
        EffectId(self.next_effect.fetch_add(1, Ordering::Relaxed))
    }

    fn effect_ids_of(&self, plugin_id: &str) -> Vec<EffectId> {
        self.provides
            .values()
            .chain(self.subscriptions.values())
            .filter(|entry| entry.owner == plugin_id)
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Revoke every effect owned by `plugin_id` (provides + subscriptions).
    /// Always leaves the registry residue-free for that owner.
    fn revoke_effects(&mut self, plugin_id: &str) {
        self.provides.retain(|_, entry| entry.owner != plugin_id);
        self.subscriptions
            .retain(|_, entry| entry.owner != plugin_id);
    }

    /// Deliver an event value emitted by `emitter_id` to the current
    /// subscribers of `key`, in ascending subscriber plugin-id order. Only
    /// subscriptions bound to `emitter_id` as their provider receive the
    /// event, so same-key providers never cross-talk. Each callback runs
    /// under `catch_unwind`: a panicking subscriber is marked
    /// `Failed{Deliver}`, loses its effects and activation slot, and is
    /// appended to the recovery list — the emitter and healthy subscribers
    /// are unaffected. Returns the number of successful deliveries.
    fn deliver_event(&mut self, emitter_id: &str, key: &InterfaceKey, value: &dyn Any) -> usize {
        let mut subscribers: Vec<(String, EffectId)> = self
            .subscriptions
            .values()
            .filter(|entry| entry.key == *key)
            .map(|entry| (entry.owner.clone(), entry.id))
            .collect();
        subscribers.sort();
        let mut delivered = 0usize;
        let mut panicked: Vec<String> = Vec::new();
        for (owner, effect_id) in subscribers {
            let Some(entry) = self.subscriptions.get(&effect_id) else {
                continue;
            };
            let EffectKind::Subscription {
                provider_id,
                callback,
            } = &entry.kind
            else {
                continue;
            };
            if provider_id.as_str() != emitter_id {
                continue;
            }
            if catch_unwind(AssertUnwindSafe(|| callback(value))).is_ok() {
                delivered += 1;
            } else {
                panicked.push(owner);
            }
        }
        for owner in panicked {
            if let Some(instance) = self.instances.get_mut(&owner) {
                instance.state = InstanceState::Failed {
                    stage: FailureStage::Deliver,
                };
                instance.plugin = None;
            }
            self.revoke_effects(&owner);
            self.activation_order.retain(|member| member != &owner);
            self.recovery.push(RecoveryItem {
                plugin_id: owner,
                stage: FailureStage::Deliver,
                cause: PluginFailureCause::Panic,
            });
        }
        delivered
    }

    /// Emit an event as the host on behalf of `plugin_id` — the delivery pump
    /// for event-providing plugins outside `activate`/`deactivate`. `key`
    /// must be declared in the plugin's manifest provides and of kind
    /// `event`.
    pub fn emit(
        &mut self,
        plugin_id: &str,
        key: &InterfaceKey,
        value: Box<dyn Any + Send>,
    ) -> Result<usize, HostError> {
        let instance = self
            .instances
            .get(plugin_id)
            .ok_or_else(|| HostError::UnknownPlugin {
                id: plugin_id.to_string(),
            })?;
        // A stopped/unloaded provider no longer produces events.
        if instance.state != InstanceState::Active {
            return Err(HostError::InvalidState {
                plugin_id: plugin_id.to_string(),
                current: instance.state,
            });
        }
        if !instance.manifest.provides.contains(key) {
            return Err(HostError::UndeclaredInterface {
                plugin_id: plugin_id.to_string(),
                key: key.clone(),
            });
        }
        if key.kind != InterfaceKind::Event {
            return Err(HostError::UndeclaredInterface {
                plugin_id: plugin_id.to_string(),
                key: key.clone(),
            });
        }
        Ok(self.deliver_event(plugin_id, key, value.as_ref()))
    }

    /// Run `f` guarded by `catch_unwind`: `Ok(Ok(()))` clean, `Ok(Err(_))`
    /// typed plugin error, `Err(_)` panic.
    fn guarded<F>(f: F) -> Result<Result<(), PluginError>, Box<dyn Any + Send>>
    where
        F: FnOnce() -> Result<(), PluginError>,
    {
        catch_unwind(AssertUnwindSafe(f))
    }

    /// Build the restricted context for one lifecycle call.
    fn context_for(
        &mut self,
        plugin_id: &str,
        manifest: &CatalogManifestV1,
        plan: &ResolutionPlan,
    ) -> PluginContext<'_> {
        PluginContext {
            host: self,
            plugin_id: plugin_id.to_string(),
            manifest: manifest.clone(),
            plan: plan.clone(),
        }
    }

    /// Deactivate one instance: guarded `deactivate`, then unconditional
    /// effect revocation. The instance object is always dropped. On plugin
    /// error/panic the recovery list gains an entry and the returned state is
    /// `Failed{Deactivate}` together with the recorded cause; otherwise
    /// `Stopped` with no cause.
    fn teardown_instance(
        &mut self,
        plugin_id: &str,
    ) -> (InstanceState, Option<PluginFailureCause>) {
        let (plugin, manifest, plan) = {
            let Some(instance) = self.instances.get_mut(plugin_id) else {
                return (InstanceState::Stopped, None);
            };
            (
                instance.plugin.take(),
                instance.manifest.clone(),
                instance.plan.clone(),
            )
        };
        let mut failure: Option<PluginFailureCause> = None;
        if let Some(mut object) = plugin {
            let outcome = {
                let mut ctx = self.context_for(plugin_id, &manifest, &plan);
                Self::guarded(|| object.deactivate(&mut ctx))
            };
            failure = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(PluginFailureCause::PluginError {
                    message: error.message,
                }),
                Err(_) => Some(PluginFailureCause::Panic),
            };
        }
        self.revoke_effects(plugin_id);
        self.activation_order.retain(|member| member != plugin_id);
        match failure {
            None => (InstanceState::Stopped, None),
            Some(cause) => {
                self.recovery.push(RecoveryItem {
                    plugin_id: plugin_id.to_string(),
                    stage: FailureStage::Deactivate,
                    cause: cause.clone(),
                });
                (
                    InstanceState::Failed {
                        stage: FailureStage::Deactivate,
                    },
                    Some(cause),
                )
            }
        }
    }

    /// Tear down the given plugins in reverse order (consumers first).
    /// Returns the rolled-back refs in unwind order. Deactivation failures
    /// during rollback still land on the recovery list but never abort the
    /// unwind.
    fn rollback(&mut self, activated: &[PluginRef]) -> Vec<PluginRef> {
        let mut unwound = Vec::new();
        for plugin_ref in activated.iter().rev() {
            let (state, _cause) = self.teardown_instance(&plugin_ref.id);
            if let Some(instance) = self.instances.get_mut(&plugin_ref.id) {
                instance.state = state;
            }
            unwound.push(plugin_ref.clone());
        }
        unwound
    }

    /// Resolve roots, then activate each plan member in order. `builtin`
    /// plugins go through their injected factory; wasm/process are rejected
    /// as `UnsupportedRuntime`. On any failure the instances activated
    /// earlier in this call are torn down in plan-reverse order and listed in
    /// `unwound`.
    pub fn start(&mut self, roots: &[String]) -> Result<StartReport, HostError> {
        let plan = self.catalog.resolve(roots).map_err(HostError::Catalog)?;
        // Repeat-start guard: an already-Active or Failed *root* is a usage
        // error reported before anything new is activated.
        for root in roots {
            if let Some(instance) = self.instances.get(root.as_str()) {
                match instance.state {
                    InstanceState::Active | InstanceState::Failed { .. } => {
                        return Err(HostError::InvalidState {
                            plugin_id: root.clone(),
                            current: instance.state,
                        });
                    }
                    _ => {}
                }
            }
        }
        let root_set: BTreeSet<&str> = roots.iter().map(String::as_str).collect();
        let mut activated: Vec<PluginRef> = Vec::new();
        let mut reused: Vec<PluginRef> = Vec::new();

        for plugin_ref in &plan.ordered_plugins {
            let id = plugin_ref.id.as_str();
            if let Some(instance) = self.instances.get(id) {
                let current = instance.state;
                match current {
                    InstanceState::Active => {
                        if root_set.contains(id) {
                            // Unreachable: pre-scan rejects Active roots.
                            return Err(HostError::InvalidState {
                                plugin_id: id.to_string(),
                                current: InstanceState::Active,
                            });
                        }
                        reused.push(plugin_ref.clone());
                        continue;
                    }
                    InstanceState::Failed { .. } => {
                        // Non-root Failed member: roll back this call's fresh
                        // activations, then report the illegal transition.
                        self.rollback(&activated);
                        return Err(HostError::InvalidState {
                            plugin_id: id.to_string(),
                            current,
                        });
                    }
                    // Stopped / Registered / Loaded: activate a fresh object.
                    _ => {}
                }
            }

            let manifest = match self.catalog.descriptors().into_iter().find(|m| m.id == id) {
                Some(manifest) => manifest.clone(),
                None => {
                    self.rollback(&activated);
                    return Err(HostError::Catalog(CatalogError::UnknownPlugin {
                        id: id.to_string(),
                    }));
                }
            };

            // Runtime gate before any instantiation.
            if manifest.runtime != RuntimeKind::Builtin {
                let unwound = self.rollback(&activated);
                return Err(HostError::UnsupportedRuntime {
                    plugin_id: id.to_string(),
                    unwound,
                });
            }
            if !self.factories.contains_key(id) {
                let unwound = self.rollback(&activated);
                return Err(HostError::MissingFactory {
                    plugin_id: id.to_string(),
                    unwound,
                });
            }
            let mut object = self.factories[id]();

            // Upsert the instance record (fresh object replaces the dropped
            // one; Stopped instances never reuse their old object).
            {
                let instance = self
                    .instances
                    .entry(id.to_string())
                    .or_insert_with(|| Instance {
                        manifest: manifest.clone(),
                        plan: plan.clone(),
                        state: InstanceState::Registered,
                        plugin: None,
                    });
                instance.manifest = manifest.clone();
                instance.plan = plan.clone();
                instance.state = InstanceState::Loaded;
                instance.plugin = None;
            }

            let plugin_id = id.to_string();
            let outcome = {
                let mut ctx = self.context_for(&plugin_id, &manifest, &plan);
                Self::guarded(|| object.activate(&mut ctx))
            };

            let failure = match outcome {
                Ok(Ok(())) => {
                    // Coverage: every declared provides key must have a
                    // self-registered effect before activate is done.
                    let missing: Vec<InterfaceKey> = manifest
                        .provides
                        .iter()
                        .filter(|key| {
                            !self
                                .provides
                                .values()
                                .any(|entry| entry.owner == plugin_id && entry.key == **key)
                        })
                        .cloned()
                        .collect();
                    if missing.is_empty() {
                        None
                    } else {
                        Some(PluginFailureCause::ProvidesNotCovered { missing })
                    }
                }
                Ok(Err(error)) => Some(PluginFailureCause::PluginError {
                    message: error.message,
                }),
                Err(_) => Some(PluginFailureCause::Panic),
            };

            if let Some(cause) = failure {
                self.revoke_effects(&plugin_id);
                if let Some(instance) = self.instances.get_mut(&plugin_id) {
                    instance.plugin = None;
                    instance.state = InstanceState::Failed {
                        stage: FailureStage::Activate,
                    };
                }
                self.activation_order.retain(|member| member != &plugin_id);
                let unwound = self.rollback(&activated);
                return Err(match cause {
                    PluginFailureCause::ProvidesNotCovered { missing } => {
                        HostError::ProvidesNotCovered {
                            plugin_id,
                            missing,
                            unwound,
                        }
                    }
                    PluginFailureCause::Panic => HostError::PluginPanic {
                        plugin_id,
                        stage: FailureStage::Activate,
                    },
                    other => HostError::PluginStartError {
                        plugin_id,
                        stage: FailureStage::Activate,
                        cause: Box::new(other),
                        unwound,
                    },
                });
            }

            if let Some(instance) = self.instances.get_mut(&plugin_id) {
                instance.plugin = Some(object);
                instance.state = InstanceState::Active;
            }
            self.activation_order.push(plugin_id);
            activated.push(plugin_ref.clone());
        }

        Ok(StartReport { activated, reused })
    }

    /// Stop one Active plugin. Refuses with `DependentsActive` (sorted ids)
    /// while Active consumers bound to it remain. Deactivation is guarded;
    /// effects are always revoked afterwards.
    pub fn stop(&mut self, id: &str) -> Result<(), HostError> {
        let state = match self.instances.get(id) {
            Some(instance) => instance.state,
            None => {
                return Err(HostError::UnknownPlugin { id: id.to_string() });
            }
        };
        if state != InstanceState::Active {
            return Err(HostError::InvalidState {
                plugin_id: id.to_string(),
                current: state,
            });
        }
        let dependents = self.active_dependents(id);
        if !dependents.is_empty() {
            return Err(HostError::DependentsActive {
                plugin_id: id.to_string(),
                dependents,
            });
        }
        let (new_state, cause) = self.teardown_instance(id);
        if let Some(instance) = self.instances.get_mut(id) {
            instance.state = new_state;
        }
        if let Some(cause) = cause {
            return Err(match cause {
                PluginFailureCause::Panic => HostError::PluginPanic {
                    plugin_id: id.to_string(),
                    stage: FailureStage::Deactivate,
                },
                _ => HostError::PluginDeactivateError {
                    plugin_id: id.to_string(),
                },
            });
        }
        Ok(())
    }

    /// Stop a plugin together with its still-Active transitive dependents,
    /// consumers before providers (reverse activation order). Returns the
    /// stopped plugins in teardown order.
    pub fn stop_subtree(&mut self, id: &str) -> Result<Vec<PluginRef>, HostError> {
        let state = match self.instances.get(id) {
            Some(instance) => instance.state,
            None => {
                return Err(HostError::UnknownPlugin { id: id.to_string() });
            }
        };
        if state != InstanceState::Active {
            return Err(HostError::InvalidState {
                plugin_id: id.to_string(),
                current: state,
            });
        }
        // Active subtree rooted at `id`: every Active plugin that transitively
        // consumes `id` through its activation-plan bindings.
        let mut subtree: BTreeSet<String> = BTreeSet::new();
        subtree.insert(id.to_string());
        let mut changed = true;
        while changed {
            changed = false;
            for (instance_id, instance) in &self.instances {
                if subtree.contains(instance_id) || instance.state != InstanceState::Active {
                    continue;
                }
                let consumes = instance.plan.bindings.iter().any(|binding| {
                    binding.consumer_id == *instance_id && subtree.contains(&binding.provider_id)
                });
                if consumes {
                    subtree.insert(instance_id.clone());
                    changed = true;
                }
            }
        }
        // Reverse activation order: consumers before providers.
        let order: Vec<String> = self
            .activation_order
            .iter()
            .filter(|member| subtree.contains(*member))
            .cloned()
            .collect();
        let mut stopped = Vec::new();
        let mut first_error = None;
        for member in order.iter().rev() {
            let (new_state, cause) = self.teardown_instance(member);
            let version = self
                .instances
                .get(member)
                .map(|instance| instance.manifest.version)
                .unwrap_or(ExactVersion {
                    major: 0,
                    minor: 0,
                    patch: 0,
                });
            if let Some(instance) = self.instances.get_mut(member) {
                instance.state = new_state;
            }
            stopped.push(PluginRef {
                id: member.clone(),
                version,
            });
            if first_error.is_none()
                && let Some(cause) = cause
            {
                first_error = Some(match cause {
                    PluginFailureCause::Panic => HostError::PluginPanic {
                        plugin_id: member.clone(),
                        stage: FailureStage::Deactivate,
                    },
                    _ => HostError::PluginDeactivateError {
                        plugin_id: member.clone(),
                    },
                });
            }
        }
        match first_error {
            None => Ok(stopped),
            Some(error) => Err(error),
        }
    }

    /// Unload a non-Active instance: remove its record and any leftover
    /// effects. `Failed` instances keep their recovery entries. `Active` is
    /// `InvalidState`; unknown id is `UnknownPlugin`. Catalog descriptors are
    /// untouched — declaration removal is `catalog.remove_descriptor`.
    pub fn unload(&mut self, id: &str) -> Result<(), HostError> {
        let state = match self.instances.get(id) {
            Some(instance) => instance.state,
            None => return Err(HostError::UnknownPlugin { id: id.to_string() }),
        };
        if state == InstanceState::Active {
            return Err(HostError::InvalidState {
                plugin_id: id.to_string(),
                current: state,
            });
        }
        // A Failed instance should never still own effects (teardown revokes
        // them), but if it somehow does, acknowledge the residue on the
        // recovery list before clearing the registry.
        if matches!(state, InstanceState::Failed { stage } if stage == FailureStage::Activate)
            && !self.effect_ids_of(id).is_empty()
        {
            self.recovery.push(RecoveryItem {
                plugin_id: id.to_string(),
                stage: FailureStage::Activate,
                cause: PluginFailureCause::PluginError {
                    message: "unload: residual effects cleared".to_string(),
                },
            });
        }
        self.revoke_effects(id);
        self.instances.remove(id);
        self.activation_order.retain(|member| member != id);
        Ok(())
    }

    /// Ids of Active plugins bound to consume `provider_id`, sorted.
    fn active_dependents(&self, provider_id: &str) -> Vec<String> {
        self.instances
            .iter()
            .filter(|(id, instance)| {
                instance.state == InstanceState::Active
                    && instance.plan.bindings.iter().any(|binding| {
                        binding.consumer_id == **id && binding.provider_id == provider_id
                    })
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Instances sorted by id; effect ids sorted within each snapshot.
    pub fn instances(&self) -> Vec<InstanceSnapshot> {
        self.instances
            .iter()
            .map(|(id, instance)| InstanceSnapshot {
                id: id.clone(),
                version: instance.manifest.version,
                state: instance.state,
                effect_ids: self.effect_ids_of(id),
            })
            .collect()
    }

    /// All registered effects (provides + subscriptions) in deterministic
    /// order: by owner id, then key, then effect id.
    pub fn effects(&self) -> Vec<EffectRecord> {
        let mut records: Vec<EffectRecord> = self
            .provides
            .values()
            .chain(self.subscriptions.values())
            .map(|entry| EffectRecord {
                effect_id: entry.id,
                plugin_id: entry.owner.clone(),
                key: entry.key.clone(),
                is_subscription: matches!(entry.kind, EffectKind::Subscription { .. }),
            })
            .collect();
        records.sort_by(|a, b| {
            a.plugin_id
                .cmp(&b.plugin_id)
                .then_with(|| a.key.cmp(&b.key))
                .then_with(|| a.effect_id.cmp(&b.effect_id))
        });
        records
    }

    /// Explicit recovery list in registration order.
    pub fn pending_recovery(&self) -> Vec<RecoveryItem> {
        self.recovery.clone()
    }
}

// --- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_catalog::{ScopeKind, parse_manifest};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn manifest_json(id: &str, display_name: &str, provides: Value, requires: Value) -> String {
        json!({
            "manifest_version": 1,
            "id": id,
            "display_name": display_name,
            "version": {"major": 1, "minor": 0, "patch": 0},
            "scope": "host",
            "runtime": "builtin",
            "lifecycle": "host_managed",
            "surfaces": ["headless"],
            "permissions": [],
            "config_schema": {},
            "provides": provides,
            "requires": requires,
        })
        .to_string()
    }

    fn provides_json(entries: &[(&str, &str)]) -> Value {
        Value::Array(
            entries
                .iter()
                .map(|(kind, name)| {
                    json!({"kind": kind, "name": name, "version": {"major":1,"minor":0,"patch":0}})
                })
                .collect(),
        )
    }

    fn key(kind: InterfaceKind, name: &str) -> InterfaceKey {
        InterfaceKey {
            kind,
            name: name.to_string(),
            version: ExactVersion {
                major: 1,
                minor: 0,
                patch: 0,
            },
        }
    }

    struct NullPlugin;
    impl BuiltinPlugin for NullPlugin {
        fn activate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
            Ok(())
        }
        fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
            Ok(())
        }
    }

    fn host_with(id: &str, provides: Value, requires: Value) -> PluginHost {
        let mut host = PluginHost::new(ScopeKey::Host).unwrap();
        host.catalog_mut()
            .register(parse_manifest(&manifest_json(id, id, provides, requires)).unwrap())
            .unwrap();
        host.with_factory(id, || Box::new(NullPlugin));
        host
    }

    #[test]
    fn invalid_scope_rejected() {
        assert!(
            PluginHost::new(ScopeKey::Project {
                project_id: String::new(),
            })
            .is_err()
        );
    }

    #[test]
    fn unknown_ids_are_typed() {
        let mut host = PluginHost::new(ScopeKey::Host).unwrap();
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
        assert!(matches!(
            host.emit("ghost", &key(InterfaceKind::Event, "e"), Box::new(1u32)),
            Err(HostError::UnknownPlugin { .. })
        ));
    }

    #[test]
    fn effect_ids_increase_deterministically() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        struct Two;
        impl BuiltinPlugin for Two {
            fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
                ctx.register_effect(&key(InterfaceKind::Service, "a"), Box::new(1u32))
                    .unwrap();
                ctx.register_effect(&key(InterfaceKind::Service, "b"), Box::new(2u32))
                    .unwrap();
                CALLS.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
                Ok(())
            }
        }
        let mut host = host_with(
            "two",
            provides_json(&[("service", "a"), ("service", "b")]),
            Value::Array(vec![]),
        );
        host.with_factory("two", || Box::new(Two));
        host.start(&["two".to_string()]).unwrap();
        let effects = host.effects();
        assert_eq!(effects.len(), 2);
        assert!(effects[0].effect_id < effects[1].effect_id);
        assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn display_name_is_not_identity() {
        let mut host = PluginHost::new(ScopeKey::Host).unwrap();
        for id in ["a.one", "a.two"] {
            host.catalog_mut()
                .register(
                    parse_manifest(&manifest_json(
                        id,
                        "Same Name",
                        Value::Array(vec![]),
                        Value::Array(vec![]),
                    ))
                    .unwrap(),
                )
                .unwrap();
            host.with_factory(id, || Box::new(NullPlugin));
        }
        host.start(&["a.one".to_string(), "a.two".to_string()])
            .unwrap();
        let ids: Vec<String> = host.instances().iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["a.one".to_string(), "a.two".to_string()]);
    }

    #[test]
    fn scope_kind_is_enforced_by_catalog() {
        let mut host = PluginHost::new(ScopeKey::Project {
            project_id: "p1".to_string(),
        })
        .unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "id": "s.plugin",
            "display_name": "S",
            "version": {"major": 1, "minor": 0, "patch": 0},
            "scope": "session",
            "runtime": "builtin",
            "lifecycle": "host_managed",
            "surfaces": ["headless"],
            "permissions": [],
            "config_schema": {},
            "provides": [],
            "requires": [],
        })
        .to_string();
        assert!(matches!(
            host.catalog_mut()
                .register(parse_manifest(&manifest).unwrap()),
            Err(CatalogError::ScopeMismatch {
                expected: ScopeKind::Project,
                found: ScopeKind::Session,
            })
        ));
    }

    #[test]
    fn stop_without_dependents_succeeds_and_revokes() {
        struct RegisterSvc;
        impl BuiltinPlugin for RegisterSvc {
            fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
                ctx.register_effect(&key(InterfaceKind::Service, "s"), Box::new(42u32))
                    .unwrap();
                Ok(())
            }
            fn deactivate(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
                Ok(())
            }
        }
        let mut host = host_with(
            "prov",
            provides_json(&[("service", "s")]),
            Value::Array(vec![]),
        );
        host.with_factory("prov", || Box::new(RegisterSvc));
        host.start(&["prov".to_string()]).unwrap();
        assert_eq!(host.effects().len(), 1);
        host.stop("prov").unwrap();
        assert!(host.effects().is_empty());
        assert_eq!(host.instances()[0].state, InstanceState::Stopped);
        assert!(matches!(
            host.stop("prov"),
            Err(HostError::InvalidState {
                current: InstanceState::Stopped,
                ..
            })
        ));
    }
}
