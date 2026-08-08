//! The program loader and the per-process address space.
//!
//! A loaded program gets its OWN page table (satp) built from frames: the
//! kernel is mapped (U=0) so traps still work, and the program's segments plus
//! a stack are mapped U=1. This is the foundation for running real, separately
//! built programs, and it is what ended the "user calls kernel-resident
//! helpers" fault for good.
//!
//! Boot hart only in practice: address spaces are built and reclaimed by the
//! console's scheduler. The frame allocator underneath has the same constraint
//! and says so in `mm::frames`; W13 is where both have to answer for it.

use crate::mm::frames::FRAME_SIZE;
// The length of this list is the honest measure of how coupled the loader still
// is. Most of it is Sv39 paging vocabulary - PTE_*, L1, ROOT, pte() - which
// belongs in `mm/paging.rs` and has not been split yet. The rest is the
// task/process types the scheduler owns. Both shrink this block when their own
// modules land; until then the import list names the debt instead of hiding it
// behind a glob.
use crate::{find_virtio_mmio, frame_free, pte, AddressSpaceBuild, ProcessSpec, TaskKind, TaskResources, DEV_UART_VA, DEV_VIRTIO_BLK_VA, DEV_VIRTIO_NET_VA, EMPTY_TASK_RESOURCES, L1, MARZ_DMA, MARZ_DMA_SIZE, MARZ_DMA_VA, PTE_R, PTE_U, PTE_V, PTE_W, PTE_X, ROOT, TASK_BLOCK_READ, TASK_BLOCK_WRITE, TASK_DEVICE_VIRTIO_BLK, TASK_DEVICE_VIRTIO_NET, UART_BASE, VIRTIO_DEVICE_ID_BLOCK, VIRTIO_DEVICE_ID_NET, VIRTIO_DMA, VIRTIO_DMA_SIZE, VIRTIO_DMA_VA};

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    let mut i = 0;
    while i < 8 {
        a[i] = b[o + i];
        i += 1;
    }
    u64::from_le_bytes(a)
}

/// Walk one level of an Sv39 table, allocating the next-level table if absent.
unsafe fn walk_alloc(
    table: *mut u64,
    idx: usize,
    resources: &mut TaskResources,
) -> Option<*mut u64> {
    let e = *table.add(idx);
    if e & PTE_V != 0 {
        Some((((e >> 10) << 12) as usize) as *mut u64) // existing next table
    } else {
        let frame = resources.alloc_frame();
        if frame == 0 {
            return None;
        }
        *table.add(idx) = ((frame as u64 >> 12) << 10) | PTE_V; // non-leaf
        Some(frame as *mut u64)
    }
}

/// Map one 4 KiB page va->pa with `flags` in the page table rooted at `root`.
fn map_page(root: usize, va: usize, pa: usize, flags: u64, resources: &mut TaskResources) -> bool {
    let vpn2 = (va >> 30) & 0x1ff;
    let vpn1 = (va >> 21) & 0x1ff;
    let vpn0 = (va >> 12) & 0x1ff;
    unsafe {
        let Some(l1) = walk_alloc(root as *mut u64, vpn2, resources) else {
            return false;
        };
        let Some(l0) = walk_alloc(l1, vpn1, resources) else {
            return false;
        };
        *l0.add(vpn0) = pte(pa as u64, flags);
    }
    true
}

pub(crate) const USER_STACK_TOP: usize = 0x4070_0000;
const USER_STACK_BOTTOM: usize = 0x406F_0000;

/// Walk a page table to the frame backing `va` (page must already be mapped).
unsafe fn translate(root: usize, va: usize) -> usize {
    let vpn2 = (va >> 30) & 0x1ff;
    let vpn1 = (va >> 21) & 0x1ff;
    let vpn0 = (va >> 12) & 0x1ff;
    let l1 = (((*(root as *const u64).add(vpn2)) >> 10) << 12) as usize;
    let l0 = (((*(l1 as *const u64).add(vpn1)) >> 10) << 12) as usize;
    let leaf = *(l0 as *const u64).add(vpn0);
    ((leaf >> 10) << 12) as usize
}

