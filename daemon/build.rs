use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

fn main() {
    let omninet_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Omninet");

    // ── Link the Rust FFI library (divi_* C functions) ─────────────
    let target_dir = omninet_root.join("Target");
    let release_dir = target_dir.join("release");
    let debug_dir = target_dir.join("debug");
    if release_dir.join("libdivinity_ffi.a").exists() {
        println!("cargo:rustc-link-search=native={}", release_dir.display());
    } else {
        println!("cargo:rustc-link-search=native={}", debug_dir.display());
    }
    println!("cargo:rustc-link-lib=static=divinity_ffi");

    // macOS frameworks required by divinity_ffi (SQLCipher, crypto, etc.)
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=Security");
        println!("cargo:rustc-link-lib=framework=SystemConfiguration");
    }

    // ── Parse divinity_ffi.h and generate Phone handler registrations ──
    let header_path = omninet_root.join(
        "Divinity/Apple/Sources/COmnideaFFI/include/divinity_ffi.h",
    );
    println!("cargo:rerun-if-changed={}", header_path.display());

    if header_path.exists() {
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let out_path = Path::new(&out_dir).join("ffi_ops_generated.rs");
        generate_ffi_ops(&header_path, &out_path);
    } else {
        eprintln!(
            "cargo:warning=divinity_ffi.h not found at {}; skipping FFI op generation",
            header_path.display()
        );
    }
}

// ── C Header Parser ─────────────────────────────────────────────────

/// A parsed function declaration from the C header.
#[derive(Debug, Clone)]
struct FfiFunc {
    name: String,
    return_type: ReturnType,
    params: Vec<Param>,
}

#[derive(Debug, Clone, PartialEq)]
enum ReturnType {
    CharPtr,   // char *
    Int32,     // int32_t
    Bool,      // bool
    Void,      // void
    Uintptr,   // uintptr_t
    StructPtr, // struct Foo *
}

#[derive(Debug, Clone, PartialEq)]
enum Param {
    Void,                        // (void)
    ConstCharPtr(String),        // const char *name
    ConstStructPtr(String, String), // const struct Foo *name
    MutStructPtr(String, String),   // struct Foo *name
    Int32(String),               // int32_t name
    Uint32(String),              // uint32_t name
    Uint64(String),              // uint64_t name
    Double(String),              // double name
    Uintptr(String),             // uintptr_t name
    Bool(String),                // bool name
    Other(String),               // anything else (callbacks, out-params, etc.)
}

/// Which dispatch pattern a function maps to.
#[derive(Debug, Clone, PartialEq)]
enum DispatchPattern {
    /// () -> char*
    VoidToStr,
    /// (const char*) -> char*
    StrToStr,
    /// (const char*, const char*) -> char*
    Str2ToStr,
    /// (const char*, const char*, const char*) -> char*
    Str3ToStr,
    /// () -> int32_t
    VoidToI32,
    /// (const char*) -> int32_t
    StrToI32,
    /// (const char*, const char*) -> int32_t
    Str2ToI32,
    /// () -> bool
    VoidToBool,
    /// (const char*) -> bool
    StrToBool,
    /// (const char*) -> void
    StrToVoid,
    // ── Handle-bearing patterns (single handle as first param) ──
    /// (handle) -> char*
    HandleToStr(String),
    /// (handle, const char*) -> char*
    HandleStrToStr(String),
    /// (handle) -> i32
    HandleToI32(String),
    /// (handle, const char*) -> i32
    HandleStrToI32(String),
    /// (handle) -> bool
    HandleToBool(String),
    /// (handle, const char*) -> bool
    HandleStrToBool(String),
    /// (handle) -> void
    HandleToVoid(String),
    /// (handle, const char*) -> void
    HandleStrToVoid(String),
    /// Skip: constructor, destructor, multi-handle, callback, or complex
    Skip,
}

