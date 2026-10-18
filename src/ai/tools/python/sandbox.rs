//! Sandboxed Python execution via the embedded pybox WASM component.
//!
//! Adapted from the `pybox` project (`~/Projects/pybox`). The component is
//! built with `componentize-py` from a guest module and runs with no ambient
//! capabilities: no environment, network, subprocesses, or ctypes. The only
//! host access granted is a single preopened directory — the session workspace —
//! mounted at `/`, matching the bash tool. Memory and captured output are
//! bounded, and a wall-clock timeout is enforced via engine epoch interruption.

use anyhow::{Context, Result, anyhow};
use once_cell::sync::OnceCell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wasmtime::component::{Component, Linker};
use wasmtime::{Cache, Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Strategy};
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::{DirPerms, FilePerms, ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView};

// Default timeout in seconds.
const DEFAULT_TIMEOUT_SECONDS: u64 = 40;

// The engine epoch is advanced by a single background thread. A store's
// deadline is expressed in these ticks, so granularity is TICK_MS.
const TICK_MS: u64 = 100;
const TICKS_PER_SECOND: u64 = 1000 / TICK_MS;

// Caps on untrusted code. Memory is limited per linear memory; output is
// captured into bounded pipes so a guest cannot exhaust host memory or disk.
const MAX_MEMORY_BYTES: usize = 512 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

// The sandbox component is embedded so the binary is self-contained and never
// loads executable code from the (potentially untrusted) working directory.
const SANDBOX_WASM: &[u8] = include_bytes!("sandbox.wasm");

struct SandboxState {
    wasi_ctx: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    timed_out: bool,
}

impl wasmtime_wasi::WasiView for SandboxState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.table,
        }
    }
}

// NOTE: This generates a `Sandbox` type automatically and your IDE
// might not pick it up
wasmtime::component::bindgen!({
    path: "src/ai/tools/python/sandbox.wit",
    world: "sandbox",
});

/// The captured result of running guest code.
#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    /// JSON-serialized value of the last expression, or `null`.
    pub value: String,
    /// Bytes the guest wrote to stdout, decoded lossily.
    pub stdout: String,
    /// Bytes the guest wrote to stderr, decoded lossily.
    pub stderr: String,
}

/// A sandboxed Python execution environment using WebAssembly.
pub struct PySandbox {
    engine: Engine,
    component: Component,
    pub timeout_seconds: u64,
    stop_ticker: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

/// The process-wide sandbox. Compiling the embedded component is expensive, so
/// it happens once and is shared across all tool calls. `exec` takes `&self`
/// and builds a fresh store per call, so concurrent calls are safe.
pub fn shared_sandbox() -> Result<&'static PySandbox> {
    static SANDBOX: OnceCell<PySandbox> = OnceCell::new();
    SANDBOX.get_or_try_init(|| {
        if cfg!(test) {
            // Winch compiles the ~40 MB component far faster, which keeps the
            // test suite quick. Production uses the Cranelift default.
            PySandbox::new_for_test(None)
        } else {
            PySandbox::new(None)
        }
    })
}

impl PySandbox {
    /// Create a new webassembly sandbox for executing untrusted python code.
    ///
    /// # Arguments
    /// * `timeout_secs` - Optional timeout in seconds. Defaults to `DEFAULT_TIMEOUT_SECONDS`.
    pub fn new(timeout_secs: Option<u64>) -> Result<Self> {
        let mut cfg = Config::new();
        // Enable timeouts
        cfg.epoch_interruption(true);
        // Enable the compilation cache, using the default cache configuration
        // settings.
        cfg.cache(Some(Cache::from_file(None)?));
        let engine = Engine::new(&cfg)
            .map_err(anyhow::Error::from)
            .context("Failed to create wasm engine")?;

        Self::from_engine(engine, timeout_secs)
    }

    /// Create a sandbox with fast compilation settings for tests.
    ///
    /// Uses Winch baseline compiler, parallel compilation, and caching
    /// to significantly speed up test execution.
    ///
    /// # Arguments
    /// * `timeout_secs` - Optional timeout in seconds. Defaults to `DEFAULT_TIMEOUT_SECONDS`.
    pub fn new_for_test(timeout_secs: Option<u64>) -> Result<Self> {
        let mut config = Config::new();

        // Enable the compilation cache, using the default cache configuration
        // settings.
        config.cache(Some(Cache::from_file(None)?));

        // Enable Winch, Wasmtime's baseline compiler.
        config.strategy(Strategy::Winch);

        // Enable parallel compilation.
        config.parallel_compilation(true);

        // Enable epoch interruption for timeout support
        config.epoch_interruption(true);

        let engine = Engine::new(&config)
            .map_err(anyhow::Error::from)
            .context("Failed to create wasm engine")?;

        Self::from_engine(engine, timeout_secs)
    }

