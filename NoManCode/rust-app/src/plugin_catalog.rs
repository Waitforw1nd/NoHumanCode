//! Declarative plugin catalog and dependency resolution (NEXT-02A).
//!
//! A [`PluginCatalog`] stores validated [`CatalogManifestV1`] declarations
//! bound to one [`ScopeKey`]. [`PluginCatalog::resolve`] walks the transitive
//! interface requirements of selected roots and returns a deterministic
//! [`ResolutionPlan`] — providers ordered before their consumers plus one
//! explicit binding per requirement — or a typed [`CatalogError`]
//! (missing dependency, version mismatch, ambiguous provider, or a real
//! dependency cycle).
//!
//! The catalog is purely declarative: `runtime`, `permissions`, `lifecycle`,
//! and `config_schema` are recorded, never executed. Resolving reads only the
//! catalog snapshot — no filesystem, network, database, process, clock, or
//! environment access. Nothing here loads WASM, grants permissions, or
//! starts/stops plugins; `remove_descriptor` only drops the declaration, it is
//! not an unload.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

use crate::secrets;

/// Exact three-component version. Only `major.minor.patch` equality is
/// supported — no ranges, wildcards, or prereleases — and it is not a SemVer
/// implementation. Plugin versions and interface versions are independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl fmt::Display for ExactVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Versioned interface dependency kind. Declaration order is the fixed sort
/// ordinal used by `ResolutionPlan::bindings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceKind {
    Service,
    Command,
    Query,
    Event,
    Resource,
    Capability,
}

impl fmt::Display for InterfaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            InterfaceKind::Service => "service",
            InterfaceKind::Command => "command",
            InterfaceKind::Query => "query",
            InterfaceKind::Event => "event",
            InterfaceKind::Resource => "resource",
            InterfaceKind::Capability => "capability",
        };
        f.write_str(name)
    }
}

/// A versioned capability contract: `kind` + `name` + exact `version`. Field
/// order is also the deterministic traversal order for `requires`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceKey {
    pub kind: InterfaceKind,
    pub name: String,
    pub version: ExactVersion,
}

impl fmt::Display for InterfaceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.kind, self.name, self.version)
    }
}

/// The scope kind a manifest declares itself for. A catalog only accepts
/// manifests whose `scope` matches its own [`ScopeKey`] kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    Host,
    Project,
    Session,
}

impl fmt::Display for ScopeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            ScopeKind::Host => "host",
            ScopeKind::Project => "project",
            ScopeKind::Session => "session",
        };
        f.write_str(name)
    }
}

/// Runtime form the plugin will run under in later slices. Recorded only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Builtin,
    Wasm,
    Process,
}

/// `host_managed` records that the Host will own load/start/stop/unload once
/// lifecycle slices land. It does not mean lifecycle is implemented here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleKind {
    #[serde(rename = "host_managed")]
    HostManaged,
}

/// UI/run surfaces a plugin declares support for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    Cli,
    Desktop,
    Headless,
}

/// The `manifest_version = 1` declaration shape. All fields are validated by
/// [`parse_manifest`] and again by [`PluginCatalog::register`], so directly
/// constructed values cannot bypass the rules.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogManifestV1 {
    /// Must be the integer 1.
    pub manifest_version: u64,
    /// Stable plugin id: 1–64 ASCII bytes, first `a-z`/`0-9`, then
    /// `a-z`/`0-9`/`.`/`_`/`-`. Identity for registration and bindings.
    pub id: String,
    /// Display-only label: non-blank, no control characters, at most 200
    /// UTF-8 bytes. Never used as identity.
    pub display_name: String,
    /// The plugin's own version — separate from any interface versions.
    pub version: ExactVersion,
    pub scope: ScopeKind,
    pub runtime: RuntimeKind,
    pub lifecycle: LifecycleKind,
    /// Non-empty, no duplicates.
    pub surfaces: Vec<Surface>,
    /// Declared permission names only — no approval state or capability is
    /// created here. May be empty, no duplicates.
    pub permissions: Vec<String>,
    /// Open JSON object carrying configuration schema metadata; member names
    /// are unrestricted and no JSON Schema semantics are validated.
    #[serde(deserialize_with = "deserialize_config_schema")]
    pub config_schema: Value,
    /// Interfaces this plugin offers; no fully identical key may repeat.
    pub provides: Vec<InterfaceKey>,
    /// Interfaces this plugin consumes; no fully identical key may repeat.
    pub requires: Vec<InterfaceKey>,
}

/// The scope a catalog instance is bound to at creation; immutable after
/// `PluginCatalog::new`. Two `Project`/`Session` keys are distinct catalog
/// instances — there is no global registry or parent-scope fallback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ScopeKey {
    Host,
    Project {
        project_id: String,
    },
    Session {
        project_id: String,
        session_id: String,
    },
}

impl ScopeKey {
    pub fn kind(&self) -> ScopeKind {
        match self {
            ScopeKey::Host => ScopeKind::Host,
            ScopeKey::Project { .. } => ScopeKind::Project,
            ScopeKey::Session { .. } => ScopeKind::Session,
        }
    }
}