fn parse_header(path: &Path) -> Vec<FfiFunc> {
    let content = fs::read_to_string(path).expect("failed to read divinity_ffi.h");
    let mut functions = Vec::new();

    // Join multi-line declarations into single lines.
    // cbindgen wraps long params with leading whitespace.
    let mut joined = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("/*")
            || trimmed.starts_with("*") || trimmed.starts_with("#")
            || trimmed.starts_with("typedef") || trimmed.starts_with("}")
            || trimmed.starts_with("{")
        {
            if !joined.is_empty() {
                // flush pending
                if let Some(f) = parse_declaration(&joined) {
                    functions.push(f);
                }
                joined.clear();
            }
            continue;
        }

        // If the line starts with a return type keyword, it's a new declaration.
        if line.starts_with("char ")
            || line.starts_with("int32_t ")
            || line.starts_with("bool ")
            || line.starts_with("void ")
            || line.starts_with("uintptr_t ")
            || line.starts_with("struct ")
        {
            // Flush previous
            if !joined.is_empty() {
                if let Some(f) = parse_declaration(&joined) {
                    functions.push(f);
                }
            }
            joined = line.to_string();
        } else {
            // Continuation line
            joined.push(' ');
            joined.push_str(trimmed);
        }
    }
    // Flush last
    if !joined.is_empty() {
        if let Some(f) = parse_declaration(&joined) {
            functions.push(f);
        }
    }

    functions
}

fn parse_declaration(line: &str) -> Option<FfiFunc> {
    // Must contain "divi_" to be a function we care about
    if !line.contains("divi_") {
        return None;
    }
    // Must end with ";" (possibly after whitespace)
    let line = line.trim();
    if !line.ends_with(';') {
        return None;
    }
    let line = &line[..line.len() - 1]; // strip trailing ;

    // Split at '(' to get return+name and params
    let paren_pos = line.find('(')?;
    let before_paren = &line[..paren_pos];
    let params_str = &line[paren_pos + 1..].trim_end_matches(')');

    // Parse return type and function name from before_paren.
    // Formats: "char *divi_foo", "int32_t divi_foo", "void divi_foo",
    //          "struct Foo *divi_foo", "bool divi_foo", "uintptr_t divi_foo"
    let (return_type, name) = parse_return_and_name(before_paren)?;

    // Parse parameters
    let params = parse_params(params_str);

    Some(FfiFunc { name, return_type, params })
}

fn parse_return_and_name(s: &str) -> Option<(ReturnType, String)> {
    let s = s.trim();

    if let Some(rest) = s.strip_prefix("char *") {
        Some((ReturnType::CharPtr, rest.trim().to_string()))
    } else if let Some(rest) = s.strip_prefix("int32_t ") {
        Some((ReturnType::Int32, rest.trim().to_string()))
    } else if let Some(rest) = s.strip_prefix("bool ") {
        Some((ReturnType::Bool, rest.trim().to_string()))
    } else if let Some(rest) = s.strip_prefix("uintptr_t ") {
        Some((ReturnType::Uintptr, rest.trim().to_string()))
    } else if let Some(rest) = s.strip_prefix("void ") {
        Some((ReturnType::Void, rest.trim().to_string()))
    } else if s.starts_with("struct ") {
        // "struct Foo *divi_foo_new"
        let star_pos = s.find('*')?;
        let name = s[star_pos + 1..].trim().to_string();
        Some((ReturnType::StructPtr, name))
    } else {
        None
    }
}

