//! # dezh-ir - Step 8: Dezh IR/WASM runtime contract
//!
//! Dezh's long-term native execution substrate is a typed, verifiable IR. For
//! now that practical substrate is core WebAssembly, but it needs a Dezh-owned
//! contract instead of an informal set of imports hidden in each runtime crate.
//!
//! This crate validates a wasm module before execution:
//! - no WASI, `env`, or ambient imports,
//! - only the capability-mediated `dezh` host surface is allowed,
//! - exported linear memory is required,
//! - `run() -> i64` is the v0 entrypoint,
//! - compiled-code cache keys are content-addressed and contract-versioned.

use std::fmt;

use wasmtime::{Engine, ExternType, Module, ValType};

pub const CONTRACT_VERSION: &str = "dezh-ir-v0";
pub const DEZH_IMPORT_MODULE: &str = "dezh";
pub const MEMORY_EXPORT: &str = "memory";
pub const ENTRYPOINT_EXPORT: &str = "run";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    I32,
    I64,
    F32,
    F64,
    V128,
    FuncRef,
    ExternRef,
    Unknown,
}

impl From<&ValType> for ValueKind {
    fn from(value: &ValType) -> Self {
        match value {
            ValType::I32 => ValueKind::I32,
            ValType::I64 => ValueKind::I64,
            ValType::F32 => ValueKind::F32,
            ValType::F64 => ValueKind::F64,
            ValType::V128 => ValueKind::V128,
            ValType::Ref(_) => ValueKind::Unknown,
        }
    }
}

impl fmt::Display for ValueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueKind::I32 => write!(f, "i32"),
            ValueKind::I64 => write!(f, "i64"),
            ValueKind::F32 => write!(f, "f32"),
            ValueKind::F64 => write!(f, "f64"),
            ValueKind::V128 => write!(f, "v128"),
            ValueKind::FuncRef => write!(f, "funcref"),
            ValueKind::ExternRef => write!(f, "externref"),
            ValueKind::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSig {
    params: Vec<ValueKind>,
    results: Vec<ValueKind>,
}

impl FunctionSig {
    pub fn new(params: impl Into<Vec<ValueKind>>, results: impl Into<Vec<ValueKind>>) -> Self {
        FunctionSig {
            params: params.into(),
            results: results.into(),
        }
    }

    pub fn params(&self) -> &[ValueKind] {
        &self.params
    }

    pub fn results(&self) -> &[ValueKind] {
        &self.results
    }
}

