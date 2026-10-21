//! Each live process's recent CPU and memory, for the inspector's graphs: the last
//! [`HISTORY_SAMPLES`] samples (5 minutes at one every 2 s) of every process the source
//! sampled, kept by the window from the samples it already receives. A process's ring
//! goes when a sample no longer lists it, so nothing is kept for processes gone.

use super::{ProcessKey, key};
use compi_protocol::system_metrics::Process;
use std::collections::{HashMap, VecDeque};

/// Samples kept per process.
pub const HISTORY_SAMPLES: usize = 150;

/// One process's samples, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessRing {
    /// Percent of one logical CPU.
    cpu: VecDeque<Option<f32>>,
    memory: VecDeque<Option<u64>>,
}

impl ProcessRing {
    fn push(&mut self, process: &Process) {
        if self.cpu.len() == HISTORY_SAMPLES {
            self.cpu.pop_front();
            self.memory.pop_front();
        }
        self.cpu.push_back(process.sample.cpu_percent);
        self.memory.push_back(process.sample.memory_bytes);
    }

    pub fn len(&self) -> usize {
        self.cpu.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cpu.is_empty()
    }

    pub fn cpu(&self) -> impl Iterator<Item = Option<f32>> + '_ {
        self.cpu.iter().copied()
    }

    pub fn memory(&self) -> impl Iterator<Item = Option<u64>> + '_ {
        self.memory.iter().copied()
    }
}

/// Every sampled process's ring.
#[derive(Debug, Default)]
pub struct ProcessHistory {
    rings: HashMap<ProcessKey, ProcessRing>,
}

impl ProcessHistory {
    /// Adds a sample of `processes`: each one's values join its ring, and the rings of
    /// processes it no longer lists go.
    pub fn record(&mut self, processes: &[Process]) {
        let mut rings = HashMap::with_capacity(processes.len());
        for process in processes {
            let key = key(process);
            let mut ring = self.rings.remove(&key).unwrap_or_default();
            ring.push(process);
            rings.insert(key, ring);
        }
        self.rings = rings;
    }

    pub fn ring(&self, key: &ProcessKey) -> Option<&ProcessRing> {
        self.rings.get(key)
    }

    pub fn clear(&mut self) {
        self.rings.clear();
    }

    pub fn len(&self) -> usize {
        self.rings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::{ProcessDetails, ProcessOwner, ProcessSample};
    use std::sync::Arc;

    fn process(pid: u32, start_time: u64, cpu: f32) -> Process {
        Process {
            sample: ProcessSample {
                pid,
                start_time,
                parent_pid: None,
                details: None,
                cpu_percent: Some(cpu),
                memory_bytes: None,
                threads: None,
                cpu_time_ms: None,
            },
            details: Arc::new(ProcessDetails {
                name: "p".into(),
                command: None,
                executable: None,
                user: None,
                owner: ProcessOwner::CurrentUser,
                app: false,
                restricted: false,
                app_group: None,
                surface: None,
            }),
        }
    }

    #[test]
    fn rings_keep_the_last_samples_and_go_with_their_process() {
        let mut history = ProcessHistory::default();
        for sample in 0..HISTORY_SAMPLES + 5 {
            history.record(&[process(1, 10, sample as f32), process(2, 20, 0.0)]);
        }
        let ring = history.ring(&(1, 10)).unwrap();
        assert_eq!(ring.len(), HISTORY_SAMPLES);
        assert_eq!(ring.cpu().next(), Some(Some(5.0)));
        assert_eq!(ring.memory().last(), Some(None));
        // Pid 2 left, and pid 1 now names another process: neither keeps a ring.
        history.record(&[process(1, 99, 1.0)]);
        assert_eq!(history.len(), 1);
        assert!(history.ring(&(2, 20)).is_none());
        assert_eq!(history.ring(&(1, 99)).unwrap().len(), 1);
        assert!(history.ring(&(1, 10)).is_none());
    }
}