/// A resolved plugin reference inside a plan: stable id + plugin version.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PluginRef {
    pub id: String,
    pub version: ExactVersion,
}

/// One fulfilled requirement: the consuming plugin id, the full interface
/// requirement, and the providing plugin's stable id and plugin version.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DependencyBinding {
    pub consumer_id: String,
    pub requirement: InterfaceKey,
    pub provider_id: String,
    pub provider_version: ExactVersion,
}

/// Immutable plain-data result of a resolve. It holds no running instances,
/// grants, or handles; after the catalog changes a stored plan is history and
/// must be re-resolved, not executed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolutionPlan {
    pub scope: ScopeKey,
    /// Reachable plugins ordered provider-before-consumer; ready nodes are
    /// picked by ascending id, so input order never changes the result.
    pub ordered_plugins: Vec<PluginRef>,
    /// One entry per requirement, sorted by consumer id, requirement
    /// (kind ordinal, name, version), then provider id.
    pub bindings: Vec<DependencyBinding>,
}

/// Typed catalog/parse/resolution failures. Variants carry machine-readable
/// evidence (ids, keys, versions, provider lists, cycle paths); parsing never
/// echoes the raw input JSON or `config_schema` content.
#[derive(Clone, Debug, PartialEq)]
pub enum CatalogError {
    /// Structural/semantic manifest rejection: field path + reason.
    InvalidManifest { path: String, reason: String },
    /// `manifest_version` other than 1.
    UnsupportedManifestVersion { found: u64 },
    /// Same plugin id already registered (any version).
    DuplicatePlugin { id: String },
    /// Manifest scope kind differs from the catalog's scope kind.
    ScopeMismatch {
        expected: ScopeKind,
        found: ScopeKind,
    },
    /// `remove_descriptor` target not present; catalog unchanged.
    UnknownPlugin { id: String },
    /// Well-formed but unregistered resolve root.
    UnknownRoot { id: String },
    /// No provider declares the requirement's kind+name.
    MissingDependency {
        consumer_id: String,
        requirement: InterfaceKey,
    },
    /// Providers exist for kind+name but none at the required version.
    /// `available` is sorted and deduplicated.
    VersionMismatch {
        consumer_id: String,
        requirement: InterfaceKey,
        available: Vec<ExactVersion>,
    },
    /// More than one provider matches exactly; `providers` is sorted by id.
    /// Resolution never picks a first/registration-order provider.
    AmbiguousProvider {
        consumer_id: String,
        requirement: InterfaceKey,
        providers: Vec<String>,
    },
    /// Real closed dependency path (first == last; every hop is a
    /// consumer→provider edge). Off-cycle nodes are never included.
    DependencyCycle { path: Vec<String> },
    /// Scope identity failed the persisted-id rules.
    InvalidScope { field: String, reason: String },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::InvalidManifest { path, reason } => {
                if path.is_empty() {
                    write!(f, "插件声明无效：{reason}")
                } else {
                    write!(f, "插件声明无效（{path}）：{reason}")
                }
            }
            CatalogError::UnsupportedManifestVersion { found } => {
                write!(f, "不支持的声明版本：{found}")
            }
            CatalogError::DuplicatePlugin { id } => {
                write!(f, "目录中已存在插件：{id}")
            }
            CatalogError::ScopeMismatch { expected, found } => {
                write!(f, "声明作用域 {found} 与目录作用域 {expected} 不符")
            }
            CatalogError::UnknownPlugin { id } => {
                write!(f, "目录中不存在插件：{id}")
            }
            CatalogError::UnknownRoot { id } => {
                write!(f, "解析根未登记：{id}")
            }
            CatalogError::MissingDependency {
                consumer_id,
                requirement,
            } => {
                write!(f, "插件 {consumer_id} 的依赖 {requirement} 没有候选提供者")
            }
            CatalogError::VersionMismatch {
                consumer_id,
                requirement,
                available,
            } => {
                let versions = available
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "插件 {consumer_id} 的依赖 {requirement} 版本不符；可用版本：{versions}"
                )
            }
            CatalogError::AmbiguousProvider {
                consumer_id,
                requirement,
                providers,
            } => {
                write!(
                    f,
                    "插件 {consumer_id} 的依赖 {requirement} 存在多个提供者：{}",
                    providers.join(", ")
                )
            }
            CatalogError::DependencyCycle { path } => {
                write!(f, "插件依赖循环：{}", path.join(" -> "))
            }
            CatalogError::InvalidScope { field, reason } => {
                write!(f, "目录作用域标识无效（{field}）：{reason}")
            }
        }
    }
}

impl std::error::Error for CatalogError {}

fn invalid(path: &str, reason: &str) -> CatalogError {
    CatalogError::InvalidManifest {
        path: path.to_string(),
        reason: reason.to_string(),
    }
}

fn join_path(path: &str, member: &str) -> String {
    if path.is_empty() {
        member.to_string()
    } else {
        format!("{path}.{member}")
    }
}

fn valid_name(value: &str, max_bytes: usize) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > max_bytes || !value.is_ascii() {
        return false;
    }
    valid_first_char(bytes[0]) && bytes[1..].iter().all(|byte| valid_char(*byte))
}

