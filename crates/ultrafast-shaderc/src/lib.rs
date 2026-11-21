//! Runtime Slang compiler.
//!
//! ultrafast generates Slang source at runtime (fused kernels, agent-written
//! shaders) and compiles it to the native shading language of the active
//! backend: MSL for Metal, SPIR-V for Vulkan. libslang is loaded dynamically
//! through its stable C API, so the engine builds without it and only needs it
//! when a kernel is not already in the on-disk cache.
//!
//! A [`Compiler`] is not thread-safe; the renderer owns one on a dedicated
//! compile thread.

use libloading::{Library, Symbol};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Shader stage of an entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Compute,
    Vertex,
    Fragment,
}

impl Stage {
    fn slang_id(self) -> u32 {
        match self {
            Stage::Vertex => 1,   // SLANG_STAGE_VERTEX
            Stage::Fragment => 5, // SLANG_STAGE_FRAGMENT
            Stage::Compute => SLANG_STAGE_COMPUTE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Metal,
    Spirv,
}

impl Target {
    fn slang_id(self) -> c_int {
        match self {
            Target::Metal => 24, // SLANG_METAL
            Target::Spirv => 6,  // SLANG_SPIRV
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Target::Metal => "metal",
            Target::Spirv => "spv",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("libslang not found: {0}")]
    NotFound(String),
    #[error("libslang load failed: {0}")]
    Load(String),
    #[error("slang compile failed:\n{0}")]
    Compile(String),
}

/// Output of a compile: target code plus where it came from.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub code: Vec<u8>,
    pub cached: bool,
    pub ms: f64,
}

impl Compiled {
    pub fn as_text(&self) -> &str {
        std::str::from_utf8(&self.code).unwrap_or("")
    }
}

type SpCreateSession = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type SpCreateCompileRequest = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type SpDestroyCompileRequest = unsafe extern "C" fn(*mut c_void);
type SpSetCodeGenTarget = unsafe extern "C" fn(*mut c_void, c_int);
type SpSetTargetFlags = unsafe extern "C" fn(*mut c_void, c_int, u32);
type SpSetOptimizationLevel = unsafe extern "C" fn(*mut c_void, c_int);
type SpAddTranslationUnit = unsafe extern "C" fn(*mut c_void, c_int, *const c_char) -> c_int;
type SpAddSource = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, *const c_char);
type SpAddEntryPoint = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, u32) -> c_int;
type SpCompile = unsafe extern "C" fn(*mut c_void) -> i32;
type SpGetDiagnostics = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type SpGetCode = unsafe extern "C" fn(*mut c_void, *mut usize) -> *const c_void;

const SLANG_SOURCE_LANGUAGE_SLANG: c_int = 1;
const SLANG_STAGE_COMPUTE: u32 = 6;
const SLANG_TARGET_FLAG_GENERATE_WHOLE_PROGRAM: u32 = 1 << 8;
const SLANG_TARGET_FLAG_GENERATE_SPIRV_DIRECTLY: u32 = 1 << 10;
const SLANG_OPTIMIZATION_LEVEL_HIGH: c_int = 2;

/// Lazily-loaded Slang compiler with a content-addressed disk cache, and
/// optionally a second one shared between machines.
pub struct Compiler {
    slang: Option<Slang>,
    slang_dir: PathBuf,
    /// `slang_dir` is the cache folder a missing Slang is fetched into.
    fetch: bool,
    cache_dir: Option<PathBuf>,
    /// `$ULTRAFAST_SHARED_CACHE_DIR/kernels`: a cache other machines fill (a
    /// bucket mount), read on a local miss and written after a compile.
    shared_dir: Option<PathBuf>,
    version_tag: String,
}