impl fmt::Display for FunctionSig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(")?;
        for (i, param) in self.params.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{param}")?;
        }
        write!(f, ") -> ")?;
        match self.results.as_slice() {
            [] => write!(f, "()"),
            [one] => write!(f, "{one}"),
            many => {
                write!(f, "(")?;
                for (i, result) in many.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{result}")?;
                }
                write!(f, ")")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostImport {
    CapRead,
    CapWrite,
    CapPrint,
    CapAttenuate,
}

impl HostImport {
    pub fn name(self) -> &'static str {
        match self {
            HostImport::CapRead => "cap_read",
            HostImport::CapWrite => "cap_write",
            HostImport::CapPrint => "cap_print",
            HostImport::CapAttenuate => "cap_attenuate",
        }
    }

    pub fn signature(self) -> FunctionSig {
        match self {
            HostImport::CapRead => FunctionSig::new(
                [ValueKind::I32, ValueKind::I32, ValueKind::I32],
                [ValueKind::I32],
            ),
            HostImport::CapWrite => FunctionSig::new(
                [ValueKind::I32, ValueKind::I32, ValueKind::I32],
                [ValueKind::I32],
            ),
            HostImport::CapPrint => FunctionSig::new(
                [ValueKind::I32, ValueKind::I32, ValueKind::I32],
                [ValueKind::I32],
            ),
            HostImport::CapAttenuate => {
                FunctionSig::new([ValueKind::I32, ValueKind::I32], [ValueKind::I64])
            }
        }
    }

    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "cap_read" => Some(HostImport::CapRead),
            "cap_write" => Some(HostImport::CapWrite),
            "cap_print" => Some(HostImport::CapPrint),
            "cap_attenuate" => Some(HostImport::CapAttenuate),
            _ => None,
        }
    }

    /// The maximal known `dezh` host surface. A concrete host (e.g. the Cairn
    /// runtime) declares its own, usually narrower, surface and passes it to
    /// [`validate_module_with`]; this constant is only the default for the
    /// permissive [`validate_module`].
    pub const ALL: &'static [HostImport] = &[
        HostImport::CapRead,
        HostImport::CapWrite,
        HostImport::CapPrint,
        HostImport::CapAttenuate,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportUse {
    pub module: String,
    pub name: String,
    pub signature: FunctionSig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportUse {
    Memory {
        name: String,
    },
    Function {
        name: String,
        signature: FunctionSig,
    },
    Other {
        name: String,
        kind: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheKey(String);

impl CacheKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleReport {
    pub contract_version: &'static str,
    pub imports: Vec<ImportUse>,
    pub exports: Vec<ExportUse>,
    pub cache_key: CacheKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContractViolation {
    InvalidWasm(String),
    ForbiddenImport {
        module: String,
        name: String,
    },
    UnknownDezhImport {
        name: String,
    },
    DisallowedDezhImport {
        name: String,
    },
    ImportMustBeFunction {
        name: String,
        found: &'static str,
    },
    WrongImportSignature {
        name: String,
        expected: FunctionSig,
        found: FunctionSig,
    },
    MissingMemoryExport,
    MissingRunExport,
    RunMustBeFunction {
        found: &'static str,
    },
    WrongRunSignature {
        expected: FunctionSig,
        found: FunctionSig,
    },
}

impl fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContractViolation::InvalidWasm(e) => write!(f, "invalid wasm: {e}"),
            ContractViolation::ForbiddenImport { module, name } => {
                write!(f, "forbidden import {module}::{name}")
            }
            ContractViolation::UnknownDezhImport { name } => {
                write!(f, "unknown dezh import {name}")
            }
            ContractViolation::DisallowedDezhImport { name } => {
                write!(f, "dezh import {name} is not offered by this host")
            }
            ContractViolation::ImportMustBeFunction { name, found } => {
                write!(f, "dezh import {name} must be a function, found {found}")
            }
            ContractViolation::WrongImportSignature {
                name,
                expected,
                found,
            } => {
                write!(
                    f,
                    "dezh import {name} has wrong signature: expected {expected}, found {found}"
                )
            }
            ContractViolation::MissingMemoryExport => write!(f, "missing exported memory"),
            ContractViolation::MissingRunExport => write!(f, "missing run export"),
            ContractViolation::RunMustBeFunction { found } => {
                write!(f, "run export must be a function, found {found}")
            }
            ContractViolation::WrongRunSignature { expected, found } => {
                write!(
                    f,
                    "run export has wrong signature: expected {expected}, found {found}"
                )
            }
        }
    }
}

impl std::error::Error for ContractViolation {}

pub type ValidationResult = Result<ModuleReport, Vec<ContractViolation>>;

pub fn compiled_cache_key(wasm: &[u8]) -> CacheKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CONTRACT_VERSION.as_bytes());
    hasher.update(b"\0");
    hasher.update(wasm);
    CacheKey(hasher.finalize().to_hex().to_string())
}

/// Validate against the maximal known `dezh` host surface. Convenience wrapper
/// over [`validate_module_with`] for callers that accept every defined host
/// import. Concrete hosts should instead pass their own (usually narrower)
/// surface so a module importing a function the host does not actually offer is
/// rejected up front rather than failing later at instantiation.
pub fn validate_module(wasm: &[u8]) -> ValidationResult {
    validate_module_with(wasm, HostImport::ALL)
}