fn parse_params(s: &str) -> Vec<Param> {
    let s = s.trim();
    if s == "void" || s.is_empty() {
        return vec![Param::Void];
    }

    s.split(',')
        .map(|p| {
            let p = p.trim();
            if p == "void" {
                Param::Void
            } else if p.starts_with("const char *") || p.starts_with("const char*") {
                let name = p.rsplit(' ').next().unwrap_or("").trim_start_matches('*');
                Param::ConstCharPtr(name.to_string())
            } else if p.starts_with("const struct ") {
                // "const struct Foo *name"
                let parts: Vec<&str> = p.split_whitespace().collect();
                let struct_name = parts.get(2).unwrap_or(&"").trim_start_matches('*');
                let param_name = parts.last().unwrap_or(&"").trim_start_matches('*');
                Param::ConstStructPtr(struct_name.to_string(), param_name.to_string())
            } else if p.starts_with("struct ") && p.contains('*') {
                let parts: Vec<&str> = p.split_whitespace().collect();
                let struct_name = parts.get(1).unwrap_or(&"").trim_start_matches('*');
                let param_name = parts.last().unwrap_or(&"").trim_start_matches('*');
                Param::MutStructPtr(struct_name.to_string(), param_name.to_string())
            } else if p.starts_with("int32_t ") {
                let name = p.strip_prefix("int32_t ").unwrap().trim();
                Param::Int32(name.to_string())
            } else if p.starts_with("uint32_t ") {
                let name = p.strip_prefix("uint32_t ").unwrap().trim();
                Param::Uint32(name.to_string())
            } else if p.starts_with("uint64_t ") {
                let name = p.strip_prefix("uint64_t ").unwrap().trim();
                Param::Uint64(name.to_string())
            } else if p.starts_with("double ") {
                let name = p.strip_prefix("double ").unwrap().trim();
                Param::Double(name.to_string())
            } else if p.starts_with("uintptr_t ") {
                let name = p.strip_prefix("uintptr_t ").unwrap().trim();
                Param::Uintptr(name.to_string())
            } else if p.starts_with("bool ") {
                let name = p.strip_prefix("bool ").unwrap().trim();
                Param::Bool(name.to_string())
            } else {
                Param::Other(p.to_string())
            }
        })
        .collect()
}

/// Known handle types that the daemon manages globally.
/// Maps struct name → the FFI constructor function to get the handle.
fn is_known_handle(struct_name: &str) -> bool {
    matches!(struct_name,
        "DiviVault" | "CrownKeyring" | "CrownSoul"
        | "QuestEngineFFI" | "MagicDocumentState" | "MagicCanvasState"
        | "MagicDocumentHistory" | "MagicToolRegistry"
        | "AdvisorLoop" | "AdvisorStore" | "AdvisorRouter" | "AdvisorSkills"
        | "OracleWorkflowRegistry"
        | "BulwarkPermissionChecker" | "BulwarkConsentValidator"
        | "PolityRightsRegistry" | "PolityDutiesRegistry"
        | "PolityProtectionsRegistry" | "PolityBreachRegistry"
        | "PolityConsentRegistry" | "PolityAmendmentRegistry"
        | "DiviAppCatalog" | "DiviDeviceFleet"
        | "IdeasPackage" | "IdeasSchemaRegistry"
        | "ZeitgeistDirectory" | "ZeitgeistCache"
        | "CommerceProduct" | "CommerceCart" | "CommerceOrder"
        | "FortuneTreasury" | "FortuneLedger" | "FortunePolicy"
        | "ExporterRegistry" | "ImporterRegistry" | "BridgeRegistry"
        | "JailTrustGraph"
    )
}