/// Writes `data` to `path` whole (write, then rename), so a concurrent
/// reader never sees a torn file.
fn write_whole(path: &Path, data: &[u8]) {
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

struct Slang {
    _lib: Library,
    session: *mut c_void,
    create_request: SpCreateCompileRequest,
    destroy_request: SpDestroyCompileRequest,
    set_target: SpSetCodeGenTarget,
    set_target_flags: SpSetTargetFlags,
    set_opt: SpSetOptimizationLevel,
    add_tu: SpAddTranslationUnit,
    add_source: SpAddSource,
    add_entry: SpAddEntryPoint,
    compile: SpCompile,
    diagnostics: SpGetDiagnostics,
    get_code: SpGetCode,
}

/// The Slang release ultrafast is built and tested with. When no other copy
/// is found, this one is fetched once into the cache (see [`find_slang`]).
pub const SLANG_VERSION: &str = "2026.19";

/// The official Slang release archive for this platform and its sha256
/// (GitHub's asset digest), or `None` where Slang publishes no build.
fn slang_release() -> Option<(&'static str, &'static str)> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    Some(match (os, arch) {
        // glibc 2.28 builds: they load on RHEL 8, Ubuntu 18.10 and newer.
        ("linux", "x86_64") => (
            "linux-x86_64-glibc-2.28",
            "bd7afcb0956dee9304f44ed115e9aaa7140704271b5c3294bcb237e3ae67adc7",
        ),
        ("linux", "aarch64") => (
            "linux-aarch64-glibc-2.28",
            "f49229eb9606b47b122e1d8789be46a7bcd6bb2ffa94b06fae6d544a4e6df637",
        ),
        ("macos", "aarch64") => (
            "macos-aarch64",
            "fa46822c92d81404a7951de3746093985c1e6f520f4b533e2aa3bea576f0f2f0",
        ),
        ("macos", "x86_64") => (
            "macos-x86_64",
            "490b6447e3ea6c8fa9dfc45b1dc227db7cf80a60981c4390ed0fdc8ddbedee11",
        ),
        ("windows", "x86_64") => (
            "windows-x86_64",
            "fa7629c9ce59c87c821614ae4b52e52f785d4c659a5a06a89fa9ec86d615eeee",
        ),
        ("windows", "aarch64") => (
            "windows-aarch64",
            "e47c327262a56e99a9ae07c31c3bab5ea5a531ffd3b75b5d5746b24b63a6e751",
        ),
        _ => return None,
    })
}

/// ultrafast's cache folder: `ULTRAFAST_CACHE_DIR`, else the platform's
/// (`~/Library/Caches/ultrafast`, `$XDG_CACHE_HOME/ultrafast` or
/// `~/.cache/ultrafast`, `%LOCALAPPDATA%\ultrafast`), as for fonts and
/// pipelines.
pub fn cache_root() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("ULTRAFAST_CACHE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("ultrafast"));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches/ultrafast")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join("ultrafast"))
            .unwrap_or_else(|| home.join(".cache/ultrafast"))
    })
}

fn lib_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libslang.dylib"
    } else if cfg!(windows) {
        "slang.dll"
    } else {
        "libslang.so"
    }
}

/// The libslang inside a Slang folder (`lib/`, or `bin/` as Windows
/// releases lay it out, or the folder itself).
fn lib_in(dir: &Path) -> Option<PathBuf> {
    let name = lib_name();
    [dir.join("lib").join(name), dir.join("bin").join(name), dir.join(name)]
        .into_iter()
        .find(|p| p.exists())
}

/// Slang shipped with the executable: a `slang-<version>` folder beside it
/// (the release archives and tools/windows/build.sh lay it out so), or in
/// `../libexec` or `../lib/ultrafast` (package managers). Symlinks to the
/// executable (Homebrew's bin/) are followed first.
fn slang_beside_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let dir = exe.parent()?.to_path_buf();
    let homes = [dir.clone(), dir.join("../libexec"), dir.join("../lib/ultrafast")];
    for home in homes {
        let Ok(entries) = std::fs::read_dir(&home) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("slang")))
            .filter(|p| lib_in(p).is_some())
            .collect();
        found.sort();
        if let Some(p) = found.pop() {
            return Some(p);
        }
    }
    None
}

/// Where libslang comes from, first found: `ULTRAFAST_SLANG_DIR`; a copy
/// shipped beside the executable; the folder `SLANG_DIR` named at build
/// time; a copy fetched into the cache before. With none of those, the
/// cache folder it will be fetched into on first use (the second value is
/// true).
pub fn find_slang() -> (PathBuf, bool) {
    if let Some(d) = std::env::var_os("ULTRAFAST_SLANG_DIR").filter(|d| !d.is_empty()) {
        return (PathBuf::from(d), false);
    }
    if let Some(d) = slang_beside_exe() {
        return (d, false);
    }
    let built_in = PathBuf::from(env!("ULTRAFAST_DEFAULT_SLANG_DIR"));
    if !built_in.as_os_str().is_empty() && lib_in(&built_in).is_some() {
        return (built_in, false);
    }
    match cache_root() {
        Some(root) => (root.join(format!("slang-{SLANG_VERSION}")), true),
        None => (PathBuf::from(format!("slang-{SLANG_VERSION}")), false),
    }
}