fn valid_first_char(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn valid_char(byte: u8) -> bool {
    valid_first_char(byte) || matches!(byte, b'.' | b'_' | b'-')
}

/// Plugin id: 1–64 ASCII bytes.
fn valid_plugin_id(id: &str) -> bool {
    valid_name(id, 64)
}

/// Interface and permission names: 1–128 ASCII bytes, same charset.
fn valid_interface_name(name: &str) -> bool {
    valid_name(name, 128)
}

fn validate_scope_id(field: &str, value: &str) -> Result<(), CatalogError> {
    secrets::validate_persisted_id(field, value).map_err(|error| CatalogError::InvalidScope {
        field: field.to_string(),
        reason: error.to_string(),
    })
}

/// Shared semantic validation for `parse_manifest` and `register`; callers who
/// construct the struct directly get the same rules.
fn validate_manifest(manifest: &CatalogManifestV1) -> Result<(), CatalogError> {
    if manifest.manifest_version != 1 {
        return Err(CatalogError::UnsupportedManifestVersion {
            found: manifest.manifest_version,
        });
    }
    if !valid_plugin_id(&manifest.id) {
        return Err(invalid(
            "id",
            "plugin id must be 1-64 ASCII bytes: first a-z or 0-9, rest a-z/0-9/.-_",
        ));
    }
    if manifest.display_name.trim().is_empty()
        || manifest.display_name.len() > 200
        || manifest.display_name.chars().any(|ch| ch.is_control())
    {
        return Err(invalid(
            "display_name",
            "display name must be non-blank, control-free, and at most 200 UTF-8 bytes",
        ));
    }
    if manifest.surfaces.is_empty() {
        return Err(invalid("surfaces", "must declare at least one surface"));
    }
    let mut surfaces = HashSet::new();
    for (index, surface) in manifest.surfaces.iter().enumerate() {
        if !surfaces.insert(surface) {
            return Err(invalid(&format!("surfaces[{index}]"), "duplicate surface"));
        }
    }
    let mut permissions = HashSet::new();
    for (index, permission) in manifest.permissions.iter().enumerate() {
        if !valid_interface_name(permission) {
            return Err(invalid(
                &format!("permissions[{index}]"),
                "permission name must be 1-128 ASCII bytes: first a-z or 0-9, rest a-z/0-9/.-_",
            ));
        }
        if !permissions.insert(permission) {
            return Err(invalid(
                &format!("permissions[{index}]"),
                "duplicate permission name",
            ));
        }
    }
    if !manifest.config_schema.is_object() {
        return Err(invalid("config_schema", "must be a JSON object"));
    }
    validate_interface_keys(&manifest.provides, "provides")?;
    validate_interface_keys(&manifest.requires, "requires")?;
    Ok(())
}

fn validate_interface_keys(keys: &[InterfaceKey], path: &str) -> Result<(), CatalogError> {
    let mut seen = HashSet::new();
    for (index, key) in keys.iter().enumerate() {
        if !valid_interface_name(&key.name) {
            return Err(invalid(
                &format!("{path}[{index}].name"),
                "interface name must be 1-128 ASCII bytes: first a-z or 0-9, rest a-z/0-9/.-_",
            ));
        }
        if !seen.insert(key) {
            return Err(invalid(
                &format!("{path}[{index}]"),
                "duplicate interface key",
            ));
        }
    }
    Ok(())
}

// --- Strict JSON input -----------------------------------------------------
//
// `serde_json::Value` silently keeps the last value of a duplicated object
// member, so every object in the input is deserialized through a visitor that
// rejects duplicates instead. Member names inside `config_schema` are open
// content and never appear in diagnostics.

struct StrictJson(Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_any(StrictJsonVisitor {
                path: String::new(),
            })
            .map(StrictJson)
    }
}

struct StrictJsonSeed {
    path: String,
}

impl<'de> DeserializeSeed<'de> for StrictJsonSeed {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(StrictJsonVisitor { path: self.path })
    }
}

fn inside_config_schema(path: &str) -> bool {
    path == "config_schema"
        || path.starts_with("config_schema.")
        || path.starts_with("config_schema[")
}

