//! Mesure de la RAM / CPU consommés par le navigateur et ses sous-processus,
//! avec une répartition par rôle (interface, pages web, GPU, réseau, extensions…).
//!
//! Chromium (CEF) lance des sous-processus `Ferrox.exe` ; leur rôle se lit dans la ligne de
//! commande (`--type=renderer`, `--type=gpu-process`…).

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub struct Monitor {
    sys: Option<System>,
    me: Pid,
    ncpu: f32,
    /// Nous + nos sous-processus (découverts lors du dernier scan complet).
    family: Vec<Pid>,
    samples: u32,
}

/// Mémoire « privée » d'un processus, comme le Gestionnaire des tâches :
/// sans les bibliothèques partagées comptées plusieurs fois.
fn private_mem(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: handle fermé dans tous les cas ; structure de la bonne taille.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut c: PROCESS_MEMORY_COUNTERS_EX2 = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        let ok = GetProcessMemoryInfo(h, &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb) != 0;
        CloseHandle(h);
        (ok && c.PrivateWorkingSetSize > 0).then_some(c.PrivateWorkingSetSize as u64)
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Category {
    pub key: &'static str,
    pub label: &'static str,
    pub ram_mb: u64,
    pub cpu: f32,
    pub procs: usize,
}

#[derive(Default, Clone)]
pub struct Usage {
    pub ram_mb: u64,
    pub cpu_pct: f32,
    pub procs: usize,
    pub sys_total_mb: u64,
    pub sys_used_mb: u64,
    pub categories: Vec<Category>,
    /// Mémoire (octets) et CPU (%) de chaque processus de la famille.
    pub per_pid: HashMap<u32, (u64, f32)>,
}

fn classify(cmd: &str, is_me: bool) -> (&'static str, &'static str) {
    if is_me {
        return ("ui", "Ferrox (interface et moteur)");
    }
    // Sous-processus de Chromium : tous des « Ferrox.exe », rôle donné par `--type=`.
    if cmd.contains("--type=renderer") {
        if cmd.contains("--extension-process") {
            return ("ext", "Extensions");
        }
        return ("pages", "Pages web");
    }
    if cmd.contains("--type=gpu-process") {
        return ("gpu", "Carte graphique");
    }
    if cmd.contains("network.mojom") {
        return ("network", "Réseau");
    }
    ("other", "Services (audio, stockage…)")
}

const ORDER: &[&str] = &["pages", "engine", "ui", "gpu", "network", "ext", "sandbox", "other"];

impl Monitor {
    pub fn new() -> Self {
        let ncpu = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
        Self { sys: System::new().ok(), me: Pid::from_u32(std::process::id()), ncpu, family: Vec::new(), samples: 0 }
    }

    pub fn sample(&mut self) -> Usage {
        let Some(sys) = self.sys.as_mut() else { return Usage::default() };
        let kind = ProcessRefreshKind::nothing()
            .with_memory()
            .with_cpu()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .without_tasks();
        // Scan complet de tous les processus du système seulement toutes les 10 s
        // (pour découvrir les nouveaux sous-processus) ; sinon, uniquement les nôtres.
        let full = self.family.is_empty() || self.samples % 5 == 0;
        self.samples = self.samples.wrapping_add(1);
        if full {
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        } else {
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&self.family), true, kind);
        }
        sys.refresh_memory();
        let procs = sys.processes();
        if full {
            // Famille = nous + tous nos descendants.
            let mut fam: HashSet<Pid> = HashSet::from([self.me]);
            let mut changed = true;
            while changed {
                changed = false;
                for (pid, p) in procs {
                    if !fam.contains(pid) && p.parent().is_some_and(|pp| fam.contains(&pp)) {
                        fam.insert(*pid);
                        changed = true;
                    }
                }
            }
            self.family = fam.into_iter().collect();
        }
        self.family.retain(|p| procs.contains_key(p));
        let family = self.family.clone();
        let mut u = Usage {
            sys_total_mb: sys.total_memory() / (1024 * 1024),
            sys_used_mb: sys.used_memory() / (1024 * 1024),
            ..Default::default()
        };
        let mut cats: HashMap<&'static str, Category> = HashMap::new();
        for pid in &family {
            let Some(p) = procs.get(pid) else { continue };
            if p.thread_kind().is_some() {
                continue;
            }
            let cpu = p.cpu_usage() / self.ncpu;
            let cmd: String = p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            let (key, label) = classify(&cmd, *pid == self.me);
            let mem = private_mem(pid.as_u32()).unwrap_or_else(|| p.memory());
            let c = cats.entry(key).or_insert(Category { key, label, ram_mb: 0, cpu: 0.0, procs: 0 });
            c.ram_mb += mem;
            c.cpu += cpu;
            c.procs += 1;
            u.ram_mb += mem;
            u.cpu_pct += cpu;
            u.procs += 1;
            u.per_pid.insert(pid.as_u32(), (mem, cpu));
        }
        u.ram_mb /= 1024 * 1024;
        let mut v: Vec<Category> = cats.into_values().map(|mut c| { c.ram_mb /= 1024 * 1024; c }).collect();
        v.sort_by_key(|c| ORDER.iter().position(|k| *k == c.key).unwrap_or(99));
        u.categories = v;
        u
    }
}