fn classify(f: &FfiFunc) -> DispatchPattern {
    // Skip constructors (_new), destructors (_free), helpers
    if f.name.ends_with("_free") || f.name.ends_with("_new") {
        return DispatchPattern::Skip;
    }
    if f.name == "divi_last_error" || f.name == "divi_free_string" || f.name == "divi_free_bytes" {
        return DispatchPattern::Skip;
    }
    // Skip anything with complex params (callbacks, out-params, etc.)
    if f.params.iter().any(|p| matches!(p, Param::Other(..))) {
        return DispatchPattern::Skip;
    }
    // Skip mutable struct pointer params (need special ownership handling)
    if f.params.iter().any(|p| matches!(p, Param::MutStructPtr(..))) {
        return DispatchPattern::Skip;
    }
    // Skip struct pointer returns (constructors that didn't end in _new)
    if f.return_type == ReturnType::StructPtr {
        return DispatchPattern::Skip;
    }

    // Count param types
    let handle_params: Vec<&String> = f.params.iter().filter_map(|p| {
        if let Param::ConstStructPtr(s, _) = p { Some(s) } else { None }
    }).collect();
    let str_count = f.params.iter().filter(|p| matches!(p, Param::ConstCharPtr(..))).count();
    let is_void_params = f.params.len() == 1 && f.params[0] == Param::Void;
    let has_numeric = f.params.iter().any(|p| matches!(p,
        Param::Int32(..) | Param::Uint32(..) | Param::Uint64(..) |
        Param::Double(..) | Param::Uintptr(..) | Param::Bool(..)
    ));

    // Skip multi-handle ops and numeric params (for now)
    if handle_params.len() > 1 || has_numeric {
        return DispatchPattern::Skip;
    }

    // ── Handle-bearing patterns (single known handle + optional str) ──
    if handle_params.len() == 1 {
        let handle_type = handle_params[0].clone();
        if !is_known_handle(&handle_type) {
            return DispatchPattern::Skip;
        }
        return match (&f.return_type, str_count) {
            (ReturnType::CharPtr, 0) => DispatchPattern::HandleToStr(handle_type),
            (ReturnType::CharPtr, 1) => DispatchPattern::HandleStrToStr(handle_type),
            (ReturnType::Int32, 0) => DispatchPattern::HandleToI32(handle_type),
            (ReturnType::Int32, 1) => DispatchPattern::HandleStrToI32(handle_type),
            (ReturnType::Bool, 0) => DispatchPattern::HandleToBool(handle_type),
            (ReturnType::Bool, 1) => DispatchPattern::HandleStrToBool(handle_type),
            (ReturnType::Void, 0) => DispatchPattern::HandleToVoid(handle_type),
            (ReturnType::Void, 1) => DispatchPattern::HandleStrToVoid(handle_type),
            _ => DispatchPattern::Skip,
        };
    }

    // ── Stateless patterns (no handles) ──
    let has_only_str_params = f.params.iter().all(|p| matches!(p, Param::ConstCharPtr(..)));

    match (&f.return_type, is_void_params, str_count) {
        (ReturnType::CharPtr, true, _) => DispatchPattern::VoidToStr,
        (ReturnType::CharPtr, false, 1) if has_only_str_params => DispatchPattern::StrToStr,
        (ReturnType::CharPtr, false, 2) if has_only_str_params => DispatchPattern::Str2ToStr,
        (ReturnType::CharPtr, false, 3) if has_only_str_params => DispatchPattern::Str3ToStr,
        (ReturnType::Int32, true, _) => DispatchPattern::VoidToI32,
        (ReturnType::Int32, false, 1) if has_only_str_params => DispatchPattern::StrToI32,
        (ReturnType::Int32, false, 2) if has_only_str_params => DispatchPattern::Str2ToI32,
        (ReturnType::Bool, true, _) => DispatchPattern::VoidToBool,
        (ReturnType::Bool, false, 1) if has_only_str_params => DispatchPattern::StrToBool,
        (ReturnType::Void, false, 1) if has_only_str_params => DispatchPattern::StrToVoid,
        _ => DispatchPattern::Skip,
    }
}

/// Convert divi_crown_soul_profile → crown.soul_profile
fn op_name(func_name: &str) -> String {
    let stripped = func_name.strip_prefix("divi_").unwrap_or(func_name);
    // First underscore separates module from op
    if let Some(pos) = stripped.find('_') {
        let module = &stripped[..pos];
        let op = &stripped[pos + 1..];
        format!("{module}.{op}")
    } else {
        stripped.to_string()
    }
}

// ── Code Generator ──────────────────────────────────────────────────