/// Fetches the pinned Slang release into `dir` (once per machine: the
/// archive is checked against its sha256, unpacked beside `dir` and renamed
/// into place, so concurrent fetches never see half a folder). Needs `curl`
/// and `tar`, as font fetching does. `ULTRAFAST_SLANG_FETCH=0` turns it off.
fn fetch_slang(dir: &Path) -> Result<(), CompileError> {
    use sha2::Digest;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if lib_in(dir).is_some() {
        return Ok(());
    }
    let not_found = |why: &str| {
        CompileError::NotFound(format!(
            "{} ({why}); install Slang {SLANG_VERSION} from https://github.com/shader-slang/slang/releases \
             and set ULTRAFAST_SLANG_DIR, or use a prebuilt ultrafast release, which bundles it",
            dir.display()
        ))
    };
    if std::env::var("ULTRAFAST_SLANG_FETCH").is_ok_and(|v| v == "0") {
        return Err(not_found("ULTRAFAST_SLANG_FETCH=0"));
    }
    let Some((platform, sha256)) = slang_release() else {
        return Err(not_found("no Slang release for this platform"));
    };
    let name = format!("slang-{SLANG_VERSION}-{platform}.tar.gz");
    let url = format!("https://github.com/shader-slang/slang/releases/download/v{SLANG_VERSION}/{name}");
    let parent = dir.parent().ok_or_else(|| not_found("no parent folder"))?;
    std::fs::create_dir_all(parent).map_err(|e| not_found(&e.to_string()))?;
    let work = parent.join(format!(".slang-fetch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("x")).map_err(|e| not_found(&e.to_string()))?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(work.clone());
    eprintln!(
        "ultrafast: fetching Slang {SLANG_VERSION}, the shader compiler, once (about 30-60 MB) into {}",
        dir.display()
    );
    let archive = work.join(&name);
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "--retry", "3", "--connect-timeout", "20", "-o"])
        .arg(&archive)
        .arg(&url)
        .status()
        .map_err(|e| not_found(&format!("could not run curl: {e}")))?;
    if !status.success() {
        return Err(not_found(&format!("download of {url} failed")));
    }
    let bytes = std::fs::read(&archive).map_err(|e| not_found(&e.to_string()))?;
    let got: String = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != sha256 {
        return Err(not_found(&format!("{name} has sha256 {got}, expected {sha256}")));
    }
    drop(bytes);
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(work.join("x"))
        .status()
        .map_err(|e| not_found(&format!("could not run tar: {e}")))?;
    if !status.success() {
        return Err(not_found(&format!("unpacking {name} failed")));
    }
    // Only the compiler libraries are used: LLVM serves CPU targets
    // ultrafast never asks for, and the rest is docs, headers and tools.
    for f in ["libslang-llvm.so", "libslang-llvm.dylib"] {
        let _ = std::fs::remove_file(work.join("x/lib").join(f));
    }
    let _ = std::fs::remove_file(work.join("x/bin/slang-llvm.dll"));
    for d in ["share", "include", "lib64", "lib/cmake"] {
        let _ = std::fs::remove_dir_all(work.join("x").join(d));
    }
    if lib_in(&work.join("x")).is_none() {
        return Err(not_found(&format!("{name} holds no {}", lib_name())));
    }
    if std::fs::rename(work.join("x"), dir).is_err() && lib_in(dir).is_none() {
        return Err(not_found("could not move the unpacked release into place"));
    }
    eprintln!("ultrafast: Slang {SLANG_VERSION} ready");
    Ok(())
}