/// Validate a module against `allowed`, the exact set of `dezh` host functions
/// the target host provides. A known host import outside `allowed` is rejected
/// as [`ContractViolation::DisallowedDezhImport`].
pub fn validate_module_with(wasm: &[u8], allowed: &[HostImport]) -> ValidationResult {
    let engine = Engine::default();
    let module = match Module::new(&engine, wasm) {
        Ok(module) => module,
        Err(e) => return Err(vec![ContractViolation::InvalidWasm(e.to_string())]),
    };

    let mut violations = Vec::new();
    let mut imports = Vec::new();

    for import in module.imports() {
        let module_name = import.module();
        let name = import.name();
        if module_name != DEZH_IMPORT_MODULE {
            violations.push(ContractViolation::ForbiddenImport {
                module: module_name.to_string(),
                name: name.to_string(),
            });
            continue;
        }

        let allowed_import = match HostImport::by_name(name) {
            Some(known) => known,
            None => {
                violations.push(ContractViolation::UnknownDezhImport {
                    name: name.to_string(),
                });
                continue;
            }
        };

        // The import is a defined host function, but this host may not offer it.
        if !allowed.contains(&allowed_import) {
            violations.push(ContractViolation::DisallowedDezhImport {
                name: name.to_string(),
            });
            continue;
        }

        let found = match import.ty() {
            ExternType::Func(func) => function_sig(&func),
            other => {
                violations.push(ContractViolation::ImportMustBeFunction {
                    name: name.to_string(),
                    found: extern_kind(&other),
                });
                continue;
            }
        };
        let expected = allowed_import.signature();
        if found != expected {
            violations.push(ContractViolation::WrongImportSignature {
                name: name.to_string(),
                expected,
                found: found.clone(),
            });
        }
        imports.push(ImportUse {
            module: module_name.to_string(),
            name: name.to_string(),
            signature: found,
        });
    }

    let mut exports = Vec::new();
    let mut has_memory = false;
    let mut run_export = None;

    for export in module.exports() {
        let name = export.name().to_string();
        match export.ty() {
            ExternType::Memory(_) => {
                if name == MEMORY_EXPORT {
                    has_memory = true;
                }
                exports.push(ExportUse::Memory { name });
            }
            ExternType::Func(func) => {
                let sig = function_sig(&func);
                if name == ENTRYPOINT_EXPORT {
                    run_export = Some(sig.clone());
                }
                exports.push(ExportUse::Function {
                    name,
                    signature: sig,
                });
            }
            other => {
                if name == ENTRYPOINT_EXPORT {
                    violations.push(ContractViolation::RunMustBeFunction {
                        found: extern_kind(&other),
                    });
                }
                exports.push(ExportUse::Other {
                    name,
                    kind: extern_kind(&other),
                });
            }
        }
    }

    if !has_memory {
        violations.push(ContractViolation::MissingMemoryExport);
    }

    match run_export {
        Some(found) => {
            let expected = FunctionSig::new([], [ValueKind::I64]);
            if found != expected {
                violations.push(ContractViolation::WrongRunSignature { expected, found });
            }
        }
        None => {
            if !violations
                .iter()
                .any(|v| matches!(v, ContractViolation::RunMustBeFunction { .. }))
            {
                violations.push(ContractViolation::MissingRunExport);
            }
        }
    }

    if violations.is_empty() {
        Ok(ModuleReport {
            contract_version: CONTRACT_VERSION,
            imports,
            exports,
            cache_key: compiled_cache_key(wasm),
        })
    } else {
        Err(violations)
    }
}

fn function_sig(func: &wasmtime::FuncType) -> FunctionSig {
    FunctionSig::new(
        func.params()
            .map(|v| ValueKind::from(&v))
            .collect::<Vec<_>>(),
        func.results()
            .map(|v| ValueKind::from(&v))
            .collect::<Vec<_>>(),
    )
}