struct StrictJsonVisitor {
    path: String,
}

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("unsupported number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(StrictJsonVisitor { path: self.path })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        let mut index = 0usize;
        while let Some(item) = seq.next_element_seed(StrictJsonSeed {
            // Inside config_schema the parent path is kept verbatim so array
            // indices of open content never reach diagnostics either.
            path: if inside_config_schema(&self.path) {
                self.path.clone()
            } else {
                format!("{}[{index}]", self.path)
            },
        })? {
            items.push(item);
            index += 1;
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut seen = HashSet::new();
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                // Field path of the duplicate member itself — except inside
                // config_schema, whose member names are open content and stay
                // out of diagnostics.
                let at = if inside_config_schema(&self.path) {
                    self.path.clone()
                } else if self.path.is_empty() {
                    key.clone()
                } else {
                    join_path(&self.path, &key)
                };
                return Err(A::Error::custom(format!("duplicate object member at {at}")));
            }
            // Inside config_schema member names are open content; keep the
            // parent path so diagnostics stay free of schema member names.
            let child = if inside_config_schema(&self.path) {
                self.path.clone()
            } else {
                join_path(&self.path, &key)
            };
            let value = map.next_value_seed(StrictJsonSeed { path: child })?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

fn deserialize_config_schema<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Value, D::Error> {
    StrictJsonSeed {
        path: "config_schema".to_string(),
    }
    .deserialize(deserializer)
}

// --- Manifest extraction ---------------------------------------------------
//
// Field extraction is hand-rolled so `InvalidManifest` can carry a precise
// field path while never echoing input values (only member names of the typed
// envelope appear in paths — arbitrary content lives only under
// `config_schema`, which is checked as a whole).

const MANIFEST_FIELDS: &[&str] = &[
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
];

/// Parse and fully validate a manifest from a UTF-8 JSON string. Failures are
/// typed and never echo the raw JSON or `config_schema` content.
pub fn parse_manifest(json: &str) -> Result<CatalogManifestV1, CatalogError> {
    let StrictJson(document) = serde_json::from_str::<StrictJson>(json).map_err(|error| {
        let text = error.to_string();
        // StrictJsonVisitor reports duplicates as "duplicate object member at
        // <field path>"; serde_json appends " at line N column M". Recover the
        // structured path so callers never have to parse `reason`.
        const DUPLICATE: &str = "duplicate object member at ";
        let path = text
            .strip_prefix(DUPLICATE)
            .map(|rest| match rest.rfind(" at line ") {
                Some(index) => &rest[..index],
                None => rest,
            })
            .unwrap_or_default()
            .to_string();
        CatalogError::InvalidManifest { path, reason: text }
    })?;
    let manifest = manifest_from_value(&document)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn manifest_from_value(value: &Value) -> Result<CatalogManifestV1, CatalogError> {
    let root = expect_object(value, "")?;
    reject_unknown_members(root, MANIFEST_FIELDS, "")?;
    let manifest_version = expect_u64(
        required_member(root, "manifest_version", "")?,
        "manifest_version",
    )?;
    let id = expect_string(required_member(root, "id", "")?, "id")?.to_string();
    let display_name =
        expect_string(required_member(root, "display_name", "")?, "display_name")?.to_string();
    let version = version_from_value(required_member(root, "version", "")?, "version")?;
    let scope = scope_kind_from_value(required_member(root, "scope", "")?, "scope")?;
    let runtime = runtime_kind_from_value(required_member(root, "runtime", "")?, "runtime")?;
    let lifecycle =
        lifecycle_kind_from_value(required_member(root, "lifecycle", "")?, "lifecycle")?;
    let surfaces = surfaces_from_value(required_member(root, "surfaces", "")?, "surfaces")?;
    let permissions = strings_from_value(required_member(root, "permissions", "")?, "permissions")?;
    let config_schema = required_member(root, "config_schema", "")?;
    if !config_schema.is_object() {
        return Err(invalid("config_schema", "must be a JSON object"));
    }
    let provides = interface_keys_from_value(required_member(root, "provides", "")?, "provides")?;
    let requires = interface_keys_from_value(required_member(root, "requires", "")?, "requires")?;
    Ok(CatalogManifestV1 {
        manifest_version,
        id,
        display_name,
        version,
        scope,
        runtime,
        lifecycle,
        surfaces,
        permissions,
        config_schema: config_schema.clone(),
        provides,
        requires,
    })
}

fn version_from_value(value: &Value, path: &str) -> Result<ExactVersion, CatalogError> {
    let object = expect_object(value, path)?;
    reject_unknown_members(object, &["major", "minor", "patch"], path)?;
    Ok(ExactVersion {
        major: expect_u32(
            required_member(object, "major", path)?,
            &join_path(path, "major"),
        )?,
        minor: expect_u32(
            required_member(object, "minor", path)?,
            &join_path(path, "minor"),
        )?,
        patch: expect_u32(
            required_member(object, "patch", path)?,
            &join_path(path, "patch"),
        )?,
    })
}

fn scope_kind_from_value(value: &Value, path: &str) -> Result<ScopeKind, CatalogError> {
    match expect_string(value, path)? {
        "host" => Ok(ScopeKind::Host),
        "project" => Ok(ScopeKind::Project),
        "session" => Ok(ScopeKind::Session),
        _ => Err(invalid(path, "unknown scope kind")),
    }
}

fn runtime_kind_from_value(value: &Value, path: &str) -> Result<RuntimeKind, CatalogError> {
    match expect_string(value, path)? {
        "builtin" => Ok(RuntimeKind::Builtin),
        "wasm" => Ok(RuntimeKind::Wasm),
        "process" => Ok(RuntimeKind::Process),
        _ => Err(invalid(path, "unknown runtime kind")),
    }
}

fn lifecycle_kind_from_value(value: &Value, path: &str) -> Result<LifecycleKind, CatalogError> {
    match expect_string(value, path)? {
        "host_managed" => Ok(LifecycleKind::HostManaged),
        _ => Err(invalid(path, "unsupported lifecycle")),
    }
}

fn surfaces_from_value(value: &Value, path: &str) -> Result<Vec<Surface>, CatalogError> {
    expect_array(value, path)?
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let item_path = format!("{path}[{index}]");
            match expect_string(item, &item_path)? {
                "cli" => Ok(Surface::Cli),
                "desktop" => Ok(Surface::Desktop),
                "headless" => Ok(Surface::Headless),
                _ => Err(invalid(&item_path, "unknown surface")),
            }
        })
        .collect()
}