impl Compiler {
    /// Creates a compiler. libslang is not loaded until the first cache miss.
    pub fn new() -> Self {
        let (slang_dir, fetch) = find_slang();
        let cache_dir = cache_root().map(|d| d.join("kernels"));
        let version_tag = slang_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let shared_dir = std::env::var_os("ULTRAFAST_SHARED_CACHE_DIR")
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join("kernels"));
        Self {
            slang: None,
            slang_dir,
            fetch,
            cache_dir,
            shared_dir,
            version_tag,
        }
    }

    pub fn without_disk_cache(mut self) -> Self {
        self.cache_dir = None;
        self.shared_dir = None;
        self
    }

    /// Content hash of a compile job; also the disk cache key.
    pub fn key(&self, source: &str, entries: &[&str], target: Target) -> String {
        let mut h = blake3::Hasher::new();
        h.update(self.version_tag.as_bytes());
        h.update(&[target.slang_id() as u8]);
        for e in entries {
            h.update(e.as_bytes());
            h.update(&[0]);
        }
        h.update(source.as_bytes());
        h.finalize().to_hex()[..32].to_string()
    }

    /// Compiles `source` with the given compute entry points into one target
    /// module. Returns cached output when the exact job was compiled before.
    pub fn compile(&mut self, source: &str, entries: &[&str], target: Target) -> Result<Compiled, CompileError> {
        let staged: Vec<(&str, Stage)> = entries.iter().map(|e| (*e, Stage::Compute)).collect();
        self.compile_stages(source, &staged, target)
    }

    /// Like [`Compiler::compile`] with an explicit stage per entry point
    /// (vertex/fragment pairs for 3D pipelines).
    pub fn compile_stages(
        &mut self,
        source: &str,
        entries: &[(&str, Stage)],
        target: Target,
    ) -> Result<Compiled, CompileError> {
        let names: Vec<&str> = entries.iter().map(|e| e.0).collect();
        let entries_key: Vec<String> = entries.iter().map(|(n, s)| format!("{n}:{s:?}")).collect();
        let key_entries: Vec<&str> = entries_key.iter().map(String::as_str).collect();
        let _ = &names;
        let start = Instant::now();
        let key = self.key(source, &key_entries, target);
        let cache_path = self
            .cache_dir
            .as_ref()
            .map(|d| d.join(format!("{key}.{}", target.ext())));
        if let Some(p) = &cache_path
            && let Ok(code) = std::fs::read(p)
        {
            return Ok(Compiled {
                code,
                cached: true,
                ms: start.elapsed().as_secs_f64() * 1000.0,
            });
        }
        // Compiled before on another machine: kept locally from now on.
        let shared_path = self
            .shared_dir
            .as_ref()
            .map(|d| d.join(format!("{key}.{}", target.ext())));
        if let Some(sp) = &shared_path
            && let Ok(code) = std::fs::read(sp)
        {
            if let Some(p) = &cache_path {
                write_whole(p, &code);
            }
            return Ok(Compiled {
                code,
                cached: true,
                ms: start.elapsed().as_secs_f64() * 1000.0,
            });
        }
        let code = self.slang()?.compile(source, entries, target)?;
        if let Some(p) = &cache_path {
            write_whole(p, &code);
        }
        // Shared with the other machines off this thread (a bucket write
        // takes tens of milliseconds).
        if let Some(sp) = shared_path {
            let code = code.clone();
            std::thread::spawn(move || write_whole(&sp, &code));
        }
        Ok(Compiled {
            code,
            cached: false,
            ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }

    fn slang(&mut self) -> Result<&Slang, CompileError> {
        if self.slang.is_none() {
            if self.fetch && lib_in(&self.slang_dir).is_none() {
                fetch_slang(&self.slang_dir)?;
            }
            self.slang = Some(Slang::load(&self.slang_dir)?);
        }
        Ok(self.slang.as_ref().unwrap())
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

/// Windows looks for DLLs next to slang.dll only when told to: its imports
/// through the load flag, and the helpers Slang loads by name later
/// (slang-glslang runs spirv-opt) through the process DLL directory.
#[cfg(windows)]
unsafe fn open(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library as WinLibrary};
    use std::os::windows::ffi::OsStrExt;
    if let Some(dir) = path.parent() {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe { SetDllDirectoryW(wide.as_ptr()) };
    }
    unsafe { WinLibrary::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH) }.map(Library::from)
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetDllDirectoryW(path: *const u16) -> i32;
}

#[cfg(not(windows))]
unsafe fn open(path: &Path) -> Result<Library, libloading::Error> {
    unsafe { Library::new(path) }
}

impl Slang {
    fn load(dir: &Path) -> Result<Self, CompileError> {
        // Windows releases keep the DLLs in bin/.
        let path = &lib_in(dir).ok_or_else(|| CompileError::NotFound(dir.display().to_string()))?;
        unsafe {
            let lib = open(path).map_err(|e| CompileError::Load(e.to_string()))?;
            macro_rules! sym {
                ($name:literal, $ty:ty) => {{
                    let s: Symbol<$ty> = lib
                        .get(concat!($name, "\0").as_bytes())
                        .map_err(|e| CompileError::Load(format!("{}: {e}", $name)))?;
                    *s
                }};
            }
            let create_session = sym!("spCreateSession", SpCreateSession);
            let s = Slang {
                create_request: sym!("spCreateCompileRequest", SpCreateCompileRequest),
                destroy_request: sym!("spDestroyCompileRequest", SpDestroyCompileRequest),
                set_target: sym!("spSetCodeGenTarget", SpSetCodeGenTarget),
                set_target_flags: sym!("spSetTargetFlags", SpSetTargetFlags),
                set_opt: sym!("spSetOptimizationLevel", SpSetOptimizationLevel),
                add_tu: sym!("spAddTranslationUnit", SpAddTranslationUnit),
                add_source: sym!("spAddTranslationUnitSourceString", SpAddSource),
                add_entry: sym!("spAddEntryPoint", SpAddEntryPoint),
                compile: sym!("spCompile", SpCompile),
                diagnostics: sym!("spGetDiagnosticOutput", SpGetDiagnostics),
                get_code: sym!("spGetCompileRequestCode", SpGetCode),
                session: create_session(std::ptr::null()),
                _lib: lib,
            };
            if s.session.is_null() {
                return Err(CompileError::Load("spCreateSession returned null".into()));
            }
            Ok(s)
        }
    }

    fn compile(&self, source: &str, entries: &[(&str, Stage)], target: Target) -> Result<Vec<u8>, CompileError> {
        let src = CString::new(source).map_err(|e| CompileError::Compile(e.to_string()))?;
        let path = c"ultrafast_generated.slang";
        unsafe {
            let req = (self.create_request)(self.session);
            struct Guard<'a>(&'a Slang, *mut c_void);
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    unsafe { (self.0.destroy_request)(self.1) }
                }
            }
            let _g = Guard(self, req);
            (self.set_target)(req, target.slang_id());
            // MSL: one module holding every entry point. SPIR-V: emitted
            // directly (no glslang), all entry points in one module too.
            let flags = match target {
                Target::Metal => SLANG_TARGET_FLAG_GENERATE_WHOLE_PROGRAM,
                Target::Spirv => SLANG_TARGET_FLAG_GENERATE_WHOLE_PROGRAM | SLANG_TARGET_FLAG_GENERATE_SPIRV_DIRECTLY,
            };
            (self.set_target_flags)(req, 0, flags);
            (self.set_opt)(req, SLANG_OPTIMIZATION_LEVEL_HIGH);
            let tu = (self.add_tu)(req, SLANG_SOURCE_LANGUAGE_SLANG, std::ptr::null());
            (self.add_source)(req, tu, path.as_ptr(), src.as_ptr());
            let names: Vec<(CString, Stage)> = entries.iter().map(|(e, s)| (CString::new(*e).unwrap(), *s)).collect();
            for (n, stage) in &names {
                (self.add_entry)(req, tu, n.as_ptr(), stage.slang_id());
            }
            let rc = (self.compile)(req);
            let diag = (self.diagnostics)(req);
            let diag = if diag.is_null() {
                String::new()
            } else {
                CStr::from_ptr(diag).to_string_lossy().into_owned()
            };
            if rc < 0 {
                return Err(CompileError::Compile(diag));
            }
            let mut size = 0usize;
            let ptr = (self.get_code)(req, &mut size);
            if ptr.is_null() || size == 0 {
                return Err(CompileError::Compile(format!("no code produced\n{diag}")));
            }
            let mut code = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
            // Text targets may include a trailing NUL.
            if target == Target::Metal && code.last() == Some(&0) {
                code.pop();
            }
            Ok(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
WTexture2D<float4> OUT;
StructuredBuffer<float> P;
[shader("compute")][numthreads(8,8,1)]
void k_test(uint3 gid: SV_DispatchThreadID) { OUT.Store(gid.xy, float4(P[0])); }
[shader("compute")][numthreads(8,8,1)]
void k_test2(uint3 gid: SV_DispatchThreadID) { OUT.Store(gid.xy, float4(P[1])); }
"#;

    #[test]
    fn compiles_to_metal_and_spirv() {
        let mut c = Compiler::new().without_disk_cache();
        let t = Instant::now();
        let msl = c.compile(SRC, &["k_test", "k_test2"], Target::Metal).unwrap();
        eprintln!(
            "first compile (incl. session) {:.1} ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
        let text = msl.as_text();
        assert!(text.contains("k_test") && text.contains("k_test2"), "{text}");
        let spv = c.compile(SRC, &["k_test"], Target::Spirv).unwrap();
        assert_eq!(&spv.code[..4], &[0x03, 0x02, 0x23, 0x07]);
        eprintln!("spirv compile {:.1} ms", spv.ms);
    }

    #[test]
    fn errors_carry_diagnostics() {
        let mut c = Compiler::new().without_disk_cache();
        let e = c.compile("void broken( {", &["k"], Target::Metal).unwrap_err();
        assert!(e.to_string().contains("error"), "{e}");
    }
}