fn extern_kind(ty: &ExternType) -> &'static str {
    match ty {
        ExternType::Func(_) => "func",
        ExternType::Global(_) => "global",
        ExternType::Table(_) => "table",
        ExternType::Memory(_) => "memory",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleMetadata {
    pub name: String,
    pub principal_hint: Option<String>,
    pub provenance_hook: Option<String>,
}

impl ModuleMetadata {
    pub fn new(name: impl Into<String>) -> Self {
        ModuleMetadata {
            name: name.into(),
            principal_hint: None,
            provenance_hook: None,
        }
    }

    pub fn with_principal_hint(mut self, principal: impl Into<String>) -> Self {
        self.principal_hint = Some(principal.into());
        self
    }

    pub fn with_provenance_hook(mut self, hook: impl Into<String>) -> Self {
        self.provenance_hook = Some(hook.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasm(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).unwrap()
    }

    fn valid_module() -> Vec<u8> {
        wasm(
            r#"
            (module
              (import "dezh" "cap_read" (func $cap_read (param i32 i32 i32) (result i32)))
              (import "dezh" "cap_write" (func $cap_write (param i32 i32 i32) (result i32)))
              (import "dezh" "cap_print" (func $cap_print (param i32 i32 i32) (result i32)))
              (import "dezh" "cap_attenuate" (func $cap_attenuate (param i32 i32) (result i64)))
              (memory (export "memory") 1)
              (func (export "run") (result i64)
                (i64.const 0)))
            "#,
        )
    }

    #[test]
    fn accepts_current_dezh_import_contract() {
        let report = validate_module(&valid_module()).unwrap();

        assert_eq!(report.contract_version, CONTRACT_VERSION);
        assert_eq!(report.imports.len(), 4);
        assert!(report
            .exports
            .iter()
            .any(|e| matches!(e, ExportUse::Memory { name } if name == "memory")));
        assert!(report
            .exports
            .iter()
            .any(|e| matches!(e, ExportUse::Function { name, signature } if name == "run" && signature.results() == [ValueKind::I64])));
    }

    #[test]
    fn rejects_wasi_imports() {
        let violations = validate_module(&wasm(
            r#"
            (module
              (import "wasi_snapshot_preview1" "fd_write"
                (func $fd_write (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run") (result i64) (i64.const 0)))
            "#,
        ))
        .unwrap_err();

        assert!(violations.iter().any(|v| matches!(
            v,
            ContractViolation::ForbiddenImport { module, name }
                if module == "wasi_snapshot_preview1" && name == "fd_write"
        )));
    }

    #[test]
    fn rejects_dezh_import_not_offered_by_host() {
        // valid_module imports all four host functions; a host that only offers
        // cap_read must reject the other three up front.
        let violations =
            validate_module_with(&valid_module(), &[HostImport::CapRead]).unwrap_err();

        for missing in ["cap_write", "cap_print", "cap_attenuate"] {
            assert!(violations.iter().any(|v| matches!(
                v,
                ContractViolation::DisallowedDezhImport { name } if name == missing
            )));
        }
    }

    #[test]
    fn rejects_unknown_dezh_imports() {
        let violations = validate_module(&wasm(
            r#"
            (module
              (import "dezh" "cap_network" (func $cap_network (param i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run") (result i64) (i64.const 0)))
            "#,
        ))
        .unwrap_err();

        assert!(violations.iter().any(|v| {
            matches!(v, ContractViolation::UnknownDezhImport { name } if name == "cap_network")
        }));
    }

    #[test]
    fn rejects_wrong_import_signature() {
        let violations = validate_module(&wasm(
            r#"
            (module
              (import "dezh" "cap_write" (func $cap_write (param i32 i32) (result i64)))
              (memory (export "memory") 1)
              (func (export "run") (result i64) (i64.const 0)))
            "#,
        ))
        .unwrap_err();

        assert!(violations.iter().any(|v| matches!(
            v,
            ContractViolation::WrongImportSignature { name, .. } if name == "cap_write"
        )));
    }

    #[test]
    fn rejects_missing_memory_export() {
        let violations = validate_module(&wasm(
            r#"
            (module
              (func (export "run") (result i64) (i64.const 0)))
            "#,
        ))
        .unwrap_err();

        assert!(violations
            .iter()
            .any(|v| matches!(v, ContractViolation::MissingMemoryExport)));
    }

    #[test]
    fn rejects_wrong_run_signature() {
        let violations = validate_module(&wasm(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "run") (param i32) (result i32)
                (local.get 0)))
            "#,
        ))
        .unwrap_err();

        assert!(violations.iter().any(|v| matches!(
            v,
            ContractViolation::WrongRunSignature { found, .. }
                if found.params() == [ValueKind::I32] && found.results() == [ValueKind::I32]
        )));
    }

    #[test]
    fn cache_key_is_stable_versioned_and_content_addressed() {
        let one = valid_module();
        let two = wasm(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "run") (result i64)
                (i64.const 1)))
            "#,
        );

        assert_eq!(compiled_cache_key(&one), compiled_cache_key(&one));
        assert_ne!(compiled_cache_key(&one), compiled_cache_key(&two));

        let mut manual = blake3::Hasher::new();
        manual.update(CONTRACT_VERSION.as_bytes());
        manual.update(b"\0");
        manual.update(&one);
        assert_eq!(
            compiled_cache_key(&one).as_str(),
            manual.finalize().to_hex().to_string()
        );
    }

    #[test]
    fn metadata_carries_future_provenance_hooks_without_parsing_custom_sections() {
        let metadata = ModuleMetadata::new("agent-writer")
            .with_principal_hint("agent:writer")
            .with_provenance_hook("cairn.invocation.v0");

        assert_eq!(metadata.name, "agent-writer");
        assert_eq!(metadata.principal_hint.as_deref(), Some("agent:writer"));
        assert_eq!(
            metadata.provenance_hook.as_deref(),
            Some("cairn.invocation.v0")
        );
    }
}
