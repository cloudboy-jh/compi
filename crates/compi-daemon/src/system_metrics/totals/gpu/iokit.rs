//! IOKit's `IOAccelerator` services: each GPU's driver keeps a `PerformanceStatistics`
//! dictionary current with its busy share and the memory it has in use. Reading it is one
//! registry property per GPU, tens of microseconds.

use compi_protocol::system_metrics::GpuTotals;
use objc2_core_foundation::{CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_io_kit::{
    IOIteratorNext, IOObjectRelease, IORegistryEntryCreateCFProperty,
    IORegistryEntryGetParentEntry, IOServiceGetMatchingServices, IOServiceMatching, io_object_t,
    kIOMainPortDefault, kIOReturnSuccess,
};
use std::ffi::c_char;

/// Reads between two lookups of the GPUs, so one attached later (an eGPU) appears within
/// a minute.
const LOOKUP_EVERY: u32 = 30;

pub struct GpuReader {
    accelerators: Vec<Accelerator>,
    reads: u32,
    /// Physical memory, which a GPU without memory of its own shares.
    memory_bytes: u64,
}

struct Accelerator {
    entry: Entry,
    name: Option<String>,
    /// Memory of its own (a discrete GPU) as the registry gives it.
    vram_bytes: Option<u64>,
}

/// An IOKit object this reader holds a reference on.
struct Entry(io_object_t);

impl Drop for Entry {
    fn drop(&mut self) {
        IOObjectRelease(self.0);
    }
}

impl GpuReader {
    pub fn new() -> Self {
        Self {
            accelerators: accelerators(),
            reads: 0,
            memory_bytes: memory_bytes(),
        }
    }

    /// The GPU with memory of its own if there is one, else the one with the most memory.
    pub fn read(&mut self) -> Option<GpuTotals> {
        self.reads += 1;
        if self.reads.is_multiple_of(LOOKUP_EVERY) {
            self.accelerators = accelerators();
        }
        self.accelerators
            .iter()
            .filter_map(|accelerator| {
                let stats = property(&accelerator.entry, "PerformanceStatistics")?
                    .downcast::<CFDictionary>()
                    .ok()?;
                // SAFETY: registry dictionaries are keyed by strings, and every value is
                // checked for its type before use.
                let stats =
                    unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(stats) };
                Some(gpu_totals(
                    accelerator.name.clone(),
                    accelerator.vram_bytes,
                    &stats,
                    self.memory_bytes,
                ))
            })
            .max_by_key(|(dedicated, totals)| (*dedicated, totals.memory_total_bytes))
            .map(|(_, totals)| totals)
    }
}

/// The totals from an accelerator's `PerformanceStatistics`, and whether the GPU has
/// memory of its own. Discrete GPUs report their own memory (`vramUsedBytes`,
/// `vramFreeBytes`); integrated ones, Apple silicon's included, what they have in use of
/// the system's.
fn gpu_totals(
    name: Option<String>,
    vram_bytes: Option<u64>,
    stats: &CFDictionary<CFString, CFType>,
    memory_bytes: u64,
) -> (bool, GpuTotals) {
    let number = |key: &'static str| {
        let value = stats.get(&CFString::from_static_str(key))?;
        u64::try_from(value.downcast::<CFNumber>().ok()?.as_i64()?).ok()
    };
    let vram_used = number("vramUsedBytes");
    let vram_free = number("vramFreeBytes");
    let vram_total = vram_bytes.or_else(|| Some(vram_used? + vram_free?));
    let (used, total) = match vram_total {
        Some(total) => (vram_used.or_else(|| total.checked_sub(vram_free?)), total),
        None => (number("In use system memory"), memory_bytes),
    };
    (
        vram_total.is_some(),
        GpuTotals {
            name,
            utilization: number("Device Utilization %").map(|percent| percent.min(100) as f32),
            memory_used_bytes: used.map(|used| used.min(total)),
            memory_total_bytes: Some(total).filter(|&total| total > 0),
        },
    )
}

fn accelerators() -> Vec<Accelerator> {
    let Some(matching) = (unsafe { IOServiceMatching(c"IOAccelerator".as_ptr()) }) else {
        return Vec::new();
    };
    let mut iterator = 0;
    let status = unsafe {
        IOServiceGetMatchingServices(
            kIOMainPortDefault,
            Some(CFRetained::<CFDictionary>::from(&matching)),
            &mut iterator,
        )
    };
    if status != kIOReturnSuccess || iterator == 0 {
        return Vec::new();
    }
    let iterator = Entry(iterator);
    std::iter::from_fn(|| Some(IOIteratorNext(iterator.0)).filter(|&entry| entry != 0))
        .map(|entry| {
            let entry = Entry(entry);
            Accelerator {
                name: model(&entry),
                vram_bytes: vram_bytes(&entry),
                entry,
            }
        })
        .collect()
}