/// Build a fresh address space for the embedded program. Returns (satp root, entry).
///
/// Two passes so that segments sharing a page (common: a small RX segment and an
/// R segment in the same 4 KiB page) are handled correctly: map every covered
/// page once, then copy each segment's bytes to the right intra-page offset.
pub(crate) fn reclaim_resources(resources: &mut TaskResources) {
    let mut i = 0usize;
    while i < resources.count {
        let frame = resources.frames[i];
        resources.frames[i] = 0;
        if frame != 0 {
            frame_free(frame);
        }
        i += 1;
    }
    *resources = EMPTY_TASK_RESOURCES;
}

pub(crate) fn build_address_space(spec: &ProcessSpec, kind: TaskKind) -> Option<AddressSpaceBuild> {
    let img = spec.elf;
    let mut resources = TaskResources::new(kind);
    let root = resources.alloc_frame();
    if root == 0 {
        return None;
    }
    resources.root = root;
    unsafe {
        let r = root as *mut u64;
        // Kernel mappings so traps resolve while this satp is active (U=0):
        *r.add(0) = pte(0x0, PTE_R | PTE_W | PTE_X); // 0..1 GiB gigapage (UART etc)
        let l1_pa = core::ptr::addr_of!(L1) as u64; // share the kernel's 0x8000_0000 L1
        *r.add(2) = ((l1_pa >> 12) << 10) | PTE_V;
    }

    let entry = u64_at(img, 24) as usize;
    let phoff = u64_at(img, 32) as usize;
    let phentsize = u16_at(img, 54) as usize;
    let phnum = u16_at(img, 56) as usize;

    // Pass 1: find the page-aligned VA span of all PT_LOAD segments and map it.
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if u32_at(img, ph) != 1 {
            continue;
        }
        let pv = u64_at(img, ph + 16) as usize;
        let pm = u64_at(img, ph + 40) as usize;
        lo = lo.min(pv & !0xfff);
        hi = hi.max((pv + pm + 0xfff) & !0xfff);
    }
    let mut va = lo;
    while va < hi {
        let frame = resources.alloc_frame();
        if frame == 0 {
            reclaim_resources(&mut resources);
            return None;
        }
        // W^X: derive permissions from the ELF segment flags covering this page —
        // executable code is mapped R+X (never writable), data R+W (never
        // executable). (Linux historically allowed W+X; we don't.)
        let mut fl = PTE_U | PTE_R;
        for i in 0..phnum {
            let ph = phoff + i * phentsize;
            if u32_at(img, ph) != 1 {
                continue;
            }
            let pv = u64_at(img, ph + 16) as usize;
            let pm = u64_at(img, ph + 40) as usize;
            if va >= (pv & !0xfff) && va < ((pv + pm + 0xfff) & !0xfff) {
                let pf = u32_at(img, ph + 4); // PF_X=1, PF_W=2, PF_R=4
                if pf & 1 != 0 {
                    fl |= PTE_X;
                }
                if pf & 2 != 0 {
                    fl |= PTE_W;
                }
            }
        }
        if !map_page(root, va, frame, fl, &mut resources) {
            reclaim_resources(&mut resources);
            return None;
        }
        va += FRAME_SIZE;
    }

    // Pass 2: copy each segment's file bytes to the correct virtual addresses.
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if u32_at(img, ph) != 1 {
            continue;
        }
        let poff = u64_at(img, ph + 8) as usize;
        let pvaddr = u64_at(img, ph + 16) as usize;
        let pfilesz = u64_at(img, ph + 32) as usize;
        let mut k = 0usize;
        while k < pfilesz {
            let dva = pvaddr + k;
            let frame = unsafe { translate(root, dva & !0xfff) };
            unsafe { *((frame + (dva & 0xfff)) as *mut u8) = img[poff + k] };
            k += 1;
        }
    }

    // Map the user stack (U=1 RW).
    let mut s = USER_STACK_BOTTOM;
    while s < USER_STACK_TOP {
        let frame = resources.alloc_frame();
        if frame == 0 {
            reclaim_resources(&mut resources);
            return None;
        }
        if !map_page(root, s, frame, PTE_U | PTE_R | PTE_W, &mut resources) {
            reclaim_resources(&mut resources);
            return None;
        }
        s += FRAME_SIZE;
    }

    // Device grants are explicit: no process sees MMIO unless its launch spec
    // maps that device. Drivers are user processes with device capabilities,
    // not kernel code with ambient hardware reach.
    if spec.map_uart
        && !map_page(
            root,
            DEV_UART_VA,
            UART_BASE as usize,
            PTE_U | PTE_R | PTE_W,
            &mut resources,
        ) {
            reclaim_resources(&mut resources);
            return None;
        }
    // Per-device grant: the kernel finds the block device and maps ONLY its page.
    // (This used to map the whole virtio-mmio transport window, handing the block
    // daemon authority over every other device on the bus.)
    if spec.map_virtio_blk && spec.caps & TASK_DEVICE_VIRTIO_BLK != 0 {
        let Some(blk_pa) = find_virtio_mmio(VIRTIO_DEVICE_ID_BLOCK) else {
            reclaim_resources(&mut resources);
            return None;
        };
        if !map_page(
            root,
            DEV_VIRTIO_BLK_VA,
            blk_pa,
            PTE_U | PTE_R | PTE_W,
            &mut resources,
        ) {
            reclaim_resources(&mut resources);
            return None;
        }
    }
    // Marz: grant exactly ONE device page — the NIC the kernel discovered — under
    // its own capability. Unlike the block grant above, the daemon never sees the
    // rest of the virtio-mmio window.
    if spec.map_virtio_net && spec.caps & TASK_DEVICE_VIRTIO_NET != 0 {
        let Some(nic_pa) = find_virtio_mmio(VIRTIO_DEVICE_ID_NET) else {
            reclaim_resources(&mut resources);
            return None;
        };
        if !map_page(
            root,
            DEV_VIRTIO_NET_VA,
            nic_pa,
            PTE_U | PTE_R | PTE_W,
            &mut resources,
        ) {
            reclaim_resources(&mut resources);
            return None;
        }
        let marz_dma = core::ptr::addr_of!(MARZ_DMA) as usize;
        let mut off = 0usize;
        while off < MARZ_DMA_SIZE {
            if !map_page(
                root,
                MARZ_DMA_VA + off,
                marz_dma + off,
                PTE_U | PTE_R | PTE_W,
                &mut resources,
            ) {
                reclaim_resources(&mut resources);
                return None;
            }
            off += 4096;
        }
    }
    if spec.map_virtio_dma
        && spec.caps & (TASK_BLOCK_READ | TASK_BLOCK_WRITE | TASK_DEVICE_VIRTIO_NET) != 0
    {
        let dma_pa = core::ptr::addr_of!(VIRTIO_DMA) as usize;
        let mut off = 0usize;
        while off < VIRTIO_DMA_SIZE {
            if !map_page(
                root,
                VIRTIO_DMA_VA + off,
                dma_pa + off,
                PTE_U | PTE_R | PTE_W,
                &mut resources,
            ) {
                reclaim_resources(&mut resources);
                return None;
            }
            off += FRAME_SIZE;
        }
    }

    Some(AddressSpaceBuild {
        root,
        entry,
        resources,
    })
}

/// The kernel's own satp (the global identity address space the console runs in).
pub(crate) fn kernel_satp() -> usize {
    (8usize << 60) | ((core::ptr::addr_of!(ROOT) as usize) >> 12)
}

/// satp value for a process whose page table root is at `root`.
pub(crate) fn proc_satp(root: usize) -> usize {
    (8usize << 60) | (root >> 12)
}
