//! Kernel cache and background compiler.
//!
//! Kernels are identified by the hash of their generated code. Tier-0 kernels
//! (one per op, plus utility kernels) are compiled once at startup. Fused
//! kernels are compiled on a background thread when a plan needs them; until
//! one is ready the executor runs that group as a chain of tier-0 kernels,
//! so structural edits never stall a frame on the shader compiler.

use crate::codegen;
use crate::ops::OPS;
use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;
use ultrafast_gpu::{Device, Kernel};
use ultrafast_shaderc::{Compiler, Target};

#[derive(Clone)]
pub enum KernelState {
    Pending,
    Ready(Kernel),
    Failed(String),
}

/// Statistics a caller can show to explain compile behaviour.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompileStats {
    pub jobs: u64,
    pub kernels: u64,
    pub cache_hits: u64,
    pub slang_ms: f64,
    pub native_ms: f64,
}

struct Shared {
    map: Mutex<HashMap<String, KernelState>>,
    cv: Condvar,
    stats: Mutex<CompileStats>,
}

struct Job {
    kernels: Vec<(String, String)>,
}

/// Tier-0 kernel for one op: its key and the order its texture bindings map
/// to the op's ports (`usize::MAX` = the resource texture).
#[derive(Debug, Clone)]
pub struct Tier0 {
    pub key: String,
    pub bindings: Vec<usize>,
}

