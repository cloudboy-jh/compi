//! Windows' GPU performance counters, which Task Manager reads too, collected with PDH on
//! a thread of their own: `GPU Engine` has an instance per process and engine, and
//! collecting it can take tens of milliseconds on a busy host, which the totals sampler
//! does not wait for. The adapter's name and memory come from DXGI.

use super::counters::{AdapterReading, Luid};
use compi_protocol::system_metrics::{GpuTotals, TOTALS_INTERVAL_MS};
use parking_lot::Mutex;
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::{PCWSTR, w};

/// Reads between two lookups of the adapters, so one attached later appears within a
/// minute.
const LOOKUP_EVERY: u32 = 30;

/// Keeps values above 100, so the engines' sum is capped once, after adding up.
const PDH_FMT_NOCAP100: u32 = 0x8000;

const ERROR_SUCCESS: u32 = 0;

pub struct GpuReader {
    latest: Arc<Mutex<Option<GpuTotals>>>,
}

impl GpuReader {
    pub fn new() -> Self {
        let latest = Arc::new(Mutex::new(None));
        let shared = Arc::downgrade(&latest);
        // Without the thread the GPU stays unavailable; the other totals go on.
        let _ = thread::Builder::new()
            .name("compi-gpu-counters".into())
            .spawn(move || sample(shared));
        Self { latest }
    }

    /// The counters' latest reading, at most an interval old.
    pub fn read(&mut self) -> Option<GpuTotals> {
        self.latest.lock().clone()
    }
}

/// Reads every interval until the reader is dropped.
fn sample(latest: Weak<Mutex<Option<GpuTotals>>>) {
    let mut counters = Counters::open();
    let mut adapter = main_adapter();
    let mut reads = 0u32;
    loop {
        reads += 1;
        if reads.is_multiple_of(LOOKUP_EVERY) {
            adapter = main_adapter();
        }
        let totals = adapter.as_ref().map(|adapter| {
            let reading = counters
                .as_mut()
                .and_then(|counters| counters.read(adapter.luid));
            GpuTotals {
                name: Some(adapter.name.clone()).filter(|name| !name.is_empty()),
                utilization: reading.as_ref().and_then(AdapterReading::utilization),
                memory_used_bytes: reading.as_ref().and_then(AdapterReading::dedicated_bytes),
                memory_total_bytes: Some(adapter.dedicated_bytes).filter(|&bytes| bytes > 0),
            }
        });
        let Some(latest) = latest.upgrade() else {
            return;
        };
        *latest.lock() = totals;
        drop(latest);
        thread::sleep(Duration::from_millis(TOTALS_INTERVAL_MS));
    }
}

struct Adapter {
    name: String,
    luid: Luid,
    dedicated_bytes: u64,
}

/// The hardware adapter with the most memory of its own.
fn main_adapter() -> Option<Adapter> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    (0..)
        .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
        .filter_map(|adapter| unsafe { adapter.GetDesc1() }.ok())
        .filter(|desc| !DXGI_ADAPTER_FLAG(desc.Flags as i32).contains(DXGI_ADAPTER_FLAG_SOFTWARE))
        .max_by_key(|desc| desc.DedicatedVideoMemory)
        .map(|desc| {
            let description = &desc.Description;
            let end = description
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(description.len());
            Adapter {
                name: String::from_utf16_lossy(&description[..end])
                    .trim()
                    .to_owned(),
                luid: (desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart),
                dedicated_bytes: desc.DedicatedVideoMemory as u64,
            }
        })
}

/// A PDH query with the engine and memory counters for every adapter.
struct Counters {
    query: PDH_HQUERY,
    engines: Option<PDH_HCOUNTER>,
    memory: Option<PDH_HCOUNTER>,
    /// Reused between reads: the items, then the instance names they point at.
    items: Vec<PDH_FMT_COUNTERVALUE_ITEM_W>,
    name: String,
}

impl Drop for Counters {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

impl Counters {
    fn open() -> Option<Self> {
        let mut query = PDH_HQUERY(std::ptr::null_mut());
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != ERROR_SUCCESS {
            return None;
        }
        let add = |path: PCWSTR| {
            let mut counter = PDH_HCOUNTER(std::ptr::null_mut());
            (unsafe { PdhAddEnglishCounterW(query, path, 0, &mut counter) } == ERROR_SUCCESS)
                .then_some(counter)
        };
        let counters = Self {
            query,
            engines: add(w!("\\GPU Engine(*)\\Utilization Percentage")),
            memory: add(w!("\\GPU Adapter Memory(*)\\Dedicated Usage")),
            items: Vec::new(),
            name: String::new(),
        };
        if counters.engines.is_none() && counters.memory.is_none() {
            return None;
        }
        // Utilization is a rate: the first collection is what the next is compared with.
        unsafe { PdhCollectQueryData(query) };
        Some(counters)
    }

    fn read(&mut self, luid: Luid) -> Option<AdapterReading> {
        if unsafe { PdhCollectQueryData(self.query) } != ERROR_SUCCESS {
            return None;
        }
        let mut reading = AdapterReading::new(luid);
        if let Some(engines) = self.engines {
            self.each(
                engines,
                PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100),
                |instance, value| reading.engine(instance, unsafe { value.Anonymous.doubleValue }),
            );
        }
        if let Some(memory) = self.memory {
            self.each(memory, PDH_FMT_LARGE, |instance, value| {
                reading.dedicated(instance, unsafe { value.Anonymous.largeValue })
            });
        }
        Some(reading)
    }

    /// Calls `visit` with each instance of `counter` that has a value.
    fn each(
        &mut self,
        counter: PDH_HCOUNTER,
        format: PDH_FMT,
        mut visit: impl FnMut(&str, &PDH_FMT_COUNTERVALUE),
    ) {
        let item_size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let count = loop {
            let mut size = (self.items.capacity() * item_size) as u32;
            let mut count = 0;
            let buffer = (size > 0).then_some(self.items.as_mut_ptr());
            let status = unsafe {
                PdhGetFormattedCounterArrayW(counter, format, &mut size, &mut count, buffer)
            };
            match status {
                ERROR_SUCCESS => break count as usize,
                // The size covers the names after the items; whole items keep alignment.
                PDH_MORE_DATA => self
                    .items
                    .reserve_exact((size as usize).div_ceil(item_size)),
                _ => return,
            }
        };
        let Self { items, name, .. } = self;
        // SAFETY: PDH wrote `count` items at the start of the buffer.
        let items = unsafe { std::slice::from_raw_parts(items.as_ptr(), count) };
        for item in items {
            let status = item.FmtValue.CStatus;
            if status != PDH_CSTATUS_VALID_DATA && status != PDH_CSTATUS_NEW_DATA {
                continue;
            }
            name.clear();
            // SAFETY: the name is a NUL-terminated string in the same buffer.
            let wide = unsafe { item.szName.as_wide() };
            name.extend(
                char::decode_utf16(wide.iter().copied())
                    .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)),
            );
            visit(name, &item.FmtValue);
        }
    }
}