fn strings_from_value(value: &Value, path: &str) -> Result<Vec<String>, CatalogError> {
    expect_array(value, path)?
        .iter()
        .enumerate()
        .map(|(index, item)| expect_string(item, &format!("{path}[{index}]")).map(str::to_string))
        .collect()
}

fn interface_keys_from_value(value: &Value, path: &str) -> Result<Vec<InterfaceKey>, CatalogError> {
    expect_array(value, path)?
        .iter()
        .enumerate()
        .map(|(index, item)| interface_key_from_value(item, &format!("{path}[{index}]")))
        .collect()
}

fn interface_key_from_value(value: &Value, path: &str) -> Result<InterfaceKey, CatalogError> {
    let object = expect_object(value, path)?;
    reject_unknown_members(object, &["kind", "name", "version"], path)?;
    let kind_path = join_path(path, "kind");
    let kind = match expect_string(required_member(object, "kind", path)?, &kind_path)? {
        "service" => InterfaceKind::Service,
        "command" => InterfaceKind::Command,
        "query" => InterfaceKind::Query,
        "event" => InterfaceKind::Event,
        "resource" => InterfaceKind::Resource,
        "capability" => InterfaceKind::Capability,
        _ => return Err(invalid(&kind_path, "unknown interface kind")),
    };
    let name = expect_string(
        required_member(object, "name", path)?,
        &join_path(path, "name"),
    )?
    .to_string();
    let version = version_from_value(
        required_member(object, "version", path)?,
        &join_path(path, "version"),
    )?;
    Ok(InterfaceKey {
        kind,
        name,
        version,
    })
}

fn required_member<'v>(
    object: &'v Map<String, Value>,
    member: &'static str,
    path: &str,
) -> Result<&'v Value, CatalogError> {
    object
        .get(member)
        .ok_or_else(|| invalid(&join_path(path, member), "missing required field"))
}

fn reject_unknown_members(
    object: &Map<String, Value>,
    allowed: &[&'static str],
    path: &str,
) -> Result<(), CatalogError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(&join_path(path, key), "unknown field"));
        }
    }
    Ok(())
}

fn expect_object<'v>(value: &'v Value, path: &str) -> Result<&'v Map<String, Value>, CatalogError> {
    value.as_object().ok_or_else(|| {
        invalid(
            path,
            if path.is_empty() {
                "manifest must be a JSON object"
            } else {
                "expected a JSON object"
            },
        )
    })
}

fn expect_array<'v>(value: &'v Value, path: &str) -> Result<&'v Vec<Value>, CatalogError> {
    value
        .as_array()
        .ok_or_else(|| invalid(path, "expected a JSON array"))
}

fn expect_string<'v>(value: &'v Value, path: &str) -> Result<&'v str, CatalogError> {
    value
        .as_str()
        .ok_or_else(|| invalid(path, "expected a JSON string"))
}

fn expect_u64(value: &Value, path: &str) -> Result<u64, CatalogError> {
    value
        .as_u64()
        .ok_or_else(|| invalid(path, "expected an unsigned integer"))
}

fn expect_u32(value: &Value, path: &str) -> Result<u32, CatalogError> {
    let number = expect_u64(value, path)?;
    u32::try_from(number).map_err(|_| invalid(path, "integer does not fit u32"))
}

// --- Catalog ---------------------------------------------------------------

/// An in-memory catalog of validated manifests bound to one `ScopeKey`.
/// Scope kinds never cross: two `Project`/`Session` keys are independent
/// catalogs, and resolution never looks outside the instance.
#[derive(Debug)]
pub struct PluginCatalog {
    scope: ScopeKey,
    entries: BTreeMap<String, CatalogManifestV1>,
}

impl PluginCatalog {
    /// Validate the scope identity and create an empty catalog.
    pub fn new(scope: ScopeKey) -> Result<Self, CatalogError> {
        match &scope {
            ScopeKey::Host => {}
            ScopeKey::Project { project_id } => validate_scope_id("project_id", project_id)?,
            ScopeKey::Session {
                project_id,
                session_id,
            } => {
                validate_scope_id("project_id", project_id)?;
                validate_scope_id("session_id", session_id)?;
            }
        }
        Ok(Self {
            scope,
            entries: BTreeMap::new(),
        })
    }

    pub fn scope(&self) -> &ScopeKey {
        &self.scope
    }