pub struct Kernels {
    shared: Arc<Shared>,
    /// Closed on drop, which ends the compile threads.
    tx: Option<Sender<Job>>,
    tier0: HashMap<&'static str, Tier0>,
    /// Tier-0 kernels' code by key, for compiling one when it is first
    /// needed ([`Kernels::on_demand`]).
    tier0_code: HashMap<String, String>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Kernels {
    /// Ends the compile threads and waits for them: each holds the device,
    /// which must be let go here, not on a thread still running while the
    /// process exits (the GPU driver tears down under it).
    fn drop(&mut self) {
        self.tx.take();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

/// Utility kernels from the op library, compiled with tier 0.
pub const UTILITY: &[&str] = &[
    "k_blur",
    "k_prep_srgb8",
    "k_prep_video",
    "k_encode_srgb8",
    "k_encode_nv12",
    "k_clear",
    "k_raster_lines",
    "k_raster_resolve",
    "k_atlas_put",
    "k_present",
    "k_accumulate",
    "k_blur_region",
    "k_resample",
    "k_diff",
    "k_dof",
    "k_dof_mix",
    "k_fl_curl",
    "k_fl_force",
    "k_fl_div",
    "k_fl_jacobi",
    "k_fl_project",
    "k_fl_advect",
    "k_copy",
    "k_over_frames",
];

impl Kernels {
    /// Starts the compile threads and builds tier 0 (blocking): every op's
    /// kernel, so a new structure renders op by op at once (the live
    /// player, the render service).
    pub fn new(device: Device) -> Result<Self, String> {
        Self::build(device, true)
    }

    /// Starts the compile threads and builds the utility kernels only;
    /// an op's tier-0 kernel compiles when something first needs it
    /// ([`Kernels::tier0_kernel`]). For one render that waits for its
    /// fused kernels anyway (an export, a frame): the first run after an
    /// install or an upgrade doesn't compile ~30 kernels it never uses
    /// (about 0.9 s on 8 threads).
    pub fn on_demand(device: Device) -> Result<Self, String> {
        Self::build(device, false)
    }

    fn build(device: Device, eager: bool) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            map: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            stats: Mutex::new(CompileStats::default()),
        });
        let (tx, rx) = channel::<Job>();
        let target = match device.backend() {
            "metal" => Target::Metal,
            _ => Target::Spirv,
        };
        // Every kernel is its own module, so its compiled code (and the
        // driver's pipeline for it) is cached under a key of its own source
        // alone: the same kernel hits the cache in any process, whatever
        // else that process compiled before. Modules compile on a pool of
        // threads (ULTRAFAST_COMPILE_THREADS).
        let rx = Arc::new(Mutex::new(rx));
        let threads = std::env::var("ULTRAFAST_COMPILE_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()).min(8))
            .max(1);
        let mut workers = Vec::with_capacity(threads);
        for i in 0..threads {
            let rx = rx.clone();
            let worker_shared = shared.clone();
            let worker_device = device.clone();
            let handle = std::thread::Builder::new()
                .name(format!("ultrafast-compile-{i}"))
                .spawn(move || {
                    let mut compiler = Compiler::new();
                    loop {
                        let job = match rx.lock().unwrap().recv() {
                            Ok(job) => job,
                            Err(_) => break,
                        };
                        run_job(&mut compiler, &worker_device, target, &worker_shared, job);
                    }
                })
                .map_err(|e| e.to_string())?;
            workers.push(handle);
        }

        let mut tier0 = HashMap::new();
        let mut jobs = Vec::new();
        for def in OPS {
            if let Some(g) = codegen::single_op(def) {
                let resource_ext = def.ports.len();
                let bindings = g
                    .bindings
                    .iter()
                    .map(|&b| if b == resource_ext { usize::MAX } else { b })
                    .collect();
                tier0.insert(
                    def.func,
                    Tier0 {
                        key: g.key.clone(),
                        bindings,
                    },
                );
                jobs.push((g.key, g.body));
            }
        }
        let mut k = Kernels {
            shared,
            tx: Some(tx),
            tier0,
            tier0_code: HashMap::new(),
            workers,
        };
        // Utility kernels are plain entry points in the library itself:
        // first in the queue, as every render needs some.
        k.request_utility();
        if eager {
            k.request(jobs);
            for t in k.tier0.values() {
                if let KernelState::Failed(e) = k.wait(&t.key) {
                    return Err(format!("tier-0 kernel failed to build: {e}"));
                }
            }
        } else {
            k.tier0_code = jobs.into_iter().collect();
        }
        for u in UTILITY {
            if let KernelState::Failed(e) = k.wait(u) {
                return Err(format!("utility kernel {u} failed to build: {e}"));
            }
        }
        Ok(k)
    }

    fn request_utility(&self) {
        let mut map = self.shared.map.lock().unwrap();
        for u in UTILITY {
            map.insert(u.to_string(), KernelState::Pending);
        }
        drop(map);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job {
                kernels: vec![(String::new(), String::new())],
            });
        }
    }

    pub fn tier0(&self, func: &str) -> Option<&Tier0> {
        self.tier0.get(func)
    }

    /// The tier-0 kernel for op function `func`, compiled now if it hasn't
    /// been (blocks until it is).
    pub fn tier0_kernel(&self, func: &str) -> Result<Kernel, String> {
        let t = self
            .tier0
            .get(func)
            .ok_or_else(|| format!("no tier-0 kernel for {func}"))?;
        if self.get(&t.key).is_none() {
            let code = self.tier0_code.get(&t.key).cloned().unwrap_or_default();
            self.request(vec![(t.key.clone(), code)]);
        }
        match self.wait(&t.key) {
            KernelState::Ready(k) => Ok(k),
            KernelState::Failed(e) => Err(format!("tier-0 kernel for {func} failed to build: {e}")),
            KernelState::Pending => unreachable!("wait returns once a kernel is done"),
        }
    }

    /// Queues kernels that are not known yet. Returns how many were queued.
    pub fn request(&self, kernels: Vec<(String, String)>) -> usize {
        let mut map = self.shared.map.lock().unwrap();
        let mut todo = Vec::new();
        for (key, body) in kernels {
            if !map.contains_key(&key) && !todo.iter().any(|(k, _): &(String, String)| *k == key) {
                map.insert(key.clone(), KernelState::Pending);
                todo.push((key, body));
            }
        }
        drop(map);
        let n = todo.len();
        if let Some(tx) = &self.tx {
            for k in todo {
                let _ = tx.send(Job { kernels: vec![k] });
            }
        }
        n
    }

    pub fn get(&self, key: &str) -> Option<KernelState> {
        self.shared.map.lock().unwrap().get(key).cloned()
    }

    /// Ready kernel for `key`, if any.
    pub fn ready(&self, key: &str) -> Option<Kernel> {
        match self.get(key) {
            Some(KernelState::Ready(k)) => Some(k),
            _ => None,
        }
    }

    /// Blocks until `key` is no longer pending.
    pub fn wait(&self, key: &str) -> KernelState {
        let mut map = self.shared.map.lock().unwrap();
        loop {
            match map.get(key) {
                Some(KernelState::Pending) => map = self.shared.cv.wait(map).unwrap(),
                Some(s) => return s.clone(),
                None => return KernelState::Failed(format!("kernel {key} was never requested")),
            }
        }
    }

    pub fn utility(&self, name: &str) -> Kernel {
        self.ready(name)
            .unwrap_or_else(|| panic!("utility kernel {name} missing"))
    }

    pub fn pending(&self) -> usize {
        self.shared
            .map
            .lock()
            .unwrap()
            .values()
            .filter(|s| matches!(s, KernelState::Pending))
            .count()
    }

    pub fn stats(&self) -> CompileStats {
        *self.shared.stats.lock().unwrap()
    }
}

