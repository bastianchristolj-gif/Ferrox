//! Mesure de la RAM / CPU consommés par le navigateur et ses sous-processus,
//! avec une répartition par rôle (interface, pages web, GPU, réseau, extensions…).
//!
//! - Windows : WebView2 lance des `msedgewebview2.exe` ; leur rôle se lit dans la
//!   ligne de commande (`--type=renderer`, `--type=gpu-process`…).
//! - Linux : WebKitGTK lance `WebKitWebProcess`, `WebKitNetworkProcess`…

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub struct Monitor {
    sys: Option<System>,
    me: Pid,
    ncpu: f32,
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

fn classify(name: &str, cmd: &str, is_me: bool) -> (&'static str, &'static str) {
    let n = name.to_ascii_lowercase();
    if is_me {
        return ("ui", "Interface Ferrox");
    }
    if n.contains("webkitwebprocess") {
        return ("pages", "Pages web");
    }
    if n.contains("webkitnetworkprocess") {
        return ("network", "Réseau");
    }
    if n.contains("webkitgpuprocess") {
        return ("gpu", "Carte graphique");
    }
    if n.contains("bwrap") || n.contains("dbus-proxy") {
        return ("sandbox", "Bac à sable");
    }
    if n.contains("msedgewebview2") {
        if cmd.contains("--extension-process") {
            return ("ext", "Extensions");
        }
        if cmd.contains("--type=renderer") {
            return ("pages", "Pages web");
        }
        if cmd.contains("--type=gpu-process") {
            return ("gpu", "Carte graphique");
        }
        if cmd.contains("network.mojom") {
            return ("network", "Réseau");
        }
        if cmd.contains("--type=") {
            return ("other", "Services (audio, stockage…)");
        }
        return ("engine", "Moteur WebView2");
    }
    ("other", "Services (audio, stockage…)")
}

const ORDER: &[&str] = &["pages", "engine", "ui", "gpu", "network", "ext", "sandbox", "other"];

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
            ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .without_tasks(),
        );
        sys.refresh_memory();
        // Famille = nous + tous nos descendants.
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
            let (key, label) = classify(&p.name().to_string_lossy(), &cmd, *pid == self.me);
            let c = cats.entry(key).or_insert(Category { key, label, ram_mb: 0, cpu: 0.0, procs: 0 });
            c.ram_mb += p.memory();
            c.cpu += cpu;
            c.procs += 1;
            u.ram_mb += p.memory();
            u.cpu_pct += cpu;
            u.procs += 1;
            u.per_pid.insert(pid.as_u32(), (p.memory(), cpu));
        }
        u.ram_mb /= 1024 * 1024;
        let mut v: Vec<Category> = cats.into_values().map(|mut c| { c.ram_mb /= 1024 * 1024; c }).collect();
        v.sort_by_key(|c| ORDER.iter().position(|k| *k == c.key).unwrap_or(99));
        u.categories = v;
        u
    }
}
