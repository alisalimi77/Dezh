//! Device geometry: where the hardware is, and where a grantee sees it.
//!
//! Physical MMIO bases and the virtio transport window's stride, the virtual
//! addresses a granted daemon sees its one device page at, the two DMA windows
//! (block and NIC get separate ones, so neither can corrupt the other's
//! virtqueue), and the scan that finds a virtio device by id.
//!
//! This is the second half of the shrink step 7 predicted for `proc::loader`.
//! What was left in that module's import list after `mm::paging` split was
//! exactly this: UART and virtio base addresses and DMA window geometry. The
//! loader needs them because it is what maps a device page into a process,
//! and now it names one module to get them from.

use core::ptr::read_volatile;

use crate::mm::global::Global;

// Physical geometry of the QEMU `virt` virtio-mmio transport window.
pub(crate) const DEV_UART_VA: usize = 0x5000_0000;
pub(crate) const DEV_VIRTIO_BLK_VA: usize = 0x5000_0000;
pub(crate) const VIRTIO_BLK_MMIO_PA: usize = 0x1000_1000;
pub(crate) const VIRTIO_MMIO_STRIDE: usize = 0x1000;
pub(crate) const VIRTIO_MMIO_COUNT: usize = 8;

pub(crate) const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
pub(crate) const VIRTIO_DEVICE_ID_NET: u32 = 1;
pub(crate) const VIRTIO_DEVICE_ID_BLOCK: u32 = 2;
pub(crate) const VIRTIO_MMIO_OFF_DEVICE_ID: usize = 0x008;
/// Where a Marz daemon sees its granted NIC page (one device, not the window).
pub(crate) const DEV_VIRTIO_NET_VA: usize = 0x5002_0000;
/// Marz gets its OWN DMA window. Sharing one with the block daemon would let
/// either corrupt the other's virtqueue - two devices, two grants.
pub(crate) const MARZ_DMA_VA: usize = 0x5200_0000;
pub(crate) const MARZ_DMA_SIZE: usize = 16 * 1024;
// Boot hart only: the DMA windows are staged by the console path before a
// short-lived client process is launched against them.
pub(crate) static MARZ_DMA: Global<DmaWindow> = Global::new(DmaWindow([0; MARZ_DMA_SIZE]));

pub(crate) fn marz_dma_pa() -> usize {
    MARZ_DMA.get() as usize
}

/// Scan the virtio-mmio window for a device of `want_id` and return its physical
/// base. The kernel may read the window directly (it lives in the kernel-only
/// device mapping); a daemon never scans — it receives only the single page the
/// kernel grants it.
pub(crate) fn find_virtio_mmio(want_id: u32) -> Option<usize> {
    let mut i = 0usize;
    while i < VIRTIO_MMIO_COUNT {
        let base = VIRTIO_BLK_MMIO_PA + i * VIRTIO_MMIO_STRIDE;
        let magic = unsafe { read_volatile(base as *const u32) };
        let dev = unsafe { read_volatile((base + VIRTIO_MMIO_OFF_DEVICE_ID) as *const u32) };
        if magic == VIRTIO_MMIO_MAGIC && dev == want_id {
            return Some(base);
        }
        i += 1;
    }
    None
}

pub(crate) const VIRTIO_DMA_VA: usize = 0x5100_0000;
pub(crate) const VIRTIO_DMA_SIZE: usize = 16 * 1024;
pub(crate) const VIRTIO_DATA_OFF: usize = 8_192 + 16;
pub(crate) const VIRTIO_INPUT_OFF: usize = 12_288;

#[repr(align(4096))]
#[allow(dead_code)]
pub(crate) struct DmaWindow([u8; VIRTIO_DMA_SIZE]);
pub(crate) static VIRTIO_DMA: Global<DmaWindow> = Global::new(DmaWindow([0; VIRTIO_DMA_SIZE]));