    fn from_engine(engine: Engine, timeout_secs: Option<u64>) -> Result<Self> {
        let timeout_seconds = timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECONDS);

        let component = Component::new(&engine, SANDBOX_WASM)
            .map_err(anyhow::Error::from)
            .context("Failed to load embedded sandbox.wasm")?;

        // A single background thread advances the engine epoch for all stores.
        // Unlike a per-call timer, it cannot fire during a later call and cause
        // a spurious timeout.
        let stop_ticker = Arc::new(AtomicBool::new(false));
        let ticker = {
            let engine = engine.clone();
            let stop = stop_ticker.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(TICK_MS));
                    engine.increment_epoch();
                }
            })
        };

        Ok(Self {
            engine,
            component,
            timeout_seconds,
            stop_ticker,
            ticker: Some(ticker),
        })
    }

    /// Execute Python code in the sandbox.
    ///
    /// Returns the last expression's value plus any captured stdout/stderr.
    /// `workspace` is exposed to guest code as the root directory (`/`) — the
    /// same directory the bash tool mounts — so files are shared between the two
    /// tools. Guest code cannot reach anything outside it, nor the host's
    /// stdin/stdout/stderr, environment, or network.
    pub fn exec(&self, code: &str, workspace: &Path) -> Result<ExecResult> {
        // The preopen below requires the host directory to already exist.
        std::fs::create_dir_all(workspace)?;

        let stdout_pipe = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
        let stderr_pipe = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);

        // Grant no ambient capabilities beyond the workspace: empty stdin (reads
        // see EOF immediately) and bounded in-memory stdout/stderr. The single
        // preopened directory is confined to `workspace` by WASI/cap-std, so
        // `..` and symlinks cannot escape it.
        let mut builder = WasiCtxBuilder::new();
        builder.stdin(MemoryInputPipe::new(Vec::<u8>::new()));
        builder.stdout(stdout_pipe.clone());
        builder.stderr(stderr_pipe.clone());
        builder.preopened_dir(workspace, "/", DirPerms::all(), FilePerms::all())?;

        let state = SandboxState {
            wasi_ctx: builder.build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(MAX_MEMORY_BYTES)
                .build(),
            timed_out: false,
        };

        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);

        // Deadline is relative to the current epoch, in ticker ticks.
        let deadline = self.timeout_seconds.saturating_mul(TICKS_PER_SECOND).max(1);
        store.set_epoch_deadline(deadline);
        store.epoch_deadline_callback(|mut ctx| {
            ctx.data_mut().timed_out = true;
            Err(wasmtime::Error::msg("execution timed out"))
        });

        // Set up linker with WASI
        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;

        // Instantiate the component
        let wasm_sandbox = Sandbox::instantiate(&mut store, &self.component, &linker)?;

        // Give the guest a fresh budget: instantiation above may have consumed
        // part of the deadline, which should only bound user code.
        store.set_epoch_deadline(deadline);

        // Execute the code
        let result = wasm_sandbox.call_exec(&mut store, code);
        let timed_out = store.data().timed_out;

        match result {
            Ok(Ok(value)) => Ok(ExecResult {
                value,
                stdout: decode_pipe(&stdout_pipe),
                stderr: decode_pipe(&stderr_pipe),
            }),
            // Guest-level error: a Python traceback from the wrapper, safe to
            // surface to the caller.
            Ok(Err(e)) => Err(anyhow!("{}", e)),
            // Host-level trap. Surface a clean message rather than the raw
            // wasmtime trap, which leaks internal symbols and addresses.
            Err(_) if timed_out => Err(anyhow!("Execution timed out")),
            Err(_) => Err(anyhow!("sandbox execution failed (resource limit or trap)")),
        }
    }
}

impl Drop for PySandbox {
    fn drop(&mut self) {
        self.stop_ticker.store(true, Ordering::SeqCst);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
    }
}

fn decode_pipe(pipe: &MemoryOutputPipe) -> String {
    String::from_utf8_lossy(&pipe.contents()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_default_timeout() {
        let sandbox = PySandbox::new_for_test(None).expect("Failed to create sandbox");
        assert_eq!(sandbox.timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
    }

    #[test]
    fn test_sandbox_custom_timeout() {
        let sandbox = PySandbox::new_for_test(Some(10)).expect("Failed to create sandbox");
        assert_eq!(sandbox.timeout_seconds, 10);
    }

    #[test]
    fn test_sandbox_creates_successfully_with_wasm() {
        let result = PySandbox::new_for_test(None);
        assert!(result.is_ok());
    }
}