    /// Fully validate (even for directly constructed structs), check scope
    /// kind and id uniqueness, then insert in one write. Failures leave the
    /// catalog untouched. Declaring unmet or cyclic requirements is allowed —
    /// `resolve` reports them.
    pub fn register(&mut self, manifest: CatalogManifestV1) -> Result<(), CatalogError> {
        validate_manifest(&manifest)?;
        let expected = self.scope.kind();
        if manifest.scope != expected {
            return Err(CatalogError::ScopeMismatch {
                expected,
                found: manifest.scope,
            });
        }
        if self.entries.contains_key(&manifest.id) {
            return Err(CatalogError::DuplicatePlugin {
                id: manifest.id.clone(),
            });
        }
        self.entries.insert(manifest.id.clone(), manifest);
        Ok(())
    }

    /// Remove one declaration and return it. Unknown ids are an error and
    /// change nothing. Removing a depended-on provider is allowed; the next
    /// `resolve` reports the dependency as missing. This is not an unload.
    pub fn remove_descriptor(&mut self, id: &str) -> Result<CatalogManifestV1, CatalogError> {
        self.entries
            .remove(id)
            .ok_or_else(|| CatalogError::UnknownPlugin { id: id.to_string() })
    }

    /// All descriptors ordered by stable id. Read-only; there is no way to
    /// mutate the catalog outside `register`/`remove_descriptor`.
    pub fn descriptors(&self) -> Vec<&CatalogManifestV1> {
        self.entries.values().collect()
    }

    /// Resolve the transitive interface requirements of `roots`. Duplicate
    /// roots are deduplicated and processed by ascending id; an empty root
    /// list yields an empty plan; a well-formed but unregistered root is
    /// `UnknownRoot`. Unreachable manifests never affect the result.
    ///
    /// Each requirement is classified deterministically — exactly one exact
    /// provider binds, zero splits into `MissingDependency` (no same kind/name)
    /// or `VersionMismatch`, several is `AmbiguousProvider`. If every
    /// requirement binds but the graph cannot be fully ordered, a real closed
    /// dependency path is returned as `DependencyCycle`.
    pub fn resolve(&self, roots: &[String]) -> Result<ResolutionPlan, CatalogError> {
        let root_ids: BTreeSet<&str> = roots.iter().map(String::as_str).collect();
        if root_ids.is_empty() {
            return Ok(ResolutionPlan {
                scope: self.scope.clone(),
                ordered_plugins: Vec::new(),
                bindings: Vec::new(),
            });
        }
        // (kind, name) -> candidate providers. Iterating the BTreeMap keeps
        // each candidate list in ascending provider id order.
        let mut providers: ProviderIndex<'_> = HashMap::new();
        for (id, manifest) in &self.entries {
            for provided in &manifest.provides {
                providers
                    .entry((provided.kind, provided.name.as_str()))
                    .or_default()
                    .push((id.as_str(), manifest.version, provided));
            }
        }

        let mut visited: HashSet<&str> = HashSet::new();
        let mut bindings: Vec<DependencyBinding> = Vec::new();
        // Dependency edges in consumer -> provider direction, deduplicated so
        // many interfaces bound to one provider still count as a single edge.
        let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
        // Iterative DFS; each frame processes its requires in
        // (kind, name, version) order, so the first error is deterministic.
        let mut stack: Vec<(String, std::vec::IntoIter<InterfaceKey>)> = Vec::new();

        for root in root_ids {
            // Root validity/registration is checked as each root's turn comes
            // up in ascending id order, so the first error in traversal order
            // wins even across error classes.
            if !valid_plugin_id(root) {
                return Err(invalid("roots", "root is not a valid plugin id"));
            }
            if !self.entries.contains_key(root) {
                return Err(CatalogError::UnknownRoot {
                    id: root.to_string(),
                });
            }
            if !visited.insert(root) {
                continue;
            }
            stack.push((root.to_string(), sorted_requires(&self.entries[root])));
            while let Some((consumer, requires)) = stack.last_mut() {
                let Some(requirement) = requires.next() else {
                    stack.pop();
                    continue;
                };
                let consumer_id = consumer.clone();
                let candidates = providers
                    .get(&(requirement.kind, requirement.name.as_str()))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let mut exact: Vec<(&str, ExactVersion)> = candidates
                    .iter()
                    .filter(|(_, _, key)| key.version == requirement.version)
                    .map(|(id, plugin_version, _)| (*id, *plugin_version))
                    .collect();
                exact.sort_unstable();
                exact.dedup();
                match exact.len() {
                    0 if candidates.is_empty() => {
                        return Err(CatalogError::MissingDependency {
                            consumer_id,
                            requirement,
                        });
                    }
                    0 => {
                        let available: Vec<ExactVersion> = candidates
                            .iter()
                            .map(|(_, _, key)| key.version)
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect();
                        return Err(CatalogError::VersionMismatch {
                            consumer_id,
                            requirement,
                            available,
                        });
                    }
                    1 => {
                        let (provider_id, provider_version) = exact[0];
                        bindings.push(DependencyBinding {
                            consumer_id: consumer_id.clone(),
                            requirement,
                            provider_id: provider_id.to_string(),
                            provider_version,
                        });
                        edges.insert((consumer_id, provider_id.to_string()));
                        if visited.insert(provider_id) {
                            stack.push((
                                provider_id.to_string(),
                                sorted_requires(&self.entries[provider_id]),
                            ));
                        }
                    }
                    _ => {
                        return Err(CatalogError::AmbiguousProvider {
                            consumer_id,
                            requirement,
                            providers: exact.iter().map(|(id, _)| (*id).to_string()).collect(),
                        });
                    }
                }
            }
        }

        // Topological order over the reached set: provider before consumer,
        // ready nodes picked by ascending id. Input order cannot change this.
        let mut indegree: HashMap<&str, usize> = HashMap::new();
        let mut successors: HashMap<&str, Vec<&str>> = HashMap::new();
        for node in &visited {
            indegree.insert(*node, 0);
            successors.insert(*node, Vec::new());
        }
        for (consumer, provider) in &edges {
            *indegree.entry(consumer.as_str()).or_insert(0) += 1;
            successors
                .entry(provider.as_str())
                .or_default()
                .push(consumer.as_str());
        }
        let mut ready: BTreeSet<&str> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(node, _)| *node)
            .collect();
        let mut ordered: Vec<&str> = Vec::with_capacity(visited.len());
        while let Some(node) = ready.pop_first() {
            ordered.push(node);
            if let Some(consumers) = successors.get(node) {
                for consumer in consumers {
                    if let Some(degree) = indegree.get_mut(consumer) {
                        *degree -= 1;
                        if *degree == 0 {
                            ready.insert(*consumer);
                        }
                    }
                }
            }
        }
        if ordered.len() < visited.len() {
            return Err(CatalogError::DependencyCycle {
                path: cycle_path(&visited, &ordered, &edges),
            });
        }