fn property(entry: &Entry, key: &'static str) -> Option<CFRetained<CFType>> {
    unsafe {
        IORegistryEntryCreateCFProperty(entry.0, Some(&CFString::from_static_str(key)), None, 0)
    }
}

/// The accelerator's property, else its provider's: Apple silicon's accelerator names
/// itself, a PCI GPU's device carries its name and memory.
fn own_or_provider(entry: &Entry, key: &'static str) -> Option<CFRetained<CFType>> {
    property(entry, key).or_else(|| {
        // io_name_t is a fixed 128-byte array, copied whole.
        let mut plane = [0 as c_char; 128];
        for (to, from) in plane.iter_mut().zip(c"IOService".to_bytes()) {
            *to = *from as c_char;
        }
        let mut parent = 0;
        let status = unsafe { IORegistryEntryGetParentEntry(entry.0, &mut plane, &mut parent) };
        (status == kIOReturnSuccess && parent != 0)
            .then(|| property(&Entry(parent), key))
            .flatten()
    })
}

/// "Apple M3 Pro" on Apple silicon; a PCI GPU's name is bytes of text.
fn model(entry: &Entry) -> Option<String> {
    let model = own_or_provider(entry, "model")?;
    let model = match model.downcast::<CFString>() {
        Ok(text) => text.to_string(),
        Err(model) => {
            let bytes = model.downcast::<CFData>().ok()?.to_vec();
            String::from_utf8_lossy(&bytes)
                .trim_end_matches('\0')
                .to_owned()
        }
    };
    let model = model.trim();
    (!model.is_empty()).then(|| model.to_owned())
}

fn vram_bytes(entry: &Entry) -> Option<u64> {
    let megabytes = own_or_provider(entry, "VRAM,totalMB")?
        .downcast::<CFNumber>()
        .ok()?
        .as_i64()?;
    u64::try_from(megabytes)
        .ok()
        .filter(|&megabytes| megabytes > 0)
        .map(|megabytes| megabytes << 20)
}

fn memory_bytes() -> u64 {
    let mut bytes = 0u64;
    let mut size = std::mem::size_of::<u64>();
    let status = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut bytes).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status == 0 { bytes } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statistics(entries: &[(&'static str, i64)]) -> CFRetained<CFDictionary<CFString, CFType>> {
        let keys: Vec<CFRetained<CFString>> = entries
            .iter()
            .map(|(key, _)| CFString::from_static_str(key))
            .collect();
        let values: Vec<CFRetained<CFNumber>> = entries
            .iter()
            .map(|(_, value)| CFNumber::new_i64(*value))
            .collect();
        let keys: Vec<&CFString> = keys.iter().map(|key| &**key).collect();
        let values: Vec<&CFType> = values.iter().map(|value| value.as_ref()).collect();
        CFDictionary::from_slices(&keys, &values)
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn unified_memory_is_what_the_gpu_has_in_use_of_the_system_s() {
        let stats = statistics(&[
            ("Device Utilization %", 27),
            ("Renderer Utilization %", 27),
            ("Alloc system memory", 1_672_347_648),
            ("In use system memory", 1_005_223_936),
        ]);
        let (dedicated, totals) = gpu_totals(Some("Apple M5 Pro".into()), None, &stats, 48 * GIB);
        assert!(!dedicated);
        assert_eq!(
            totals,
            GpuTotals {
                name: Some("Apple M5 Pro".into()),
                utilization: Some(27.0),
                memory_used_bytes: Some(1_005_223_936),
                memory_total_bytes: Some(48 * GIB),
            }
        );
    }

    #[test]
    fn discrete_memory_is_the_gpu_s_own() {
        let stats = statistics(&[
            ("Device Utilization %", 140),
            ("vramUsedBytes", GIB as i64),
            ("vramFreeBytes", 3 * GIB as i64),
            ("In use system memory", 5),
        ]);
        let (dedicated, totals) = gpu_totals(None, None, &stats, 16 * GIB);
        assert!(dedicated);
        assert_eq!(totals.utilization, Some(100.0), "capped at 100");
        assert_eq!(totals.memory_used_bytes, Some(GIB));
        assert_eq!(totals.memory_total_bytes, Some(4 * GIB));

        let stats = statistics(&[("vramFreeBytes", 3 * GIB as i64)]);
        let (_, totals) = gpu_totals(None, Some(8 * GIB), &stats, 16 * GIB);
        assert_eq!(totals.memory_used_bytes, Some(5 * GIB));
        assert_eq!(totals.memory_total_bytes, Some(8 * GIB));
    }

    #[test]
    fn missing_statistics_are_unavailable() {
        let stats = statistics(&[("Device Utilization %", -1)]);
        let (_, totals) = gpu_totals(None, None, &stats, 16 * GIB);
        assert_eq!(totals.utilization, None);
        assert_eq!(totals.memory_used_bytes, None);
        assert_eq!(totals.memory_total_bytes, Some(16 * GIB));
    }
}
