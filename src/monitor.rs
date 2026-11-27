//! Mesure de la RAM / CPU consommés par le navigateur et ses sous-processus
//! (sur Linux, WebKit lance des processus WebKitWebProcess / NetworkProcess).

use std::collections::HashSet;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

pub struct Monitor {
    sys: Option<System>,
    me: Pid,
    ncpu: f32,
}

#[derive(Default, Clone, Copy)]
pub struct Usage {
    pub ram_mb: u64,
    pub cpu_pct: f32,
    pub procs: usize,
}

impl Monitor {
    pub fn new() -> Self {
        let ncpu = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
        Self { sys: System::new().ok(), me: Pid::from_u32(std::process::id()), ncpu }
    }

    pub fn sample(&mut self) -> Usage {
        let Some(sys) = self.sys.as_mut() else { return Usage::default() };
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu().without_tasks(),
        );
        // Ensemble : nous + tous nos descendants.
        let procs = sys.processes();
        let mut family: HashSet<Pid> = HashSet::from([self.me]);
        let mut changed = true;
        while changed {
            changed = false;
            for (pid, p) in procs {
                if !family.contains(pid) && p.parent().is_some_and(|pp| family.contains(&pp)) {
                    family.insert(*pid);
                    changed = true;
                }
            }
        }
        let (mut mem, mut cpu) = (0u64, 0f32);
        for pid in &family {
            if let Some(p) = procs.get(pid) {
                if p.thread_kind().is_some() {
                    continue;
                }
                mem += p.memory();
                cpu += p.cpu_usage();
            }
        }
        Usage { ram_mb: mem / (1024 * 1024), cpu_pct: cpu / self.ncpu, procs: family.len() }
    }
}