fn run_job(compiler: &mut Compiler, device: &Device, target: Target, shared: &Shared, job: Job) {
    let utility = job.kernels.len() == 1 && job.kernels[0].0.is_empty();
    let pairs: Vec<(&str, &str)> = if utility {
        Vec::new()
    } else {
        job.kernels.iter().map(|(k, b)| (k.as_str(), b.as_str())).collect()
    };
    let source = codegen::module(&pairs);
    // ULTRAFAST_DUMP_KERNELS=dir writes every compiled module for inspection.
    if let Some(dir) = std::env::var_os("ULTRAFAST_DUMP_KERNELS") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let name = blake3::hash(source.as_bytes()).to_hex()[..16].to_string();
        let _ = std::fs::write(dir.join(format!("{name}.slang")), &source);
    }
    let names: Vec<String> = if utility {
        UTILITY.iter().map(|s| s.to_string()).collect()
    } else {
        job.kernels.iter().map(|(k, _)| codegen::entry_name(k)).collect()
    };
    let keys: Vec<String> = if utility {
        UTILITY.iter().map(|s| s.to_string()).collect()
    } else {
        job.kernels.iter().map(|(k, _)| k.clone()).collect()
    };
    let entries: Vec<&str> = names.iter().map(String::as_str).collect();

    let t0 = Instant::now();
    let result = compiler
        .compile(&source, &entries, target)
        .map_err(|e| e.to_string())
        .and_then(|out| {
            let slang_ms = out.ms;
            let t1 = Instant::now();
            let lib = device.library(&out.code).map_err(|e| e.to_string())?;
            let native_ms = t1.elapsed().as_secs_f64() * 1000.0;
            Ok((lib, out.cached, slang_ms, native_ms))
        });

    match result {
        Ok((lib, cached, slang_ms, native_ms)) => {
            let built: Vec<KernelState> = names
                .iter()
                .map(|n| match lib.kernel(n) {
                    Ok(k) => KernelState::Ready(k),
                    Err(e) => KernelState::Failed(e.to_string()),
                })
                .collect();
            let mut st = shared.stats.lock().unwrap();
            st.jobs += 1;
            st.kernels += names.len() as u64;
            st.cache_hits += cached as u64;
            st.slang_ms += slang_ms;
            st.native_ms += native_ms;
            drop(st);
            let mut map = shared.map.lock().unwrap();
            for (k, s) in keys.into_iter().zip(built) {
                map.insert(k, s);
            }
        }
        Err(e) if keys.len() > 1 && !utility => {
            // One bad kernel (say, an agent shader with a typo) must not take
            // the others down: retry individually to isolate it.
            let _ = t0;
            for (k, b) in job.kernels {
                run_job(compiler, device, target, shared, Job { kernels: vec![(k, b)] });
            }
            let _ = e;
            shared.cv.notify_all();
            return;
        }
        Err(e) => {
            let mut map = shared.map.lock().unwrap();
            for k in keys {
                map.insert(k, KernelState::Failed(e.clone()));
            }
        }
    }
    shared.cv.notify_all();
}