        bindings.sort();
        let ordered_plugins = ordered
            .iter()
            .filter_map(|id| {
                self.entries.get(*id).map(|manifest| PluginRef {
                    id: (*id).to_string(),
                    version: manifest.version,
                })
            })
            .collect();
        Ok(ResolutionPlan {
            scope: self.scope.clone(),
            ordered_plugins,
            bindings,
        })
    }
}

/// (kind, name) -> candidate providers as `(id, plugin_version, declared key)`;
/// each list stays in ascending provider id order because entries iterate a
/// `BTreeMap`.
type ProviderIndex<'m> =
    HashMap<(InterfaceKind, &'m str), Vec<(&'m str, ExactVersion, &'m InterfaceKey)>>;

fn sorted_requires(manifest: &CatalogManifestV1) -> std::vec::IntoIter<InterfaceKey> {
    let mut requires = manifest.requires.clone();
    requires.sort();
    requires.into_iter()
}

/// Extract one real closed dependency path from the nodes Kahn's algorithm
/// could not emit. Every leftover node kept at least one provider edge inside
/// the leftover set (otherwise it would have been emitted), so walking
/// consumer -> provider edges deterministically reaches a cycle; off-cycle
/// consumers that merely depend on the loop are never reported as part of it.
fn cycle_path<'a>(
    visited: &HashSet<&'a str>,
    ordered: &[&'a str],
    edges: &BTreeSet<(String, String)>,
) -> Vec<String> {
    let emitted: HashSet<&str> = ordered.iter().copied().collect();
    let mut leftover: Vec<&str> = visited
        .iter()
        .copied()
        .filter(|node| !emitted.contains(node))
        .collect();
    leftover.sort_unstable();
    let leftover_set: HashSet<&str> = leftover.iter().copied().collect();
    // Adjacency stays in consumer -> provider direction, sorted by id.
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for (consumer, provider) in edges {
        if leftover_set.contains(consumer.as_str()) && leftover_set.contains(provider.as_str()) {
            adjacency
                .entry(consumer.as_str())
                .or_default()
                .push(provider.as_str());
        }
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort_unstable();
    }

    let mut path: Vec<&str> = Vec::new();
    let mut position: HashMap<&str, usize> = HashMap::new();
    for start in leftover {
        path.clear();
        position.clear();
        path.push(start);
        position.insert(start, 0);
        loop {
            let current = path.last().copied().unwrap_or(start);
            let Some(&next) = adjacency
                .get(current)
                .and_then(|neighbors| neighbors.first())
            else {
                break; // unreachable for a real Kahn leftover set
            };
            if let Some(&seen_at) = position.get(&next) {
                let mut closed = path[seen_at..].to_vec();
                closed.push(next);
                return closed.iter().map(|id| (*id).to_string()).collect();
            }
            position.insert(next, path.len());
            path.push(next);
        }
    }
    // Unreachable unless the leftover set violated the Kahn invariant; return
    // the open walk rather than panicking or inventing a loop.
    path.iter().map(|id| (*id).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_manifest(id: &str) -> CatalogManifestV1 {
        CatalogManifestV1 {
            manifest_version: 1,
            id: id.to_string(),
            display_name: format!("{id} plugin"),
            version: ExactVersion {
                major: 1,
                minor: 0,
                patch: 0,
            },
            scope: ScopeKind::Host,
            runtime: RuntimeKind::Builtin,
            lifecycle: LifecycleKind::HostManaged,
            surfaces: vec![Surface::Cli],
            permissions: Vec::new(),
            config_schema: Value::Object(Map::new()),
            provides: Vec::new(),
            requires: Vec::new(),
        }
    }

    const BASE_JSON: &str = r#"{
        "manifest_version": 1,
        "id": "ok.plugin",
        "display_name": "OK Plugin",
        "version": {"major": 1, "minor": 2, "patch": 3},
        "scope": "host",
        "runtime": "builtin",
        "lifecycle": "host_managed",
        "surfaces": ["cli"],
        "permissions": [],
        "config_schema": {},
        "provides": [],
        "requires": []
    }"#;

    #[test]
    fn name_charset_rules() {
        for ok in ["a", "9", "9lives", "a1.b-c_d", "x".repeat(64).as_str()] {
            assert!(valid_plugin_id(ok), "{ok:?}");
        }
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
            assert!(!valid_plugin_id(bad), "{bad:?}");
        }
        assert!(valid_interface_name(&"n".repeat(128)));
        assert!(!valid_interface_name(&"n".repeat(129)));
        assert!(!valid_interface_name("Bad-Name"));
    }

    #[test]
    fn exact_version_orders_as_numeric_triple() {
        let v = |major, minor, patch| ExactVersion {
            major,
            minor,
            patch,
        };
        assert!(v(0, 9, 9) < v(1, 0, 0));
        assert!(v(1, 0, 0) < v(1, 0, 1));
        assert!(v(1, 0, 1) < v(1, 1, 0));
        assert_eq!(v(2, 3, 4).to_string(), "2.3.4");
    }

    #[test]
    fn strict_json_rejects_duplicate_members_everywhere() {
        let dup_root = BASE_JSON.replace(
            "\"id\": \"ok.plugin\"",
            "\"id\": \"ok.plugin\", \"id\": \"other.id\"",
        );
        assert!(matches!(
            parse_manifest(&dup_root),
            Err(CatalogError::InvalidManifest { .. })
        ));
        let dup_config = BASE_JSON.replace(
            "\"config_schema\": {}",
            "\"config_schema\": {\"a\": 1, \"a\": 2}",
        );
        assert!(matches!(
            parse_manifest(&dup_config),
            Err(CatalogError::InvalidManifest { .. })
        ));
        let dup_nested = BASE_JSON.replace(
            "\"config_schema\": {}",
            "\"config_schema\": {\"items\": [{\"x\": 1, \"x\": 2}]}",
        );
        assert!(matches!(
            parse_manifest(&dup_nested),
            Err(CatalogError::InvalidManifest { .. })
        ));
        let dup_provides = BASE_JSON.replace(
            "\"provides\": []",
            "\"provides\": [{\"kind\": \"service\", \"kind\": \"query\", \"name\": \"s\", \"version\": {\"major\": 1, \"minor\": 0, \"patch\": 0}}]",
        );
        assert!(matches!(
            parse_manifest(&dup_provides),
            Err(CatalogError::InvalidManifest { .. })
        ));
    }

    #[test]
    fn scope_identity_uses_persisted_id_rules() {
        assert!(PluginCatalog::new(ScopeKey::Host).is_ok());
        assert!(
            PluginCatalog::new(ScopeKey::Project {
                project_id: "prj-1".to_string()
            })
            .is_ok()
        );
        for bad in ["", "has space", "xai-secretvalue", "p".repeat(129).as_str()] {
            assert!(matches!(
                PluginCatalog::new(ScopeKey::Project {
                    project_id: bad.to_string()
                }),
                Err(CatalogError::InvalidScope { .. })
            ));
        }
        assert!(matches!(
            PluginCatalog::new(ScopeKey::Session {
                project_id: "prj-1".to_string(),
                session_id: String::new()
            }),
            Err(CatalogError::InvalidScope { .. })
        ));
    }

    #[test]
    fn register_failure_keeps_catalog_unchanged() {
        let mut catalog = PluginCatalog::new(ScopeKey::Host).unwrap();
        catalog.register(host_manifest("one.ok")).unwrap();
        let before: Vec<CatalogManifestV1> = catalog.descriptors().into_iter().cloned().collect();
        let mut bad = host_manifest("two.bad");
        bad.id = "BAD ID".to_string();
        assert!(matches!(
            catalog.register(bad),
            Err(CatalogError::InvalidManifest { .. })
        ));
        let after: Vec<CatalogManifestV1> = catalog.descriptors().into_iter().cloned().collect();
        assert_eq!(before, after);
    }

    #[test]
    fn display_name_rules() {
        let mut catalog = PluginCatalog::new(ScopeKey::Host).unwrap();
        for bad in [
            "",
            "   ",
            "with\ttab",
            "with\nline",
            "长".repeat(67).as_str(),
        ] {
            let mut manifest = host_manifest("ok.id");
            manifest.display_name = bad.to_string();
            assert!(catalog.register(manifest).is_err(), "{bad:?}");
        }
        let mut manifest = host_manifest("ok.id");
        manifest.display_name = "  spaced name 中文 ".to_string();
        catalog.register(manifest).unwrap();
    }
}