fn generate_ffi_ops(header_path: &Path, out_path: &Path) {
    let functions = parse_header(header_path);
    let mut out = fs::File::create(out_path).expect("failed to create ffi_ops_generated.rs");

    // Collect dispatchable functions grouped by pattern
    let mut by_pattern: HashMap<String, Vec<(String, String, DispatchPattern)>> = HashMap::new();
    let mut skip_count = 0u32;
    let mut dispatch_count = 0u32;

    for f in &functions {
        let pattern = classify(f);
        if pattern == DispatchPattern::Skip {
            skip_count += 1;
            continue;
        }
        dispatch_count += 1;
        let op = op_name(&f.name);
        by_pattern
            .entry(format!("{:?}", pattern))
            .or_default()
            .push((f.name.clone(), op, pattern));
    }

    // Write the generated file
    writeln!(out, "// Auto-generated from divinity_ffi.h — do not edit manually.").unwrap();
    writeln!(out, "// {dispatch_count} dispatchable ops, {skip_count} skipped (handles/constructors/destructors/complex).").unwrap();
    writeln!(out, "//").unwrap();
    writeln!(out, "// Generated by daemon/build.rs from the C header.").unwrap();
    writeln!(out, "// Each divi_* function is registered as a Phone handler via Equipment.").unwrap();
    writeln!(out).unwrap();
    // Note: can't use #![allow()] inner attribute in include!() context.
    writeln!(out).unwrap();
    writeln!(out, "use std::ffi::{{CStr, CString}};").unwrap();
    writeln!(out, "use std::os::raw::c_char;").unwrap();
    writeln!(out, "use equipment::{{Phone, PhoneError}};").unwrap();
    writeln!(out).unwrap();

    // Collect all handle types used by dispatchable functions
    let mut handle_types_used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in &functions {
        let pattern = classify(f);
        match &pattern {
            DispatchPattern::HandleToStr(h) | DispatchPattern::HandleStrToStr(h)
            | DispatchPattern::HandleToI32(h) | DispatchPattern::HandleStrToI32(h)
            | DispatchPattern::HandleToBool(h) | DispatchPattern::HandleStrToBool(h)
            | DispatchPattern::HandleToVoid(h) | DispatchPattern::HandleStrToVoid(h) => {
                handle_types_used.insert(h.clone());
            }
            _ => {}
        }
    }

    // Opaque struct declarations for handle types
    for ht in &handle_types_used {
        writeln!(out, "#[repr(C)] pub struct {ht} {{ _opaque: [u8; 0] }}").unwrap();
    }
    writeln!(out).unwrap();

    // Handle registry — stores opaque pointers set by hand-written modules
    writeln!(out, "use std::sync::Mutex;").unwrap();
    writeln!(out, "use std::collections::HashMap;").unwrap();
    writeln!(out).unwrap();
    // Send-safe wrapper for raw pointers (FFI handles are thread-safe by convention)
    writeln!(out, "#[derive(Clone, Copy)]").unwrap();
    writeln!(out, "struct SendPtr(*const u8);").unwrap();
    writeln!(out, "unsafe impl Send for SendPtr {{}}").unwrap();
    writeln!(out, "unsafe impl Sync for SendPtr {{}}").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "static HANDLES: std::sync::LazyLock<Mutex<HashMap<&'static str, SendPtr>>> =").unwrap();
    writeln!(out, "    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "/// Store a handle in the FFI handle registry.").unwrap();
    writeln!(out, "/// Called by hand-written modules after constructing handles.").unwrap();
    writeln!(out, "pub fn set_handle(type_name: &'static str, ptr: *const u8) {{").unwrap();
    writeln!(out, "    HANDLES.lock().unwrap().insert(type_name, SendPtr(ptr));").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "fn get_handle(type_name: &str) -> Option<*const u8> {{").unwrap();
    writeln!(out, "    HANDLES.lock().unwrap().get(type_name).map(|p| p.0)").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    // Extern declarations for all dispatchable functions + helpers
    writeln!(out, "extern \"C\" {{").unwrap();
    writeln!(out, "    fn divi_last_error() -> *mut c_char;").unwrap();
    writeln!(out, "    fn divi_free_string(ptr: *mut c_char);").unwrap();

    for f in &functions {
        let pattern = classify(f);
        if pattern == DispatchPattern::Skip {
            continue;
        }
        write!(out, "    fn {}(", f.name).unwrap();
        let param_strs: Vec<String> = match &pattern {
            DispatchPattern::VoidToStr | DispatchPattern::VoidToI32 | DispatchPattern::VoidToBool => vec![],
            DispatchPattern::StrToStr | DispatchPattern::StrToI32 | DispatchPattern::StrToBool | DispatchPattern::StrToVoid => {
                vec!["a: *const c_char".into()]
            }
            DispatchPattern::Str2ToStr | DispatchPattern::Str2ToI32 => {
                vec!["a: *const c_char".into(), "b: *const c_char".into()]
            }
            DispatchPattern::Str3ToStr => {
                vec!["a: *const c_char".into(), "b: *const c_char".into(), "c: *const c_char".into()]
            }
            // Handle-bearing: first param is the opaque struct pointer
            DispatchPattern::HandleToStr(h) | DispatchPattern::HandleToI32(h)
            | DispatchPattern::HandleToBool(h) | DispatchPattern::HandleToVoid(h) => {
                vec![format!("h: *const {h}")]
            }
            DispatchPattern::HandleStrToStr(h) | DispatchPattern::HandleStrToI32(h)
            | DispatchPattern::HandleStrToBool(h) | DispatchPattern::HandleStrToVoid(h) => {
                vec![format!("h: *const {h}"), "a: *const c_char".into()]
            }
            _ => vec![],
        };
        write!(out, "{}", param_strs.join(", ")).unwrap();
        let ret = match &pattern {
            DispatchPattern::VoidToStr | DispatchPattern::StrToStr
            | DispatchPattern::Str2ToStr | DispatchPattern::Str3ToStr
            | DispatchPattern::HandleToStr(_) | DispatchPattern::HandleStrToStr(_) => " -> *mut c_char",
            DispatchPattern::VoidToI32 | DispatchPattern::StrToI32 | DispatchPattern::Str2ToI32
            | DispatchPattern::HandleToI32(_) | DispatchPattern::HandleStrToI32(_) => " -> i32",
            DispatchPattern::VoidToBool | DispatchPattern::StrToBool
            | DispatchPattern::HandleToBool(_) | DispatchPattern::HandleStrToBool(_) => " -> bool",
            DispatchPattern::StrToVoid
            | DispatchPattern::HandleToVoid(_) | DispatchPattern::HandleStrToVoid(_) => "",
            _ => "",
        };
        writeln!(out, "){ret};").unwrap();
    }
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    // Helper: get last error
    writeln!(out, "fn ffi_last_error() -> String {{").unwrap();
    writeln!(out, "    let ptr = unsafe {{ divi_last_error() }};").unwrap();
    writeln!(out, "    if ptr.is_null() {{ return \"(no error details)\".into(); }}").unwrap();
    writeln!(out, "    let msg = unsafe {{ CStr::from_ptr(ptr) }}.to_string_lossy().into_owned();").unwrap();
    writeln!(out, "    unsafe {{ divi_free_string(ptr) }};").unwrap();
    writeln!(out, "    msg").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    // Helper: make PhoneError (takes String — use .to_string() for &str at call sites)
    writeln!(out, "fn ffi_err(op: &str, msg: String) -> PhoneError {{").unwrap();
    writeln!(out, "    PhoneError::HandlerFailed {{ call_id: op.to_string(), message: msg }}").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    // The main registration function
    writeln!(out, "/// Register all auto-discovered FFI operations as Phone handlers.").unwrap();
    writeln!(out, "///").unwrap();
    writeln!(out, "/// Call this BEFORE registering hand-written override modules,").unwrap();
    writeln!(out, "/// so that Rust-native handlers can replace FFI handlers for complex ops.").unwrap();
    writeln!(out, "pub fn register_all(phone: &Phone) {{").unwrap();

    for f in &functions {
        let pattern = classify(f);
        if pattern == DispatchPattern::Skip {
            continue;
        }
        let op = op_name(&f.name);
        let fname = &f.name;

        match pattern {
            DispatchPattern::VoidToStr => {
                writeln!(out, "    phone.register_raw(\"{op}\", |_data| {{").unwrap();
                writeln!(out, "        let result = unsafe {{ {fname}() }};").unwrap();
                writeln!(out, "        if result.is_null() {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        let out = unsafe {{ CStr::from_ptr(result) }}.to_bytes().to_vec();").unwrap();
                writeln!(out, "        unsafe {{ divi_free_string(result) }};").unwrap();
                writeln!(out, "        Ok(out)").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::StrToStr => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let c_a = CString::new(data).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let result = unsafe {{ {fname}(c_a.as_ptr()) }};").unwrap();
                writeln!(out, "        if result.is_null() {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        let out = unsafe {{ CStr::from_ptr(result) }}.to_bytes().to_vec();").unwrap();
                writeln!(out, "        unsafe {{ divi_free_string(result) }};").unwrap();
                writeln!(out, "        Ok(out)").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::Str2ToStr => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let args: Vec<&str> = serde_json::from_slice(data).map_err(|e| ffi_err(\"{op}\", e.to_string()))?;").unwrap();
                writeln!(out, "        if args.len() < 2 {{ return Err(ffi_err(\"{op}\", \"expected 2 args\".to_string())); }}").unwrap();
                writeln!(out, "        let c_a = CString::new(args[0]).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let c_b = CString::new(args[1]).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let result = unsafe {{ {fname}(c_a.as_ptr(), c_b.as_ptr()) }};").unwrap();
                writeln!(out, "        if result.is_null() {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        let out = unsafe {{ CStr::from_ptr(result) }}.to_bytes().to_vec();").unwrap();
                writeln!(out, "        unsafe {{ divi_free_string(result) }};").unwrap();
                writeln!(out, "        Ok(out)").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::Str3ToStr => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let args: Vec<&str> = serde_json::from_slice(data).map_err(|e| ffi_err(\"{op}\", e.to_string()))?;").unwrap();
                writeln!(out, "        if args.len() < 3 {{ return Err(ffi_err(\"{op}\", \"expected 3 args\".to_string())); }}").unwrap();
                writeln!(out, "        let c_a = CString::new(args[0]).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let c_b = CString::new(args[1]).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let c_c = CString::new(args[2]).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let result = unsafe {{ {fname}(c_a.as_ptr(), c_b.as_ptr(), c_c.as_ptr()) }};").unwrap();
                writeln!(out, "        if result.is_null() {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        let out = unsafe {{ CStr::from_ptr(result) }}.to_bytes().to_vec();").unwrap();
                writeln!(out, "        unsafe {{ divi_free_string(result) }};").unwrap();
                writeln!(out, "        Ok(out)").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::VoidToI32 => {
                writeln!(out, "    phone.register_raw(\"{op}\", |_data| {{").unwrap();
                writeln!(out, "        let code = unsafe {{ {fname}() }};").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"code\": code}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::StrToI32 => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let c_a = CString::new(data).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let code = unsafe {{ {fname}(c_a.as_ptr()) }};").unwrap();
                writeln!(out, "        if code != 0 {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"ok\": true}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::Str2ToI32 => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let args: Vec<&str> = serde_json::from_slice(data).map_err(|e| ffi_err(\"{op}\", e.to_string()))?;").unwrap();
                writeln!(out, "        if args.len() < 2 {{ return Err(ffi_err(\"{op}\", \"expected 2 args\".to_string())); }}").unwrap();
                writeln!(out, "        let c_a = CString::new(args[0]).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let c_b = CString::new(args[1]).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let code = unsafe {{ {fname}(c_a.as_ptr(), c_b.as_ptr()) }};").unwrap();
                writeln!(out, "        if code != 0 {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"ok\": true}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::VoidToBool => {
                writeln!(out, "    phone.register_raw(\"{op}\", |_data| {{").unwrap();
                writeln!(out, "        let val = unsafe {{ {fname}() }};").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"result\": val}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::StrToBool => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let c_a = CString::new(data).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let val = unsafe {{ {fname}(c_a.as_ptr()) }};").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"result\": val}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::StrToVoid => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let c_a = CString::new(data).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        unsafe {{ {fname}(c_a.as_ptr()) }};").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"ok\": true}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            // ── Handle-bearing patterns ──
            DispatchPattern::HandleToStr(ref h) => {
                writeln!(out, "    phone.register_raw(\"{op}\", |_data| {{").unwrap();
                writeln!(out, "        let h = get_handle(\"{h}\").ok_or_else(|| ffi_err(\"{op}\", \"handle '{h}' not initialized\".to_string()))?;").unwrap();
                writeln!(out, "        let result = unsafe {{ {fname}(h as *const {h}) }};").unwrap();
                writeln!(out, "        if result.is_null() {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        let out = unsafe {{ CStr::from_ptr(result) }}.to_bytes().to_vec();").unwrap();
                writeln!(out, "        unsafe {{ divi_free_string(result) }};").unwrap();
                writeln!(out, "        Ok(out)").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::HandleStrToStr(ref h) => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let h = get_handle(\"{h}\").ok_or_else(|| ffi_err(\"{op}\", \"handle '{h}' not initialized\".to_string()))?;").unwrap();
                writeln!(out, "        let c_a = CString::new(data).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let result = unsafe {{ {fname}(h as *const {h}, c_a.as_ptr()) }};").unwrap();
                writeln!(out, "        if result.is_null() {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        let out = unsafe {{ CStr::from_ptr(result) }}.to_bytes().to_vec();").unwrap();
                writeln!(out, "        unsafe {{ divi_free_string(result) }};").unwrap();
                writeln!(out, "        Ok(out)").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::HandleToI32(ref h) => {
                writeln!(out, "    phone.register_raw(\"{op}\", |_data| {{").unwrap();
                writeln!(out, "        let h = get_handle(\"{h}\").ok_or_else(|| ffi_err(\"{op}\", \"handle '{h}' not initialized\".to_string()))?;").unwrap();
                writeln!(out, "        let code = unsafe {{ {fname}(h as *const {h}) }};").unwrap();
                writeln!(out, "        if code != 0 {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"ok\": true}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::HandleStrToI32(ref h) => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let h = get_handle(\"{h}\").ok_or_else(|| ffi_err(\"{op}\", \"handle '{h}' not initialized\".to_string()))?;").unwrap();
                writeln!(out, "        let c_a = CString::new(data).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let code = unsafe {{ {fname}(h as *const {h}, c_a.as_ptr()) }};").unwrap();
                writeln!(out, "        if code != 0 {{ return Err(ffi_err(\"{op}\", ffi_last_error())); }}").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"ok\": true}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::HandleToBool(ref h) => {
                writeln!(out, "    phone.register_raw(\"{op}\", |_data| {{").unwrap();
                writeln!(out, "        let h = get_handle(\"{h}\").ok_or_else(|| ffi_err(\"{op}\", \"handle '{h}' not initialized\".to_string()))?;").unwrap();
                writeln!(out, "        let val = unsafe {{ {fname}(h as *const {h}) }};").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"result\": val}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::HandleStrToBool(ref h) => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let h = get_handle(\"{h}\").ok_or_else(|| ffi_err(\"{op}\", \"handle '{h}' not initialized\".to_string()))?;").unwrap();
                writeln!(out, "        let c_a = CString::new(data).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        let val = unsafe {{ {fname}(h as *const {h}, c_a.as_ptr()) }};").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"result\": val}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::HandleToVoid(ref h) => {
                writeln!(out, "    phone.register_raw(\"{op}\", |_data| {{").unwrap();
                writeln!(out, "        let h = get_handle(\"{h}\").ok_or_else(|| ffi_err(\"{op}\", \"handle '{h}' not initialized\".to_string()))?;").unwrap();
                writeln!(out, "        unsafe {{ {fname}(h as *const {h}) }};").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"ok\": true}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::HandleStrToVoid(ref h) => {
                writeln!(out, "    phone.register_raw(\"{op}\", |data| {{").unwrap();
                writeln!(out, "        let h = get_handle(\"{h}\").ok_or_else(|| ffi_err(\"{op}\", \"handle '{h}' not initialized\".to_string()))?;").unwrap();
                writeln!(out, "        let c_a = CString::new(data).map_err(|_| ffi_err(\"{op}\", \"null byte\".to_string()))?;").unwrap();
                writeln!(out, "        unsafe {{ {fname}(h as *const {h}, c_a.as_ptr()) }};").unwrap();
                writeln!(out, "        Ok(serde_json::to_vec(&serde_json::json!({{\"ok\": true}})).unwrap())").unwrap();
                writeln!(out, "    }});").unwrap();
            }
            DispatchPattern::Skip => unreachable!(),
        }
    }

    writeln!(out, "    log::info!(\"Registered {{}} FFI ops from C header\", {dispatch_count});").unwrap();
    writeln!(out, "}}").unwrap();

    eprintln!(
        "cargo:warning=FFI ops generated: {dispatch_count} dispatchable, {skip_count} skipped"
    );
}
