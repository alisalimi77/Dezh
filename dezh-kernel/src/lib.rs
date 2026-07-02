#![no_std]

//! # dezh-kernel - Step 10: minimal kernel boot contract scaffold
//!
//! This crate is intentionally not marked as a validated QEMU boot. It is the
//! first kernel-facing contract: what a minimal boot path must hand to the
//! kernel, which services init must launch, and which explicit capabilities
//! seed those services. Keeping this no_std-compatible prevents the model from
//! depending on host-process conveniences.

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub const KERNEL_CONTRACT_VERSION: &str = "dezh-kernel-boot-v0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootTarget {
    QemuVirtioX86_64,
    QemuVirtioRiscV64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    Usable,
    Reserved,
    Bootloader,
    Kernel,
    Framebuffer,
    Mmio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion {
    pub start: u64,
    pub len: u64,
    pub kind: MemoryKind,
}

impl MemoryRegion {
    pub fn new(start: u64, len: u64, kind: MemoryKind) -> Self {
        MemoryRegion { start, len, kind }
    }

    pub fn end(self) -> Option<u64> {
        self.start.checked_add(self.len)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelCapability {
    AllocateFrames,
    MapAddressSpace,
    StartService,
    SendIpc,
    OpenVirtioDevice,
    OpenCairnRoot,
    OpenWasmRuntime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilitySeed {
    pub service: &'static str,
    pub capability: KernelCapability,
}

impl CapabilitySeed {
    pub fn new(service: &'static str, capability: KernelCapability) -> Self {
        CapabilitySeed {
            service,
            capability,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    Init,
    Cairn,
    WasmRuntime,
    VirtioBlock,
    VirtioNet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceSpec {
    pub name: &'static str,
    pub kind: ServiceKind,
    pub required_caps: Vec<KernelCapability>,
}

impl ServiceSpec {
    pub fn new(
        name: &'static str,
        kind: ServiceKind,
        required_caps: impl Into<Vec<KernelCapability>>,
    ) -> Self {
        ServiceSpec {
            name,
            kind,
            required_caps: required_caps.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootInfo {
    pub target: BootTarget,
    pub memory: Vec<MemoryRegion>,
    pub init_services: Vec<ServiceSpec>,
    pub capability_seeds: Vec<CapabilitySeed>,
    pub install_manifest: InstallManifest,
}

impl BootInfo {
    pub fn qemu_minimal(memory: Vec<MemoryRegion>) -> Self {
        BootInfo {
            target: BootTarget::QemuVirtioX86_64,
            memory,
            init_services: vec![
                ServiceSpec::new(
                    "init",
                    ServiceKind::Init,
                    [KernelCapability::StartService, KernelCapability::SendIpc],
                ),
                ServiceSpec::new(
                    "cairn",
                    ServiceKind::Cairn,
                    [KernelCapability::OpenCairnRoot, KernelCapability::SendIpc],
                ),
                ServiceSpec::new(
                    "wasm-runtime",
                    ServiceKind::WasmRuntime,
                    [KernelCapability::OpenWasmRuntime, KernelCapability::SendIpc],
                ),
                ServiceSpec::new(
                    "virtio-block",
                    ServiceKind::VirtioBlock,
                    [
                        KernelCapability::OpenVirtioDevice,
                        KernelCapability::SendIpc,
                    ],
                ),
            ],
            capability_seeds: vec![
                CapabilitySeed::new("init", KernelCapability::StartService),
                CapabilitySeed::new("init", KernelCapability::SendIpc),
                CapabilitySeed::new("cairn", KernelCapability::OpenCairnRoot),
                CapabilitySeed::new("cairn", KernelCapability::SendIpc),
                CapabilitySeed::new("wasm-runtime", KernelCapability::OpenWasmRuntime),
                CapabilitySeed::new("wasm-runtime", KernelCapability::SendIpc),
                CapabilitySeed::new("virtio-block", KernelCapability::OpenVirtioDevice),
                CapabilitySeed::new("virtio-block", KernelCapability::SendIpc),
            ],
            install_manifest: InstallManifest::qemu_v0(),
        }
    }

    /// Same minimal init plan, but targeting the QEMU `virt` RISC-V machine —
    /// the board the real `dezh-boot` kernel boots on. Services and explicit
    /// capability seeds are identical; only the boot target differs.
    pub fn qemu_minimal_riscv(memory: Vec<MemoryRegion>) -> Self {
        let mut info = Self::qemu_minimal(memory);
        info.target = BootTarget::QemuVirtioRiscV64;
        info.install_manifest = InstallManifest::qemu_riscv_v0();
        info
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelPlan {
    pub target: BootTarget,
    pub usable_bytes: u64,
    pub services: Vec<ServiceSpec>,
    pub capability_seeds: Vec<CapabilitySeed>,
    pub install_manifest: InstallManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskLayout {
    pub marker_sector: u64,
    pub cairn_current_sector: u64,
    pub cairn_previous_sector: u64,
    pub root_metadata_sector: u64,
    pub root_metadata_sectors: u64,
}

impl DiskLayout {
    pub const fn qemu_v0() -> Self {
        DiskLayout {
            marker_sector: 0,
            cairn_current_sector: 2,
            cairn_previous_sector: 3,
            root_metadata_sector: 4,
            root_metadata_sectors: 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallManifest {
    pub version: u32,
    pub target: BootTarget,
    pub root_service: &'static str,
    pub block_service: &'static str,
    pub layout: DiskLayout,
}

impl InstallManifest {
    pub const fn qemu_v0() -> Self {
        InstallManifest {
            version: 0,
            target: BootTarget::QemuVirtioX86_64,
            root_service: "cairn",
            block_service: "virtio-block",
            layout: DiskLayout::qemu_v0(),
        }
    }

    pub const fn qemu_riscv_v0() -> Self {
        InstallManifest {
            version: 0,
            target: BootTarget::QemuVirtioRiscV64,
            root_service: "cairn",
            block_service: "virtio-block",
            layout: DiskLayout::qemu_v0(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootError {
    EmptyMemoryMap,
    EmptyRegion,
    RegionOverflow,
    OverlappingRegions,
    NoUsableMemory,
    MissingInitService,
    MissingRequiredCapability {
        service: &'static str,
        capability: KernelCapability,
    },
    AmbientCapabilitySeed,
    MissingInstallService,
    InvalidInstallTarget,
    InvalidRootStorage,
}

pub fn plan_boot(info: &BootInfo) -> Result<KernelPlan, BootError> {
    validate_memory_map(&info.memory)?;
    validate_services(&info.init_services, &info.capability_seeds)?;
    validate_install_manifest(&info.install_manifest, &info.init_services, info.target)?;
    Ok(KernelPlan {
        target: info.target,
        usable_bytes: info
            .memory
            .iter()
            .filter(|r| r.kind == MemoryKind::Usable)
            .map(|r| r.len)
            .sum(),
        services: info.init_services.clone(),
        capability_seeds: info.capability_seeds.clone(),
        install_manifest: info.install_manifest.clone(),
    })
}

pub fn validate_install_manifest(
    manifest: &InstallManifest,
    services: &[ServiceSpec],
    target: BootTarget,
) -> Result<(), BootError> {
    if manifest.target != target {
        return Err(BootError::InvalidInstallTarget);
    }
    if !services
        .iter()
        .any(|service| service.name == manifest.root_service)
        || !services
            .iter()
            .any(|service| service.name == manifest.block_service)
    {
        return Err(BootError::MissingInstallService);
    }
    let layout = &manifest.layout;
    if layout.root_metadata_sectors == 0
        || layout.marker_sector == layout.cairn_current_sector
        || layout.marker_sector == layout.cairn_previous_sector
        || layout.marker_sector == layout.root_metadata_sector
        || layout.cairn_current_sector == layout.cairn_previous_sector
        || layout.root_metadata_sector <= layout.cairn_previous_sector
    {
        return Err(BootError::InvalidRootStorage);
    }
    Ok(())
}

pub fn validate_memory_map(regions: &[MemoryRegion]) -> Result<(), BootError> {
    if regions.is_empty() {
        return Err(BootError::EmptyMemoryMap);
    }

    let mut normalized = Vec::with_capacity(regions.len());
    for region in regions {
        if region.len == 0 {
            return Err(BootError::EmptyRegion);
        }
        let end = region.end().ok_or(BootError::RegionOverflow)?;
        normalized.push((region.start, end, region.kind));
    }
    normalized.sort_by_key(|(start, _, _)| *start);

    let mut has_usable = false;
    let mut previous_end = None;
    for (start, end, kind) in normalized {
        if previous_end.is_some_and(|prev| start < prev) {
            return Err(BootError::OverlappingRegions);
        }
        previous_end = Some(end);
        has_usable |= kind == MemoryKind::Usable;
    }
    if !has_usable {
        return Err(BootError::NoUsableMemory);
    }
    Ok(())
}

pub fn validate_services(
    services: &[ServiceSpec],
    seeds: &[CapabilitySeed],
) -> Result<(), BootError> {
    if !services
        .iter()
        .any(|service| service.kind == ServiceKind::Init)
    {
        return Err(BootError::MissingInitService);
    }

    for seed in seeds {
        if !services.iter().any(|service| service.name == seed.service) {
            return Err(BootError::AmbientCapabilitySeed);
        }
    }

    for service in services {
        for required in &service.required_caps {
            let has_seed = seeds
                .iter()
                .any(|seed| seed.service == service.name && seed.capability == *required);
            if !has_seed {
                return Err(BootError::MissingRequiredCapability {
                    service: service.name,
                    capability: *required,
                });
            }
        }
    }
    Ok(())
}

pub fn boot_banner(plan: &KernelPlan) -> String {
    let target = match plan.target {
        BootTarget::QemuVirtioX86_64 => "qemu-virtio-x86_64",
        BootTarget::QemuVirtioRiscV64 => "qemu-virtio-riscv64",
    };
    alloc::format!(
        "{KERNEL_CONTRACT_VERSION}:{target}:services={}:usable_bytes={}",
        plan.services.len(),
        plan.usable_bytes
    )
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn memory() -> Vec<MemoryRegion> {
        vec![
            MemoryRegion::new(0, 0x100000, MemoryKind::Reserved),
            MemoryRegion::new(0x100000, 0x4000000, MemoryKind::Usable),
            MemoryRegion::new(0x4100000, 0x100000, MemoryKind::Kernel),
        ]
    }

    #[test]
    fn qemu_minimal_plan_launches_expected_user_space_services() {
        let info = BootInfo::qemu_minimal(memory());

        let plan = plan_boot(&info).unwrap();

        assert_eq!(plan.target, BootTarget::QemuVirtioX86_64);
        assert_eq!(plan.usable_bytes, 0x4000000);
        assert!(plan.services.iter().any(|s| s.kind == ServiceKind::Init));
        assert!(plan.services.iter().any(|s| s.kind == ServiceKind::Cairn));
        assert!(plan
            .services
            .iter()
            .any(|s| s.kind == ServiceKind::WasmRuntime));
    }

    #[test]
    fn qemu_riscv_plan_uses_riscv_target_and_banner() {
        let info = BootInfo::qemu_minimal_riscv(memory());

        let plan = plan_boot(&info).unwrap();

        assert_eq!(plan.target, BootTarget::QemuVirtioRiscV64);
        let banner = boot_banner(&plan);
        assert!(banner.starts_with("dezh-kernel-boot-v0:qemu-virtio-riscv64"));
        assert!(plan.services.iter().any(|s| s.kind == ServiceKind::Init));
    }

    #[test]
    fn overlapping_memory_regions_are_rejected() {
        let err = validate_memory_map(&[
            MemoryRegion::new(0x1000, 0x2000, MemoryKind::Usable),
            MemoryRegion::new(0x2000, 0x2000, MemoryKind::Kernel),
        ])
        .unwrap_err();

        assert_eq!(err, BootError::OverlappingRegions);
    }

    #[test]
    fn memory_map_requires_usable_memory() {
        let err =
            validate_memory_map(&[MemoryRegion::new(0, 0x1000, MemoryKind::Reserved)]).unwrap_err();

        assert_eq!(err, BootError::NoUsableMemory);
    }

    #[test]
    fn every_required_service_capability_must_be_seeded_explicitly() {
        let services = vec![ServiceSpec::new(
            "init",
            ServiceKind::Init,
            [KernelCapability::StartService, KernelCapability::SendIpc],
        )];
        let seeds = vec![CapabilitySeed::new("init", KernelCapability::StartService)];

        let err = validate_services(&services, &seeds).unwrap_err();

        assert_eq!(
            err,
            BootError::MissingRequiredCapability {
                service: "init",
                capability: KernelCapability::SendIpc
            }
        );
    }

    #[test]
    fn capability_seed_for_unknown_service_is_ambient_authority() {
        let services = vec![ServiceSpec::new(
            "init",
            ServiceKind::Init,
            [KernelCapability::StartService],
        )];
        let seeds = vec![
            CapabilitySeed::new("init", KernelCapability::StartService),
            CapabilitySeed::new("ghost-service", KernelCapability::OpenCairnRoot),
        ];

        let err = validate_services(&services, &seeds).unwrap_err();

        assert_eq!(err, BootError::AmbientCapabilitySeed);
    }

    #[test]
    fn boot_banner_is_stable_and_says_this_is_contract_v0() {
        let plan = plan_boot(&BootInfo::qemu_minimal(memory())).unwrap();

        let banner = boot_banner(&plan);

        assert!(banner.starts_with("dezh-kernel-boot-v0:qemu-virtio-x86_64"));
        assert!(banner.contains("services=4"));
    }

    #[test]
    fn install_manifest_is_validated_with_boot_contract() {
        let info = BootInfo::qemu_minimal_riscv(memory());

        let plan = plan_boot(&info).unwrap();

        assert_eq!(plan.install_manifest.target, BootTarget::QemuVirtioRiscV64);
        assert_eq!(plan.install_manifest.root_service, "cairn");
        assert_eq!(plan.install_manifest.block_service, "virtio-block");
        assert_eq!(plan.install_manifest.layout.marker_sector, 0);
    }

    #[test]
    fn install_manifest_rejects_missing_services() {
        let mut info = BootInfo::qemu_minimal(memory());
        info.install_manifest.root_service = "missing-root";

        let err = plan_boot(&info).unwrap_err();

        assert_eq!(err, BootError::MissingInstallService);
    }

    #[test]
    fn install_manifest_rejects_invalid_root_storage() {
        let mut info = BootInfo::qemu_minimal(memory());
        info.install_manifest.layout.root_metadata_sectors = 0;

        let err = plan_boot(&info).unwrap_err();

        assert_eq!(err, BootError::InvalidRootStorage);
    }
}
