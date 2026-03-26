//! Zig orchestrator integration — calls orch_* and divi_* C functions.
//!
//! The daemon links both `libomnidea_orchestrator.a` (Zig) and `libdivinity_ffi.a` (Rust).
//! The orchestrator composes 994 divi_* FFI calls into smart app-level operations,
//! and the pipeline executor dispatches any operation by name.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, Ordering};

// ── C FFI declarations ──────────────────────────────────────────────

extern "C" {
    // Orchestrator lifecycle
    fn orch_init() -> i32;
    fn orch_shutdown();

    // Pipeline executor — runs a JSON pipeline spec, returns JSON result
    fn orch_pipeline_execute(pipeline_json: *const c_char) -> *mut c_char;

    // Registry queries
    fn orch_registry_has_op(key: *const c_char) -> bool;
    fn orch_registry_list_ops() -> *mut c_char;
    fn orch_registry_count() -> i32;

    // Identity lifecycle
    fn orch_create_identity() -> i32;
    fn orch_soul_load(path: *const c_char) -> i32;
    fn orch_identity_load(data: *const u8, data_len: usize) -> i32;

    // Vault lifecycle
    fn orch_vault_setup(password: *const c_char, root_path: *const c_char) -> i32;
    fn orch_vault_lock() -> i32;
    fn orch_vault_is_unlocked() -> bool;

    // Error + cleanup
    fn divi_last_error() -> *mut c_char;
    fn divi_free_string(ptr: *mut c_char);
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);

// ── Public API ──────────────────────────────────────────────────────

/// Initialize the orchestrator. Call once at daemon startup.
pub fn init() -> Result<(), String> {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return Ok(()); // already initialized
    }
    let code = unsafe { orch_init() };
    if code != 0 {
        INITIALIZED.store(false, Ordering::SeqCst);
        return Err(format!("orch_init failed: {}", last_error_or(code)));
    }
    log::info!("Orchestrator initialized");
    Ok(())
}

/// Shut down the orchestrator. Call once at daemon shutdown.
pub fn shutdown() {
    if INITIALIZED.swap(false, Ordering::SeqCst) {
        unsafe { orch_shutdown() };
        log::info!("Orchestrator shut down");
    }
}

/// Execute a pipeline through the orchestrator.
/// Input: JSON pipeline string. Output: JSON result string.
pub fn pipeline_execute(pipeline_json: &str) -> Result<String, String> {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return Err("Orchestrator not initialized".into());
    }

    let c_input = CString::new(pipeline_json)
        .map_err(|_| "Pipeline JSON contains null bytes")?;

    let result_ptr = unsafe { orch_pipeline_execute(c_input.as_ptr()) };
    if result_ptr.is_null() {
        return Err(format!("Pipeline execution failed: {}", last_error()));
    }

    let result = unsafe { CStr::from_ptr(result_ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { divi_free_string(result_ptr) };
    Ok(result)
}

/// Create a new identity in the orchestrator (generates keypair).
pub fn create_identity() -> Result<(), String> {
    let code = unsafe { orch_create_identity() };
    if code != 0 {
        return Err(format!("orch_create_identity failed: {}", last_error_or(code)));
    }
    Ok(())
}

/// Load an existing soul (identity directory) into the orchestrator.
///
/// `path` is the data directory (e.g. `~/.omnidea/data`).
/// `orch_soul_load` calls `divi_crown_soul_load` which calls `Soul::load(dir)`
/// — Soul::load expects a DIRECTORY and appends `/soul.json` itself.
pub fn load_identity(path: &str) -> Result<(), String> {
    let soul_path = format!("{}/soul", path);
    let c_path = CString::new(soul_path).map_err(|_| "Path contains null bytes")?;
    let code = unsafe { orch_soul_load(c_path.as_ptr()) };
    if code != 0 {
        return Err(format!("orch_soul_load failed: {}", last_error_or(code)));
    }
    Ok(())
}

/// Load a keyring (exported bytes) into the orchestrator.
///
/// The keyring contains the private keys needed for signing and
/// for `idea.create` to get the author pubkey.
pub fn load_keyring(data: &[u8]) -> Result<(), String> {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return Err("Orchestrator not initialized".into());
    }
    let code = unsafe { orch_identity_load(data.as_ptr(), data.len()) };
    if code != 0 {
        return Err(format!("orch_identity_load failed: {}", last_error_or(code)));
    }
    Ok(())
}

/// Set up the Vault in the orchestrator.
pub fn vault_setup(password: &str, root_path: &str) -> Result<(), String> {
    let c_pw = CString::new(password).map_err(|_| "Password contains null bytes")?;
    let c_path = CString::new(root_path).map_err(|_| "Path contains null bytes")?;
    let code = unsafe { orch_vault_setup(c_pw.as_ptr(), c_path.as_ptr()) };
    if code != 0 {
        return Err(format!("orch_vault_setup failed: {}", last_error_or(code)));
    }
    Ok(())
}

/// Check if a named operation exists in the registry.
pub fn has_op(name: &str) -> bool {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return false;
    }
    let Ok(c_name) = CString::new(name) else {
        return false;
    };
    unsafe { orch_registry_has_op(c_name.as_ptr()) }
}

/// List all registered operation names (JSON array string).
pub fn list_ops() -> Result<String, String> {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return Err("Orchestrator not initialized".into());
    }
    let ptr = unsafe { orch_registry_list_ops() };
    if ptr.is_null() {
        return Err("Failed to list operations".into());
    }
    let result = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { divi_free_string(ptr) };
    Ok(result)
}

/// Get the total count of registered operations.
pub fn op_count() -> i32 {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return 0;
    }
    unsafe { orch_registry_count() }
}

/// Check if the orchestrator is initialized.
pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::SeqCst)
}

/// Check if the Vault is unlocked.
pub fn vault_is_unlocked() -> bool {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return false;
    }
    unsafe { orch_vault_is_unlocked() }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn last_error() -> String {
    let ptr = unsafe { divi_last_error() };
    if ptr.is_null() {
        return "(no error details)".into();
    }
    let msg = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { divi_free_string(ptr) };
    msg
}

fn last_error_or(code: i32) -> String {
    let err = last_error();
    if err == "(no error details)" {
        format!("error code {code}")
    } else {
        err
    }
}
