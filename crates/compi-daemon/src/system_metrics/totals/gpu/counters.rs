//! Windows' GPU performance counters by instance name. `GPU Engine` instances are one per
//! process and engine, such as `pid_1234_luid_0x00000000_0x0000D1E2_phys_0_eng_0_engtype_3D`;
//! `GPU Adapter Memory` instances one per adapter, such as `luid_0x00000000_0x0000D1E2_phys_0`.

/// An adapter's locally unique id: high part, low part.
pub type Luid = (u32, u32);

/// The adapter an instance belongs to.
pub fn instance_luid(instance: &str) -> Option<Luid> {
    let (_, rest) = instance.split_once("luid_")?;
    let mut parts = rest.splitn(3, '_');
    let mut part = || {
        let hex = parts.next()?;
        let digits = hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X"))?;
        u32::from_str_radix(digits, 16).ok()
    };
    Some((part()?, part()?))
}

/// One adapter's reading, from every counter instance offered to it.
#[derive(Debug, Default)]
pub struct AdapterReading {
    luid: Luid,
    busy: f64,
    engines: bool,
    dedicated: u64,
    memory: bool,
}

impl AdapterReading {
    pub fn new(luid: Luid) -> Self {
        Self {
            luid,
            ..Self::default()
        }
    }

    /// A `GPU Engine` instance's `Utilization Percentage`: the 3D and compute engines'
    /// shares add up; copy, video and the rest are other work alongside.
    pub fn engine(&mut self, instance: &str, percent: f64) {
        if instance_luid(instance) != Some(self.luid) {
            return;
        }
        let Some((_, kind)) = instance.rsplit_once("engtype_") else {
            return;
        };
        self.engines = true;
        if kind == "3D" || kind.starts_with("Compute") {
            self.busy += percent.max(0.0);
        }
    }

    /// A `GPU Adapter Memory` instance's `Dedicated Usage`, which linked adapters report
    /// per physical part.
    pub fn dedicated(&mut self, instance: &str, bytes: i64) {
        if instance_luid(instance) == Some(self.luid) {
            self.memory = true;
            self.dedicated += u64::try_from(bytes).unwrap_or(0);
        }
    }

    pub fn utilization(&self) -> Option<f32> {
        self.engines.then(|| self.busy.min(100.0) as f32)
    }

    pub fn dedicated_bytes(&self) -> Option<u64> {
        self.memory.then_some(self.dedicated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADAPTER: Luid = (0, 0xD1E2);

    #[test]
    fn instances_name_their_adapter() {
        assert_eq!(
            instance_luid("pid_1234_luid_0x00000000_0x0000D1E2_phys_0_eng_0_engtype_3D"),
            Some(ADAPTER)
        );
        assert_eq!(
            instance_luid("luid_0x00000001_0x0000c5a3_phys_0"),
            Some((1, 0xC5A3))
        );
        assert_eq!(instance_luid("pid_1234_eng_0"), None);
        assert_eq!(instance_luid("luid_0x00000000"), None);
    }

    #[test]
    fn three_d_and_compute_engines_add_up_to_at_most_all_of_the_gpu() {
        let mut reading = AdapterReading::new(ADAPTER);
        assert_eq!(reading.utilization(), None, "no engines read");
        let engine = |pid: u32, kind: &str| {
            format!("pid_{pid}_luid_0x00000000_0x0000D1E2_phys_0_eng_0_engtype_{kind}")
        };
        reading.engine(&engine(1, "3D"), 20.0);
        reading.engine(&engine(2, "3D"), 15.0);
        reading.engine(&engine(2, "Compute_0"), 10.0);
        reading.engine(&engine(3, "VideoDecode"), 50.0);
        reading.engine(&engine(3, "Copy"), 30.0);
        reading.engine(
            "pid_1_luid_0x00000000_0x0000AAAA_phys_0_eng_0_engtype_3D",
            90.0,
        );
        assert_eq!(reading.utilization(), Some(45.0));
        reading.engine(&engine(4, "3D"), 80.0);
        assert_eq!(reading.utilization(), Some(100.0));
    }

    #[test]
    fn dedicated_memory_adds_up_across_an_adapter_s_parts() {
        let mut reading = AdapterReading::new(ADAPTER);
        assert_eq!(reading.dedicated_bytes(), None);
        reading.dedicated("luid_0x00000000_0x0000D1E2_phys_0", 1 << 30);
        reading.dedicated("luid_0x00000000_0x0000D1E2_phys_1", 1 << 29);
        reading.dedicated("luid_0x00000000_0x0000AAAA_phys_0", 1 << 32);
        assert_eq!(reading.dedicated_bytes(), Some(3 << 29));
    }
}
