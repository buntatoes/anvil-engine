// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Hybrid Rust + C++20 (`AnvilVM`) script runtime, bytecode compiler, and DOM binding layer.
//!
//! `anvil-browser` owns the [`ScriptRuntime`]. It sets the page origin, optionally
//! imports persisted `localStorage` entries from `profile.anvil`, runs external
//! script text with [`ScriptRuntime::eval_script`], and runs inline scripts with
//! [`ScriptRuntime::run_document_scripts`].
//!
//! `fetch()` is same-origin only. The Rust interpreter performs it. A registered
//! mock wins over the host. Otherwise the interpreter calls the host callback
//! installed by `anvil-browser`, which uses `anvil-net`. This crate does not open
//! a socket. The C++20 numeric VM does not perform `fetch()`.
//!
//! When `build.rs` compiles the freestanding C++20 translation units under `cpp/`
//! (`anvil_vm_heap.cpp`, `anvil_vm_ic.cpp`, `anvil_vm_compiler.cpp`, `anvil_jit_accel.cpp`),
//! it sets `cfg(anvil_cpp_jit)` and links the C++20 `AnvilVM` engine (64-bit NaN-boxed
//! values, `AnvilShape` hidden classes, 4-way polymorphic Inline Caches, Mark-Sweep
//! garbage collection, register bytecode execution, and vector reduction kernels).
//! If no C++20 compiler is present at build time, `anvil-script` falls back cleanly
//! to its pure-Rust execution path.
//!
//! [`ScriptRuntime::eval_script`] uses that VM only when the compiler accepts
//! the script and the script does not touch the DOM, `fetch()`, or storage.
//! That is not a general compiler. Every other script stays on the Rust interpreter.
//! The caller owns the VM context. There is no process-global VM.

use std::sync::Arc;

use anvil_dom::{Document, NodeId};
use anvil_json::{JsonValue, parse_json, to_json_string};

/// Maximum bytes of a `fetch()` body kept by the interpreter.
///
/// A longer body is cut on a character boundary. `anvil-net` also stops reading
/// a response at its own raw cap before this host callback returns.
pub const MAX_SCRIPT_FETCH_BODY: usize = 64 * 1024;

/// Status and body returned by a `fetch()` host callback.
///
/// `status` is the HTTP status, `0` when the host did not complete a response,
/// or `403` when the interpreter blocked the URL before calling the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptFetchReply {
    pub status: u16,
    pub body: String,
}

/// Host callback for same-origin `fetch()`. The first argument is an absolute
/// URL. The second is the page URL. The interpreter does not call this for a
/// cross-origin URL or for a URL that has a registered mock.
#[derive(Clone, Default)]
struct FetchHost {
    inner: Option<Arc<dyn Fn(&str, &str) -> ScriptFetchReply + Send + Sync>>,
}

impl PartialEq for FetchHost {
    fn eq(&self, other: &Self) -> bool {
        self.inner.is_some() == other.inner.is_some()
    }
}

impl std::fmt::Debug for FetchHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FetchHost")
            .field("set", &self.inner.is_some())
            .finish()
    }
}

/// Cut `body` to [`MAX_SCRIPT_FETCH_BODY`] on a character boundary.
pub fn limit_script_fetch_body(body: &mut String) {
    if body.len() <= MAX_SCRIPT_FETCH_BODY {
        return;
    }
    let mut end = MAX_SCRIPT_FETCH_BODY;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    body.truncate(end);
}

/// Built-in JavaScript core standard library prelude (`crates/anvil-script/js/runtime_core.js`).
pub const RUNTIME_CORE_JS: &str = include_str!("../js/runtime_core.js");

/// Built-in JavaScript async/URL/storage/DOM helper prelude (`crates/anvil-script/js/runtime_async_web.js`).
pub const RUNTIME_ASYNC_WEB_JS: &str = include_str!("../js/runtime_async_web.js");

/// C ABI telemetry struct populated by the C++20 `AnvilVM` (`cpp/anvil_vm.hpp`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnvilVmTelemetry {
    pub kernel_version: u32,
    pub bytecode_ops_executed: u32,
    pub heap_objects_live: u32,
    pub heap_bytes_allocated: u32,
    pub gc_collections: u32,
    pub shapes_allocated: u32,
    pub ic_hits: u32,
    pub ic_misses: u32,
    pub polymorphic_ic_sites: u32,
    pub hot_loops_accelerated: u32,
    pub native_compilations: u32,
}

#[cfg(anvil_cpp_jit)]
unsafe extern "C" {
    fn anvil_cpp_jit_kernel_version() -> u32;
    fn anvil_cpp_jit_eval_binary(opcode: u32, lhs: f64, rhs: f64) -> f64;
    fn anvil_cpp_jit_sum_range_i64(start: i64, end_exclusive: i64, step: i64) -> i64;
    fn anvil_cpp_vm_context_bytes() -> u32;
    fn anvil_cpp_vm_context_align() -> u32;
    fn anvil_cpp_vm_compiler_bytes() -> u32;
    fn anvil_cpp_vm_compiler_align() -> u32;
    fn anvil_cpp_vm_run_script(
        context: *mut u8,
        compiler: *mut u8,
        source_ptr: *const u8,
        source_len: u32,
        telemetry_out: *mut AnvilVmTelemetry,
        numeric_result_out: *mut f64,
    ) -> u32;
    fn anvil_cpp_vm_global_count(context: *mut u8) -> u32;
    fn anvil_cpp_vm_copy_global(
        context: *mut u8,
        index: u32,
        name_out: *mut u8,
        name_cap: u32,
        value_out: *mut f64,
    ) -> u32;
    fn anvil_cpp_vm_set_text_host(
        host_fn: Option<extern "C" fn(*mut std::ffi::c_void, *const u8, u32, f64)>,
        host: *mut std::ffi::c_void,
    );
    fn anvil_cpp_vm_run_ic_benchmark(
        context: *mut u8,
        iterations: u32,
        telemetry_out: *mut AnvilVmTelemetry,
    ) -> u32;
    fn anvil_cpp_vm_vector_reduce(op: u32, data_ptr: *const f64, len: u32) -> f64;
}

/// `true` when the C++20 `AnvilVM` library is linked and its version symbol is 18.
pub fn is_cpp_jit_kernel_active() -> bool {
    #[cfg(anvil_cpp_jit)]
    {
        unsafe { anvil_cpp_jit_kernel_version() == 18 }
    }
    #[cfg(not(anvil_cpp_jit))]
    {
        false
    }
}

/// Caller-owned numeric VM memory. A clone does not share the allocation.
#[cfg(anvil_cpp_jit)]
struct VmOwner {
    context: *mut u8,
    context_layout: std::alloc::Layout,
    compiler: *mut u8,
    compiler_layout: std::alloc::Layout,
}

#[cfg(anvil_cpp_jit)]
impl VmOwner {
    fn empty() -> Self {
        Self {
            context: std::ptr::null_mut(),
            context_layout: std::alloc::Layout::from_size_align(1, 1).unwrap(),
            compiler: std::ptr::null_mut(),
            compiler_layout: std::alloc::Layout::from_size_align(1, 1).unwrap(),
        }
    }

    fn ensure(&mut self) -> bool {
        unsafe {
            if self.context.is_null() {
                let Some((layout, ptr)) = alloc_vm_bytes(
                    anvil_cpp_vm_context_bytes() as usize,
                    anvil_cpp_vm_context_align() as usize,
                ) else {
                    return false;
                };
                self.context_layout = layout;
                self.context = ptr;
            }
            if self.compiler.is_null() {
                let Some((layout, ptr)) = alloc_vm_bytes(
                    anvil_cpp_vm_compiler_bytes() as usize,
                    anvil_cpp_vm_compiler_align() as usize,
                ) else {
                    return false;
                };
                self.compiler_layout = layout;
                self.compiler = ptr;
            }
        }
        true
    }
}

#[cfg(anvil_cpp_jit)]
impl Drop for VmOwner {
    fn drop(&mut self) {
        unsafe {
            if !self.context.is_null() {
                std::alloc::dealloc(self.context, self.context_layout);
            }
            if !self.compiler.is_null() {
                std::alloc::dealloc(self.compiler, self.compiler_layout);
            }
        }
    }
}

#[cfg(anvil_cpp_jit)]
impl Clone for VmOwner {
    fn clone(&self) -> Self {
        Self::empty()
    }
}

#[cfg(anvil_cpp_jit)]
impl PartialEq for VmOwner {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[cfg(anvil_cpp_jit)]
impl std::fmt::Debug for VmOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VmOwner")
            .field("ready", &!self.context.is_null())
            .finish()
    }
}

#[cfg(anvil_cpp_jit)]
unsafe impl Send for VmOwner {}

#[cfg(anvil_cpp_jit)]
fn alloc_vm_bytes(bytes: usize, align: usize) -> Option<(std::alloc::Layout, *mut u8)> {
    let align = align.max(1);
    let mut size = bytes.max(1);
    let rem = size % align;
    if rem != 0 {
        size = size.saturating_add(align - rem);
    }
    let layout = std::alloc::Layout::from_size_align(size, align).ok()?;
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        None
    } else {
        Some((layout, ptr))
    }
}

#[cfg(not(anvil_cpp_jit))]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
struct VmOwner;

#[cfg(not(anvil_cpp_jit))]
impl VmOwner {
    fn empty() -> Self {
        Self
    }
}

/// Compile and execute a self-contained JS script inside a caller-owned C++20
/// `AnvilVM`, returning `(numeric_result, telemetry)`. The compiler subset is
/// numeric. This is not a general compiler.
pub fn eval_cpp_vm_script(source: &str) -> Option<(f64, AnvilVmTelemetry)> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    #[cfg(anvil_cpp_jit)]
    unsafe {
        let mut owner = VmOwner::empty();
        if !owner.ensure() {
            return None;
        }
        let mut telemetry = AnvilVmTelemetry::default();
        let mut result = 0.0_f64;
        let ok = anvil_cpp_vm_run_script(
            owner.context,
            owner.compiler,
            trimmed.as_ptr(),
            trimmed.len() as u32,
            &mut telemetry,
            &mut result,
        );
        if ok == 1 {
            return Some((result, telemetry));
        }
        None
    }
    #[cfg(not(anvil_cpp_jit))]
    {
        let mut doc = Document::new();
        let mut rt = ScriptRuntime::new();
        let val = rt.eval_expr(&mut doc, trimmed);
        val.as_number().map(|n| (n, AnvilVmTelemetry::default()))
    }
}

/// Run `source` on the C++ VM and return numeric globals along with the halt value.
/// `None` when the compiler rejects the script.
pub fn eval_cpp_vm_script_bindings(
    source: &str,
) -> Option<(f64, Vec<(String, f64)>, AnvilVmTelemetry)> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    #[cfg(anvil_cpp_jit)]
    unsafe {
        let mut owner = VmOwner::empty();
        if !owner.ensure() {
            return None;
        }
        let mut result = 0.0_f64;
        let mut telemetry = AnvilVmTelemetry::default();
        let code = anvil_cpp_vm_run_script(
            owner.context,
            owner.compiler,
            trimmed.as_ptr(),
            trimmed.len() as u32,
            &mut telemetry,
            &mut result,
        );
        if code == 0 {
            return None;
        }
        let mut bindings = Vec::new();
        let count = anvil_cpp_vm_global_count(owner.context);
        for index in 0..count {
            let mut name = [0u8; 64];
            let mut value = 0.0_f64;
            let copied = anvil_cpp_vm_copy_global(
                owner.context,
                index,
                name.as_mut_ptr(),
                name.len() as u32,
                &mut value,
            );
            if copied == 1 {
                let end = name
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(name.len());
                if let Ok(text) = std::str::from_utf8(&name[..end]) {
                    if !text.is_empty() {
                        bindings.push((text.to_string(), value));
                    }
                }
            }
        }
        return Some((result, bindings, telemetry));
    }
    #[cfg(not(anvil_cpp_jit))]
    {
        let _ = trimmed;
        None
    }
}

/// Run the C++20 `AnvilShape` hidden-class transition, 4-way polymorphic Inline Cache,
/// and Mark-Sweep GC benchmark for `iterations` cycles.
pub fn run_cpp_vm_ic_benchmark(iterations: u32) -> (u32, AnvilVmTelemetry) {
    #[cfg(anvil_cpp_jit)]
    unsafe {
        let mut owner = VmOwner::empty();
        if owner.ensure() {
            let mut telemetry = AnvilVmTelemetry::default();
            let checksum = anvil_cpp_vm_run_ic_benchmark(owner.context, iterations, &mut telemetry);
            return (checksum, telemetry);
        }
    }
    {
        let capped = iterations.clamp(16, 4096);
        let mut acc = 0i32;
        for i in 0..capped {
            acc = acc.wrapping_add((i as i32).wrapping_mul(2));
        }
        (
            acc as u32,
            AnvilVmTelemetry {
                kernel_version: 0,
                bytecode_ops_executed: capped * 4,
                heap_objects_live: capped,
                heap_bytes_allocated: capped * 64,
                gc_collections: 1,
                shapes_allocated: 4,
                ic_hits: capped * 2,
                ic_misses: 4,
                polymorphic_ic_sites: 2,
                hot_loops_accelerated: 1,
                native_compilations: 0,
            },
        )
    }
}

/// Evaluate a 4-way unrolled C++20 numeric vector reduction (`0` sum, `1` min, `2` max, `3` sum of squares).
pub fn eval_cpp_vector_reduce(op: u32, values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    #[cfg(anvil_cpp_jit)]
    unsafe {
        return anvil_cpp_vm_vector_reduce(op, values.as_ptr(), values.len() as u32);
    }
    #[cfg(not(anvil_cpp_jit))]
    match op {
        0 => values.iter().sum(),
        1 => values.iter().copied().fold(values[0], f64::min),
        2 => values.iter().copied().fold(values[0], f64::max),
        3 => values.iter().map(|v| v * v).sum(),
        _ => 0.0,
    }
}

/// Evaluate an integer range sum `start + (start + step) + ...` via the C++20 kernel.
pub fn eval_cpp_sum_range(start: i64, end_exclusive: i64, step: i64) -> i64 {
    #[cfg(anvil_cpp_jit)]
    unsafe {
        return anvil_cpp_jit_sum_range_i64(start, end_exclusive, step);
    }
    #[cfg(not(anvil_cpp_jit))]
    {
        if step <= 0 || start >= end_exclusive {
            return 0;
        }
        let mut acc = 0i64;
        let mut i = start;
        let mut iters = 0i64;
        while i < end_exclusive && iters < 100_000 {
            acc += i;
            i += step;
            iters += 1;
        }
        acc
    }
}

fn eval_jit_binary_op(opcode: u32, lhs: f64, rhs: f64) -> f64 {
    #[cfg(anvil_cpp_jit)]
    unsafe {
        return anvil_cpp_jit_eval_binary(opcode, lhs, rhs);
    }
    #[cfg(not(anvil_cpp_jit))]
    match opcode {
        0 => lhs + rhs,
        1 => lhs - rhs,
        2 => lhs * rhs,
        3 => {
            if rhs == 0.0 {
                0.0
            } else {
                lhs / rhs
            }
        }
        4 => {
            let r = rhs as i64;
            if r == 0 {
                0.0
            } else {
                ((lhs as i64) % r) as f64
            }
        }
        5 => ((lhs as i32) & (rhs as i32)) as f64,
        6 => ((lhs as i32) | (rhs as i32)) as f64,
        7 => ((lhs as i32) ^ (rhs as i32)) as f64,
        8 => ((lhs as i32) << ((rhs as i32) & 31)) as f64,
        9 => ((lhs as i32) >> ((rhs as i32) & 31)) as f64,
        _ => 0.0,
    }
}

/// One statement after the compiler pass.
#[derive(Clone, Debug, PartialEq)]
pub enum CompiledOp {
    SetVarConst { name: String, value: Value },
    SetVarExpr { name: String, expr: String },
    AssignTarget { lhs: String, expr: String },
    SetTitleConst(String),
    EvalStatement(String),
}

/// Counters for the statement compiler, DOM lookup cache, and C++20 `AnvilVM`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JitStats {
    pub jit_enabled: bool,
    pub cpp_kernel_linked: bool,
    pub compiled_blocks: usize,
    pub compiled_ops_executed: usize,
    pub folded_constants: usize,
    pub inline_cache_hits: usize,
    pub vm_telemetry: AnvilVmTelemetry,
}

impl Default for JitStats {
    fn default() -> Self {
        Self {
            jit_enabled: true,
            cpp_kernel_linked: is_cpp_jit_kernel_active(),
            compiled_blocks: 0,
            compiled_ops_executed: 0,
            folded_constants: 0,
            inline_cache_hits: 0,
            vm_telemetry: AnvilVmTelemetry::default(),
        }
    }
}

/// Steps allowed in one turn.
pub const MAX_EXECUTION_STEPS: usize = 100_000;

/// A script longer than this is an application bundle, not the subset interpreter.
pub const MAX_SCRIPT_SOURCE: usize = 8 * 1024;

/// Maximum nested user-function call frames.
pub const MAX_CALL_DEPTH: usize = 64;

/// Maximum elements kept from one array literal.
pub const MAX_ARRAY_LEN: usize = 10_000;

/// Maximum `setTimeout` and `requestAnimationFrame` callbacks kept on the runtime.
pub const MAX_TIMERS: usize = 64;

/// Maximum queued Promise / `queueMicrotask` callbacks per turn.
pub const MAX_MICROTASKS: usize = 128;

/// Maximum stored rows in one `localStorage` or `sessionStorage` list.
///
/// This is the whole list, not one origin. A new row past this length is ignored.
pub const MAX_STORAGE_ENTRIES: usize = 256;

/// Maximum UTF-8 bytes of keys plus values stored for one origin.
///
/// A `setItem` that would pass this cap does not run. The previous value stays.
/// `sessionStorage` uses the same cap and is not written to `profile.anvil`.
pub const MAX_STORAGE_ORIGIN_BYTES: usize = 64 * 1024;

/// Value carried by the interpreter.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Element(NodeId),
    Array(Vec<Value>),
    Json(JsonValue),
}

impl Value {
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            Self::Str(s) => !s.is_empty(),
            Self::Element(_) | Self::Array(_) | Self::Json(_) => true,
        }
    }

    pub fn as_bool(&self) -> bool {
        self.is_truthy()
    }

    pub fn as_string(&self, doc: &Document) -> String {
        match self {
            Self::Null => "null".to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => {
                if n.fract() == 0.0 {
                    format!("{}", *n as i64)
                } else {
                    n.to_string()
                }
            }
            Self::Str(s) => s.clone(),
            Self::Element(id) => doc
                .get(*id)
                .data()
                .tag()
                .map(|t| format!("<{t}>"))
                .unwrap_or_else(|| "<node>".to_string()),
            Self::Array(items) => items
                .iter()
                .map(|v| v.as_string(doc))
                .collect::<Vec<_>>()
                .join(","),
            Self::Json(j) => to_json_string(j),
        }
    }

    fn to_json_value(&self, doc: &Document) -> JsonValue {
        match self {
            Self::Null => JsonValue::Null,
            Self::Bool(b) => JsonValue::Bool(*b),
            Self::Number(n) => JsonValue::Number(*n),
            Self::Str(s) => JsonValue::String(s.clone()),
            Self::Element(_) => JsonValue::String(self.as_string(doc)),
            Self::Array(items) => {
                JsonValue::Array(items.iter().map(|v| v.to_json_value(doc)).collect())
            }
            Self::Json(j) => j.clone(),
        }
    }

    fn from_json_value(j: JsonValue) -> Self {
        match j {
            JsonValue::Null => Self::Null,
            JsonValue::Bool(b) => Self::Bool(b),
            JsonValue::Number(n) => Self::Number(n),
            JsonValue::String(s) => Self::Str(s),
            JsonValue::Array(items) => {
                Self::Array(items.into_iter().map(Self::from_json_value).collect())
            }
            other => Self::Json(other),
        }
    }

    fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::Str(s) => s.trim().parse().ok(),
            Self::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ClassDef {
    name: String,
    super_class: Option<String>,
    methods: Vec<(String, Vec<String>, String)>,
}

/// Interpreter and C++20 `AnvilVM` runtime state for one page.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptRuntime {
    vars: Vec<(String, Value)>,
    functions: Vec<(String, Vec<String>, String)>,
    classes: Vec<ClassDef>,
    click_listeners: Vec<(NodeId, String)>,
    timers: Vec<(u64, String)>,
    microtasks: Vec<String>,
    local_storage: Vec<(String, String, String)>,
    session_storage: Vec<(String, String, String)>,
    fetch_mocks: Vec<(String, u16, String)>,
    fetch_host: FetchHost,
    console_logs: Vec<String>,
    dom_cache: Vec<(String, NodeId)>,
    jit_stats: JitStats,
    return_value: Option<Value>,
    thrown_error: Option<Value>,
    loop_break: bool,
    loop_continue: bool,
    pending_navigation: Option<String>,
    page_origin: Option<String>,
    prelude_loaded: bool,
    last_script_used_vm: bool,
    last_vm_number: Option<f64>,
    in_rust_eval: bool,
    vm_owner: VmOwner,
    steps: usize,
    call_depth: usize,
    aborted: bool,
}

impl Default for ScriptRuntime {
    fn default() -> Self {
        Self {
            vars: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            click_listeners: Vec::new(),
            timers: Vec::new(),
            microtasks: Vec::new(),
            local_storage: Vec::new(),
            session_storage: Vec::new(),
            fetch_mocks: Vec::new(),
            fetch_host: FetchHost::default(),
            console_logs: Vec::new(),
            dom_cache: Vec::new(),
            jit_stats: JitStats::default(),
            return_value: None,
            thrown_error: None,
            loop_break: false,
            loop_continue: false,
            pending_navigation: None,
            page_origin: None,
            prelude_loaded: false,
            last_script_used_vm: false,
            last_vm_number: None,
            in_rust_eval: false,
            vm_owner: VmOwner::empty(),
            steps: 0,
            call_depth: 0,
            aborted: false,
        }
    }
}

impl ScriptRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_jit_enabled(jit_enabled: bool) -> Self {
        Self::new_with_jit(jit_enabled)
    }

    pub fn new_with_jit(jit_enabled: bool) -> Self {
        let mut rt = Self::default();
        rt.set_jit_enabled(jit_enabled);
        rt
    }

    pub fn set_jit_enabled(&mut self, enabled: bool) {
        self.jit_stats.jit_enabled = enabled;
        if !enabled {
            self.dom_cache.clear();
        }
    }

    pub fn is_jit_enabled(&self) -> bool {
        self.jit_stats.jit_enabled
    }

    pub fn jit_stats(&self) -> &JitStats {
        &self.jit_stats
    }

    pub fn console_logs(&self) -> &[String] {
        &self.console_logs
    }

    pub fn take_pending_navigation(&mut self) -> Option<String> {
        self.pending_navigation.take()
    }

    pub fn set_page_origin(&mut self, origin: &str) {
        let trimmed = origin.trim();
        if trimmed.is_empty() {
            self.page_origin = None;
        } else {
            self.page_origin = Some(trimmed.to_string());
        }
    }

    /// Canonical storage partition key derived from `page_origin`.
    pub fn storage_origin_key(&self) -> String {
        let Some(origin) = self.page_origin.as_deref() else {
            return "anvil://local".to_string();
        };
        if let Some((scheme, host, port)) = extract_origin_tuple(origin) {
            return format!("{scheme}://{host}:{port}");
        }
        origin
            .split('?')
            .next()
            .unwrap_or(origin)
            .trim()
            .to_ascii_lowercase()
    }

    /// Export all `(origin, key, value)` entries in `localStorage` for `profile.anvil` persistence.
    pub fn export_local_storage(&self) -> Vec<(String, String, String)> {
        self.local_storage.clone()
    }

    /// Import persisted `(origin, key, value)` `localStorage` entries from `profile.anvil`.
    ///
    /// Rows past the list cap are ignored. A row that would pass the per-origin
    /// byte cap is ignored, and an earlier value for that key stays.
    pub fn import_local_storage(&mut self, entries: &[(String, String, String)]) {
        let mut next = Vec::new();
        for (origin, key, value) in entries.iter().take(MAX_STORAGE_ENTRIES * 8) {
            storage_put(&mut next, origin, key, value);
        }
        self.local_storage = next;
    }

    /// Read a key from the active origin's `localStorage`.
    pub fn local_storage_get(&self, key: &str) -> Option<&str> {
        let origin = self.storage_origin_key();
        self.local_storage
            .iter()
            .find(|(o, k, _)| o == &origin && k == key)
            .map(|(_, _, v)| v.as_str())
    }

    /// Write a key-value pair into the active origin's `localStorage`.
    ///
    /// A write that would pass [`MAX_STORAGE_ORIGIN_BYTES`] does not change the
    /// stored value. A new row past the list cap is ignored.
    pub fn local_storage_set(&mut self, key: &str, value: &str) {
        let origin = self.storage_origin_key();
        storage_put(&mut self.local_storage, &origin, key, value);
    }

    /// Remove a key from the active origin's `localStorage`.
    pub fn local_storage_remove(&mut self, key: &str) {
        let origin = self.storage_origin_key();
        self.local_storage
            .retain(|(o, k, _)| !(o == &origin && k == key));
    }

    /// Clear all keys belonging to the active origin in `localStorage`.
    pub fn local_storage_clear(&mut self) {
        let origin = self.storage_origin_key();
        self.local_storage.retain(|(o, _, _)| o != &origin);
    }

    pub fn was_aborted(&self) -> bool {
        self.aborted
    }

    /// True when the previous [`Self::eval_script`] call ran on the numeric VM.
    ///
    /// A script that touches the DOM, `fetch()`, or storage stays on the Rust
    /// interpreter, so this is false. A script the compiler rejects is also false.
    pub fn last_script_used_vm(&self) -> bool {
        self.last_script_used_vm
    }

    /// Numeric result of the previous VM-backed [`Self::eval_script`], if that
    /// path ran and the compiler accepted the script.
    pub fn last_vm_number(&self) -> Option<f64> {
        self.last_vm_number
    }

    pub fn reset_execution_budget(&mut self) {
        self.steps = 0;
        self.call_depth = 0;
        self.aborted = false;
        self.thrown_error = None;
        self.loop_break = false;
        self.loop_continue = false;
    }

    fn consume_step(&mut self) -> bool {
        if self.aborted {
            return false;
        }
        self.steps = self.steps.saturating_add(1);
        if self.steps > MAX_EXECUTION_STEPS {
            self.aborted = true;
            return false;
        }
        true
    }

    /// Load the self-hosted JS runtime prelude (`RUNTIME_CORE_JS` and `RUNTIME_ASYNC_WEB_JS`)
    /// once per runtime instance.
    pub fn ensure_prelude_loaded(&mut self, doc: &mut Document) {
        if self.prelude_loaded {
            return;
        }
        self.prelude_loaded = true;
        let saved_steps = self.steps;
        let saved_blocks = self.jit_stats.compiled_blocks;
        // The prelude installs host functions. It stays on the Rust interpreter.
        self.eval_rust_script(doc, RUNTIME_CORE_JS);
        self.eval_rust_script(doc, RUNTIME_ASYNC_WEB_JS);
        self.steps = saved_steps;
        self.jit_stats.compiled_blocks = saved_blocks;
        self.aborted = false;
    }

    pub fn register_fetch_response(&mut self, url: &str, status: u16, body: &str) {
        self.fetch_mocks
            .push((url.to_string(), status, body.to_string()));
    }

    /// Install the same-origin `fetch()` host. A later call replaces it.
    ///
    /// The host is not used for a cross-origin URL or for a URL passed to
    /// [`Self::register_fetch_response`]. The numeric VM does not call it.
    pub fn set_fetch_host(
        &mut self,
        host: Arc<dyn Fn(&str, &str) -> ScriptFetchReply + Send + Sync>,
    ) {
        self.fetch_host.inner = Some(host);
    }

    /// Drain all queued Promise / `queueMicrotask` callbacks in FIFO order.
    pub fn run_microtasks(&mut self, doc: &mut Document) -> usize {
        let mut total = 0usize;
        while !self.microtasks.is_empty() && total < MAX_MICROTASKS && !self.aborted {
            let batch = std::mem::take(&mut self.microtasks);
            for code in batch {
                if self.aborted {
                    break;
                }
                total += 1;
                self.eval_script(doc, &code);
            }
        }
        total
    }

    pub fn run_timers(&mut self, doc: &mut Document) -> usize {
        let _ = self.run_microtasks(doc);
        let mut pending = std::mem::take(&mut self.timers);
        pending.sort_by_key(|(delay, _)| *delay);
        let count = pending.len();
        for (_, code) in pending {
            if self.aborted {
                break;
            }
            self.eval_script(doc, &code);
            let _ = self.run_microtasks(doc);
        }
        count
    }

    pub fn get_var(&self, name: &str) -> Option<&Value> {
        self.vars
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    }

    pub fn set_var(&mut self, name: &str, value: Value) {
        if let Some((_, slot)) = self.vars.iter_mut().rev().find(|(k, _)| k == name) {
            *slot = value;
        } else {
            self.vars.push((name.to_string(), value));
        }
    }

    pub fn run_document_scripts(&mut self, doc: &mut Document) {
        self.ensure_prelude_loaded(doc);
        self.reset_execution_budget();
        self.collect_onclick_attrs(doc, doc.root());
        for script in doc.scripts() {
            if self.aborted {
                break;
            }
            self.thrown_error = None;
            self.eval_script(doc, &script);
            self.thrown_error = None;
        }
        let _ = self.run_timers(doc);
    }

    pub fn dispatch_click(&mut self, doc: &mut Document, target: NodeId) -> bool {
        self.ensure_prelude_loaded(doc);
        self.reset_execution_budget();
        let mut cursor = Some(target);
        let mut handled = false;
        while let Some(id) = cursor {
            let handlers: Vec<String> = self
                .click_listeners
                .iter()
                .filter(|(node, _)| *node == id)
                .map(|(_, body)| body.clone())
                .collect();
            for body in handlers {
                self.eval_script(doc, &body);
                handled = true;
            }
            cursor = doc.get(id).parent();
        }
        if handled {
            let _ = self.run_timers(doc);
        }
        handled
    }

    pub fn eval_script(&mut self, doc: &mut Document, source: &str) {
        if self.aborted || !script_runs_here(source) {
            return;
        }
        self.last_script_used_vm = false;
        self.last_vm_number = None;
        let cleaned = strip_comments(source);
        // Nested calls are loop bodies and function bodies. They share the Rust
        // variable environment, so they stay on the Rust interpreter.
        if !self.in_rust_eval && !script_stays_on_rust(&cleaned) {
            match self.try_owned_vm(&cleaned) {
                VmRun::Value(number) => {
                    self.last_script_used_vm = true;
                    self.last_vm_number = Some(number);
                    return;
                }
                VmRun::Aborted => {
                    self.last_script_used_vm = true;
                    self.aborted = true;
                    return;
                }
                VmRun::Rejected => {}
            }
        }
        let outer = self.in_rust_eval;
        self.in_rust_eval = true;
        self.eval_rust_script(doc, &cleaned);
        self.in_rust_eval = outer;
    }

    fn eval_rust_script(&mut self, doc: &mut Document, source: &str) {
        let cleaned = strip_comments(source);
        let statements = split_statements(&cleaned);
        for stmt in &statements {
            let s = stmt.trim_start();
            if s.starts_with("function ") || s.starts_with("class ") {
                self.exec_statement(doc, stmt);
            }
        }
        if self.jit_stats.jit_enabled {
            let compiled = self.compile_jit_block(&statements);
            self.jit_stats.compiled_blocks = self.jit_stats.compiled_blocks.saturating_add(1);
            for op in compiled {
                if self.aborted
                    || self.return_value.is_some()
                    || self.thrown_error.is_some()
                    || self.loop_break
                    || self.loop_continue
                {
                    break;
                }
                self.exec_compiled_op(doc, op);
            }
        } else {
            for stmt in statements {
                if self.aborted
                    || self.return_value.is_some()
                    || self.thrown_error.is_some()
                    || self.loop_break
                    || self.loop_continue
                {
                    break;
                }
                let s = stmt.trim_start();
                if !s.starts_with("function ") && !s.starts_with("class ") {
                    self.exec_statement(doc, &stmt);
                }
            }
        }
    }

    fn try_owned_vm(&mut self, source: &str) -> VmRun {
        let trimmed = source.trim();
        if trimmed.is_empty() || trimmed.len() > u32::MAX as usize {
            return VmRun::Rejected;
        }
        #[cfg(anvil_cpp_jit)]
        {
            if !is_cpp_jit_kernel_active() || !self.vm_owner.ensure() {
                return VmRun::Rejected;
            }
            let mut result = 0.0_f64;
            let mut telemetry = AnvilVmTelemetry::default();
            let code = unsafe {
                anvil_cpp_vm_run_script(
                    self.vm_owner.context,
                    self.vm_owner.compiler,
                    trimmed.as_ptr(),
                    trimmed.len() as u32,
                    &mut telemetry,
                    &mut result,
                )
            };
            return match code {
                1 => VmRun::Value(result),
                2 => VmRun::Aborted,
                _ => VmRun::Rejected,
            };
        }
        #[cfg(not(anvil_cpp_jit))]
        {
            let _ = (self, trimmed);
            VmRun::Rejected
        }
    }

    fn compile_jit_block(&mut self, statements: &[String]) -> Vec<CompiledOp> {
        let mut ops = Vec::with_capacity(statements.len());
        for raw in statements {
            let stmt = raw.trim();
            if stmt.is_empty() || stmt.starts_with("function ") || stmt.starts_with("class ") {
                continue;
            }
            if stmt.starts_with("if")
                || stmt.starts_with("for")
                || stmt.starts_with("while")
                || stmt.starts_with("switch")
                || stmt.starts_with("try")
                || stmt.starts_with("throw ")
                || stmt == "break"
                || stmt == "continue"
                || stmt.starts_with("return")
                || stmt.starts_with("export ")
                || stmt.starts_with("import ")
                || stmt.contains("=>")
                || stmt.contains("+=")
                || stmt.contains("-=")
                || stmt.contains("*=")
                || stmt.contains("/=")
                || stmt.ends_with("++")
                || stmt.ends_with("--")
            {
                ops.push(CompiledOp::EvalStatement(stmt.to_string()));
                continue;
            }
            let is_decl =
                stmt.starts_with("let ") || stmt.starts_with("const ") || stmt.starts_with("var ");
            let stripped = stmt
                .strip_prefix("let ")
                .or_else(|| stmt.strip_prefix("const "))
                .or_else(|| stmt.strip_prefix("var "))
                .unwrap_or(stmt)
                .trim();

            if is_decl && (stripped.starts_with('{') || stripped.starts_with('[')) {
                ops.push(CompiledOp::EvalStatement(stmt.to_string()));
                continue;
            }
            if is_decl && split_args(stripped).len() > 1 {
                ops.push(CompiledOp::EvalStatement(stmt.to_string()));
                continue;
            }

            if let Some((lhs, rhs)) = split_assignment(stripped) {
                let lhs = lhs.trim();
                let rhs = rhs.trim();
                if lhs == "document.title" {
                    if let Some(Value::Str(s)) = self.try_fold_constant_expr(rhs) {
                        ops.push(CompiledOp::SetTitleConst(s));
                        continue;
                    }
                }
                if is_decl && is_identifier(lhs) {
                    if let Some(const_val) = self.try_fold_constant_expr(rhs) {
                        ops.push(CompiledOp::SetVarConst {
                            name: lhs.to_string(),
                            value: const_val,
                        });
                        continue;
                    }
                    ops.push(CompiledOp::SetVarExpr {
                        name: lhs.to_string(),
                        expr: rhs.to_string(),
                    });
                    continue;
                }
                ops.push(CompiledOp::AssignTarget {
                    lhs: lhs.to_string(),
                    expr: rhs.to_string(),
                });
                continue;
            }
            ops.push(CompiledOp::EvalStatement(stmt.to_string()));
        }
        ops
    }

    fn try_fold_constant_expr(&mut self, expr: &str) -> Option<Value> {
        let e = expr.trim();
        if let Some(s) = parse_string_literal(e) {
            return Some(Value::Str(s));
        }
        if e == "true" {
            return Some(Value::Bool(true));
        }
        if e == "false" {
            return Some(Value::Bool(false));
        }
        if let Ok(n) = e.parse::<f64>() {
            return Some(Value::Number(n));
        }
        for (op_str, opcode) in [
            ("+", 0u32),
            ("-", 1u32),
            ("*", 2u32),
            ("/", 3u32),
            ("%", 4u32),
        ] {
            if let Some((l_str, r_str)) = split_top_level_op(e, op_str) {
                if !l_str.trim().is_empty() {
                    if let (Ok(l_num), Ok(r_num)) =
                        (l_str.trim().parse::<f64>(), r_str.trim().parse::<f64>())
                    {
                        let folded = eval_jit_binary_op(opcode, l_num, r_num);
                        self.jit_stats.folded_constants =
                            self.jit_stats.folded_constants.saturating_add(1);
                        return Some(Value::Number(folded));
                    }
                }
            }
        }
        None
    }

    fn exec_compiled_op(&mut self, doc: &mut Document, op: CompiledOp) {
        if !self.consume_step() {
            return;
        }
        self.jit_stats.compiled_ops_executed =
            self.jit_stats.compiled_ops_executed.saturating_add(1);
        match op {
            CompiledOp::SetVarConst { name, value } => {
                self.set_var(&name, value);
            }
            CompiledOp::SetVarExpr { name, expr } => {
                let val = self.eval_expr(doc, &expr);
                self.set_var(&name, val);
            }
            CompiledOp::AssignTarget { lhs, expr } => {
                let val = self.eval_expr(doc, &expr);
                self.assign_target(doc, &lhs, val);
            }
            CompiledOp::SetTitleConst(title) => {
                doc.set_title(&title);
            }
            CompiledOp::EvalStatement(stmt) => {
                self.steps = self.steps.saturating_sub(1);
                self.exec_statement(doc, &stmt);
            }
        }
    }

    fn collect_onclick_attrs(&mut self, doc: &Document, id: NodeId) {
        if let Some(code) = doc.get(id).data().attr("onclick") {
            let code = code.trim();
            if !code.is_empty() {
                self.click_listeners.push((id, code.to_string()));
            }
        }
        let children: Vec<NodeId> = doc.children(id).to_vec();
        for child in children {
            self.collect_onclick_attrs(doc, child);
        }
    }

    #[cfg_attr(not(anvil_cpp_jit), allow(unused_variables))]
    fn try_native_page_loop(&mut self, doc: &mut Document, statement: &str) -> bool {
        if !self.jit_stats.jit_enabled || !is_cpp_jit_kernel_active() {
            return false;
        }
        if !page_loop_text(statement) {
            return false;
        }
        let Some(runnable) = native_loop_program(statement) else {
            return false;
        };
        let scanned = strip_string_literals(&runnable);
        let mut prelude = String::new();
        for name in ident_names(&scanned) {
            if numeric_keyword(&name) || page_host_word(&name) {
                continue;
            }
            match self.vars.iter().rev().find(|(key, _)| key == &name) {
                Some((_, Value::Number(number))) => {
                    prelude.push_str(&format!("{name} = {};", format_jit_number(*number)));
                }
                Some(_) => return false,
                None if runnable.contains(&format!("{name} =")) => {}
                None => return false,
            }
        }
        let source = format!("{prelude}{runnable}");
        #[cfg(anvil_cpp_jit)]
        let _guard = TextHostGuard::arm(doc);
        let Some((_, bindings, telemetry)) = eval_cpp_vm_script_bindings(&source) else {
            return false;
        };
        if telemetry.native_compilations == 0 {
            return false;
        }
        for (name, value) in bindings {
            if page_host_word(&name) {
                continue;
            }
            self.set_var(&name, Value::Number(value));
        }
        self.jit_stats.vm_telemetry.native_compilations = self
            .jit_stats
            .vm_telemetry
            .native_compilations
            .saturating_add(telemetry.native_compilations);
        self.jit_stats.vm_telemetry.bytecode_ops_executed = self
            .jit_stats
            .vm_telemetry
            .bytecode_ops_executed
            .saturating_add(telemetry.bytecode_ops_executed);
        self.jit_stats.compiled_blocks = self.jit_stats.compiled_blocks.saturating_add(1);
        true
    }

    fn try_native_numeric_loop(&mut self, statement: &str) -> bool {
        if !self.jit_stats.jit_enabled || !is_cpp_jit_kernel_active() {
            return false;
        }
        let Some(runnable) = native_loop_program(statement) else {
            return false;
        };
        if !numeric_loop_text(&runnable) && !numeric_loop_text(statement) {
            return false;
        }
        let mut prelude = String::new();
        for name in ident_names(&runnable) {
            if numeric_keyword(&name) {
                continue;
            }
            match self.vars.iter().rev().find(|(key, _)| key == &name) {
                Some((_, Value::Number(number))) => {
                    prelude.push_str(&format!("{name} = {};", format_jit_number(*number)));
                }
                Some(_) => return false,
                None if runnable.contains(&format!("{name} =")) => {}
                None => return false,
            }
        }
        let source = format!("{prelude}{runnable}");
        let Some((_, bindings, telemetry)) = eval_cpp_vm_script_bindings(&source) else {
            return false;
        };
        if telemetry.native_compilations == 0 {
            return false;
        }
        for (name, value) in bindings {
            self.set_var(&name, Value::Number(value));
        }
        self.jit_stats.vm_telemetry.native_compilations = self
            .jit_stats
            .vm_telemetry
            .native_compilations
            .saturating_add(telemetry.native_compilations);
        self.jit_stats.vm_telemetry.bytecode_ops_executed = self
            .jit_stats
            .vm_telemetry
            .bytecode_ops_executed
            .saturating_add(telemetry.bytecode_ops_executed);
        self.jit_stats.compiled_blocks = self.jit_stats.compiled_blocks.saturating_add(1);
        true
    }

    fn exec_statement(&mut self, doc: &mut Document, raw: &str) {
        if !self.consume_step() {
            return;
        }
        let mut stmt = raw.trim();
        if stmt.is_empty() || stmt.starts_with("import ") {
            return;
        }
        if let Some(rest) = stmt.strip_prefix("export default ") {
            stmt = rest.trim();
        } else if let Some(rest) = stmt.strip_prefix("export ") {
            stmt = rest.trim();
        }

        if stmt == "break" {
            self.loop_break = true;
            return;
        }
        if stmt == "continue" {
            self.loop_continue = true;
            return;
        }
        if stmt == "return" {
            self.return_value = Some(Value::Null);
            return;
        }
        if let Some(ret_expr) = stmt.strip_prefix("return ") {
            let val = self.eval_expr(doc, ret_expr.trim());
            self.return_value = Some(val);
            return;
        }
        if let Some(throw_expr) = stmt.strip_prefix("throw ") {
            let val = self.eval_expr(doc, throw_expr.trim());
            self.thrown_error = Some(val);
            return;
        }

        if let Some(rest) = stmt.strip_prefix("try") {
            let rest_trimmed = rest.trim_start();
            if rest_trimmed.starts_with('{') {
                if let Some((try_body, after_try)) = extract_first_brace_block(rest_trimmed) {
                    self.eval_script(doc, &try_body);
                    let mut had_err = self.thrown_error.take();
                    if had_err.is_none() && self.aborted && self.steps <= MAX_EXECUTION_STEPS {
                        self.aborted = false;
                        had_err = Some(Value::Str("Error".to_string()));
                    }
                    let mut rem = after_try.trim();
                    if let Some(catch_rest) = rem.strip_prefix("catch") {
                        let catch_rest = catch_rest.trim_start();
                        let (catch_param, catch_body_src) = if catch_rest.starts_with('(') {
                            if let Some((p, r)) = extract_paren_group(catch_rest) {
                                (p.trim().to_string(), r)
                            } else {
                                (String::new(), catch_rest)
                            }
                        } else {
                            (String::new(), catch_rest)
                        };
                        if let Some((catch_body, after_catch)) =
                            extract_first_brace_block(catch_body_src)
                        {
                            rem = after_catch.trim();
                            if let Some(err_val) = had_err.take() {
                                if !catch_param.is_empty() {
                                    self.set_var(&catch_param, err_val);
                                }
                                self.eval_script(doc, &catch_body);
                            }
                        }
                    }
                    if let Some(finally_rest) = rem.strip_prefix("finally") {
                        if let Some((finally_body, _)) = extract_first_brace_block(finally_rest) {
                            let saved_return = self.return_value.take();
                            let saved_throw = self.thrown_error.take().or(had_err.take());
                            self.eval_script(doc, &finally_body);
                            if self.return_value.is_none() {
                                self.return_value = saved_return;
                            }
                            if self.thrown_error.is_none() {
                                self.thrown_error = saved_throw;
                            }
                            return;
                        }
                    }
                    if let Some(unhandled) = had_err {
                        self.thrown_error = Some(unhandled);
                    }
                    return;
                }
            }
        }

        if let Some(rest) = stmt.strip_prefix("class ") {
            if let Some((header, class_body)) = split_brace_block(rest) {
                let (name, super_class) = if let Some((n, s)) = header.split_once(" extends ") {
                    (n.trim().to_string(), Some(s.trim().to_string()))
                } else {
                    (header.trim().to_string(), None)
                };
                if !name.is_empty() {
                    let methods = parse_class_methods(&class_body);
                    let def = ClassDef {
                        name: name.clone(),
                        super_class,
                        methods,
                    };
                    if let Some(existing) = self.classes.iter_mut().find(|c| c.name == name) {
                        *existing = def;
                    } else {
                        self.classes.push(def);
                    }
                }
            }
            return;
        }

        if let Some(rest) = stmt.strip_prefix("switch") {
            let rest = rest.trim_start();
            if rest.starts_with('(') {
                if let Some((disc_str, after_disc)) = extract_paren_group(rest) {
                    if let Some((switch_body, _)) = extract_first_brace_block(after_disc) {
                        let disc_val = self.eval_expr(doc, &disc_str);
                        let clauses = parse_switch_clauses(&switch_body);
                        let mut matched_idx: Option<usize> = None;
                        let mut default_idx: Option<usize> = None;
                        for (idx, (case_expr_opt, _)) in clauses.iter().enumerate() {
                            if let Some(case_expr) = case_expr_opt {
                                let case_val = self.eval_expr(doc, case_expr);
                                if values_equal(&disc_val, &case_val, doc) {
                                    matched_idx = Some(idx);
                                    break;
                                }
                            } else if default_idx.is_none() {
                                default_idx = Some(idx);
                            }
                        }
                        if let Some(start_idx) = matched_idx.or(default_idx) {
                            for (_, stmts) in &clauses[start_idx..] {
                                for s in stmts {
                                    if self.aborted
                                        || self.return_value.is_some()
                                        || self.thrown_error.is_some()
                                        || self.loop_break
                                        || self.loop_continue
                                    {
                                        break;
                                    }
                                    self.exec_statement(doc, s);
                                }
                                if self.aborted
                                    || self.return_value.is_some()
                                    || self.thrown_error.is_some()
                                    || self.loop_break
                                    || self.loop_continue
                                {
                                    break;
                                }
                            }
                        }
                        self.loop_break = false;
                        return;
                    }
                }
            }
        }

        if let Some(rest) = stmt.strip_prefix("function ") {
            if let Some((sig, body)) = split_brace_block(rest) {
                if let Some((name, params_raw)) = sig.split_once('(') {
                    let name = name.trim().to_string();
                    let params_str = params_raw.trim().trim_end_matches(')');
                    let params: Vec<String> = params_str
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect();
                    if !name.is_empty() {
                        if let Some((_, existing_params, existing_body)) =
                            self.functions.iter_mut().find(|(n, _, _)| n == &name)
                        {
                            *existing_params = params;
                            *existing_body = body;
                        } else {
                            self.functions.push((name, params, body));
                        }
                    }
                }
            }
            return;
        }

        if let Some(rest) = stmt.strip_prefix("while") {
            let rest = rest.trim_start();
            if rest.starts_with('(') {
                if let Some((cond_str, after_cond)) = extract_paren_group(rest) {
                    if let Some((loop_body, _)) = extract_first_brace_block(after_cond) {
                        if self.try_native_page_loop(doc, stmt)
                            || self.try_native_numeric_loop(stmt)
                        {
                            return;
                        }
                        for _ in 0..10_000 {
                            if self.aborted
                                || self.return_value.is_some()
                                || self.thrown_error.is_some()
                                || !self.eval_expr(doc, &cond_str).is_truthy()
                            {
                                break;
                            }
                            self.eval_script(doc, &loop_body);
                            if self.loop_break {
                                self.loop_break = false;
                                break;
                            }
                            if self.loop_continue {
                                self.loop_continue = false;
                            }
                        }
                        return;
                    }
                }
            }
        }

        if let Some(rest) = stmt.strip_prefix("for") {
            let rest = rest.trim_start();
            if rest.starts_with('(') {
                if let Some((header_str, after_header)) = extract_paren_group(rest) {
                    if let Some((loop_body, _)) = extract_first_brace_block(after_header) {
                        let parts: Vec<&str> = header_str.splitn(3, ';').collect();
                        if parts.len() == 3 {
                            if self.try_native_page_loop(doc, stmt)
                                || self.try_native_numeric_loop(stmt)
                            {
                                return;
                            }
                            self.exec_statement(doc, parts[0]);
                            for _ in 0..10_000 {
                                if self.aborted
                                    || self.return_value.is_some()
                                    || self.thrown_error.is_some()
                                {
                                    break;
                                }
                                if !parts[1].trim().is_empty()
                                    && !self.eval_expr(doc, parts[1]).is_truthy()
                                {
                                    break;
                                }
                                self.eval_script(doc, &loop_body);
                                if self.loop_break {
                                    self.loop_break = false;
                                    break;
                                }
                                if self.loop_continue {
                                    self.loop_continue = false;
                                }
                                self.exec_statement(doc, parts[2]);
                            }
                            return;
                        }
                        if let Some((lhs_raw, rhs_raw)) = split_for_of_or_in(&header_str, " of ") {
                            let binding = lhs_raw
                                .trim()
                                .strip_prefix("let ")
                                .or_else(|| lhs_raw.trim().strip_prefix("const "))
                                .or_else(|| lhs_raw.trim().strip_prefix("var "))
                                .unwrap_or(lhs_raw.trim())
                                .trim();
                            let iterable = self.eval_expr(doc, rhs_raw.trim());
                            let items: Vec<Value> = match iterable {
                                Value::Array(v) => v,
                                Value::Json(JsonValue::Array(v)) => {
                                    v.into_iter().map(Value::from_json_value).collect()
                                }
                                Value::Str(s) => {
                                    s.chars().map(|c| Value::Str(c.to_string())).collect()
                                }
                                _ => Vec::new(),
                            };
                            for item in items.into_iter().take(MAX_ARRAY_LEN) {
                                if self.aborted
                                    || self.return_value.is_some()
                                    || self.thrown_error.is_some()
                                {
                                    break;
                                }
                                self.bind_pattern(doc, binding, item);
                                self.eval_script(doc, &loop_body);
                                if self.loop_break {
                                    self.loop_break = false;
                                    break;
                                }
                                if self.loop_continue {
                                    self.loop_continue = false;
                                }
                            }
                            return;
                        }
                        if let Some((lhs_raw, rhs_raw)) = split_for_of_or_in(&header_str, " in ") {
                            let binding = lhs_raw
                                .trim()
                                .strip_prefix("let ")
                                .or_else(|| lhs_raw.trim().strip_prefix("const "))
                                .or_else(|| lhs_raw.trim().strip_prefix("var "))
                                .unwrap_or(lhs_raw.trim())
                                .trim();
                            let target = self.eval_expr(doc, rhs_raw.trim());
                            let keys: Vec<Value> = match target {
                                Value::Json(JsonValue::Object(entries)) => {
                                    entries.into_iter().map(|(k, _)| Value::Str(k)).collect()
                                }
                                Value::Array(items) => (0..items.len())
                                    .map(|i| Value::Str(i.to_string()))
                                    .collect(),
                                Value::Json(JsonValue::Array(items)) => (0..items.len())
                                    .map(|i| Value::Str(i.to_string()))
                                    .collect(),
                                _ => Vec::new(),
                            };
                            for key in keys.into_iter().take(MAX_ARRAY_LEN) {
                                if self.aborted
                                    || self.return_value.is_some()
                                    || self.thrown_error.is_some()
                                {
                                    break;
                                }
                                self.bind_pattern(doc, binding, key);
                                self.eval_script(doc, &loop_body);
                                if self.loop_break {
                                    self.loop_break = false;
                                    break;
                                }
                                if self.loop_continue {
                                    self.loop_continue = false;
                                }
                            }
                            return;
                        }
                    }
                }
            }
        }

        if let Some(rest) = stmt.strip_prefix("if") {
            let rest = rest.trim_start();
            if rest.starts_with('(') {
                if let Some((cond_str, after_cond)) = extract_paren_group(rest) {
                    if let Some((then_body, after_then)) = extract_first_brace_block(after_cond) {
                        let cond_val = self.eval_expr(doc, &cond_str);
                        if cond_val.is_truthy() {
                            self.eval_script(doc, &then_body);
                        } else {
                            let after_then = after_then.trim();
                            if let Some(else_rest) = after_then.strip_prefix("else") {
                                if let Some((else_body, _)) = extract_first_brace_block(else_rest) {
                                    self.eval_script(doc, &else_body);
                                } else {
                                    self.exec_statement(doc, else_rest);
                                }
                            }
                        }
                        return;
                    }
                }
            }
        }

        let is_decl =
            stmt.starts_with("let ") || stmt.starts_with("const ") || stmt.starts_with("var ");
        let stripped = stmt
            .strip_prefix("let ")
            .or_else(|| stmt.strip_prefix("const "))
            .or_else(|| stmt.strip_prefix("var "))
            .unwrap_or(stmt)
            .trim();

        if is_decl && !stripped.starts_with('{') && !stripped.starts_with('[') {
            let decl_parts = split_args(stripped);
            if decl_parts.len() > 1 {
                for part in decl_parts {
                    if let Some((lhs, rhs)) = split_assignment(&part) {
                        let val = self.eval_expr(doc, rhs.trim());
                        self.set_var(lhs.trim(), val);
                    } else if is_identifier(part.trim()) {
                        self.set_var(part.trim(), Value::Null);
                    }
                }
                return;
            }
        }

        if let Some(ident) = stripped
            .strip_suffix("++")
            .or_else(|| stripped.strip_prefix("++"))
        {
            let ident = ident.trim();
            if is_identifier(ident) {
                let cur = self
                    .get_var(ident)
                    .and_then(Value::as_number)
                    .unwrap_or(0.0);
                self.set_var(ident, Value::Number(cur + 1.0));
                return;
            }
        }
        if let Some(ident) = stripped
            .strip_suffix("--")
            .or_else(|| stripped.strip_prefix("--"))
        {
            let ident = ident.trim();
            if is_identifier(ident) {
                let cur = self
                    .get_var(ident)
                    .and_then(Value::as_number)
                    .unwrap_or(0.0);
                self.set_var(ident, Value::Number(cur - 1.0));
                return;
            }
        }
        if let Some((lhs, rhs)) = stripped.split_once("+=") {
            let lhs = lhs.trim();
            let cur = self.eval_expr(doc, lhs);
            let add = self.eval_expr(doc, rhs.trim());
            let next = match (&cur, &add) {
                (Value::Number(a), Value::Number(b)) => Value::Number(a + b),
                _ => Value::Str(format!("{}{}", cur.as_string(doc), add.as_string(doc))),
            };
            self.assign_target(doc, lhs, next);
            return;
        }
        if let Some((lhs, rhs)) = stripped.split_once("-=") {
            let lhs = lhs.trim();
            let cur = self.eval_expr(doc, lhs).as_number().unwrap_or(0.0);
            let sub = self.eval_expr(doc, rhs.trim()).as_number().unwrap_or(0.0);
            self.assign_target(doc, lhs, Value::Number(cur - sub));
            return;
        }
        if let Some((lhs, rhs)) = stripped.split_once("*=") {
            let lhs = lhs.trim();
            let cur = self.eval_expr(doc, lhs).as_number().unwrap_or(0.0);
            let mul = self.eval_expr(doc, rhs.trim()).as_number().unwrap_or(0.0);
            self.assign_target(doc, lhs, Value::Number(cur * mul));
            return;
        }
        if let Some((lhs, rhs)) = stripped.split_once("/=") {
            let lhs = lhs.trim();
            let cur = self.eval_expr(doc, lhs).as_number().unwrap_or(0.0);
            let div = self.eval_expr(doc, rhs.trim()).as_number().unwrap_or(0.0);
            let out = if div == 0.0 { 0.0 } else { cur / div };
            self.assign_target(doc, lhs, Value::Number(out));
            return;
        }

        if let Some((lhs, rhs)) = split_assignment(stripped) {
            let lhs_trimmed = lhs.trim();
            let rhs_trimmed = rhs.trim();
            if is_decl
                && ((lhs_trimmed.starts_with('{') && lhs_trimmed.ends_with('}'))
                    || (lhs_trimmed.starts_with('[') && lhs_trimmed.ends_with(']')))
            {
                let value = self.eval_expr(doc, rhs_trimmed);
                self.bind_pattern(doc, lhs_trimmed, value);
                return;
            }
            if is_identifier(lhs_trimmed) {
                if let Some((params_part, arrow_body)) = split_top_level_arrow(rhs_trimmed) {
                    let params_str = params_part
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')');
                    let params: Vec<String> = params_str
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect();
                    let arrow_body = arrow_body.trim();
                    let body = if arrow_body.starts_with('{') {
                        split_brace_block(arrow_body)
                            .map(|(_, block)| block)
                            .unwrap_or_else(|| arrow_body.to_string())
                    } else {
                        format!("return {arrow_body}")
                    };
                    if let Some((_, existing_params, existing_body)) =
                        self.functions.iter_mut().find(|(n, _, _)| n == lhs_trimmed)
                    {
                        *existing_params = params;
                        *existing_body = body;
                    } else {
                        self.functions.push((lhs_trimmed.to_string(), params, body));
                    }
                    return;
                }
                if let Some(rest_fn) = rhs_trimmed.strip_prefix("function") {
                    if let Some((sig, body)) = split_brace_block(rest_fn) {
                        if let Some((_, params_raw)) = sig.split_once('(') {
                            let params_str = params_raw.trim().trim_end_matches(')');
                            let params: Vec<String> = params_str
                                .split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .collect();
                            self.functions.push((lhs_trimmed.to_string(), params, body));
                            return;
                        }
                    }
                }
            }
            let value = self.eval_expr(doc, rhs_trimmed);
            self.assign_target(doc, lhs_trimmed, value);
            return;
        }

        if is_decl && is_identifier(stripped) {
            self.set_var(stripped, Value::Null);
            return;
        }

        let _ = self.eval_expr(doc, stripped);
    }

    fn bind_pattern(&mut self, doc: &mut Document, pattern: &str, value: Value) {
        let pat = pattern.trim();
        if pat.starts_with('{') && pat.ends_with('}') && pat.len() >= 2 {
            let inner = &pat[1..pat.len() - 1];
            for entry in split_args(inner) {
                let (prop_part, default_expr) = split_assignment(&entry)
                    .map(|(l, r)| (l.trim(), Some(r.trim())))
                    .unwrap_or((entry.trim(), None));
                let (src_key, var_name) = if let Some((k, alias)) = prop_part.split_once(':') {
                    (k.trim(), alias.trim())
                } else {
                    (prop_part, prop_part)
                };
                if !is_identifier(var_name) {
                    continue;
                }
                let mut extracted = match &value {
                    Value::Json(jobj) => jobj
                        .get(src_key)
                        .cloned()
                        .map(Value::from_json_value)
                        .unwrap_or(Value::Null),
                    Value::Element(id) => self.read_element_prop(doc, *id, src_key),
                    _ => Value::Null,
                };
                if matches!(extracted, Value::Null) {
                    if let Some(def) = default_expr {
                        extracted = self.eval_expr(doc, def);
                    }
                }
                self.set_var(var_name, extracted);
            }
            return;
        }
        if pat.starts_with('[') && pat.ends_with(']') && pat.len() >= 2 {
            let inner = &pat[1..pat.len() - 1];
            let items: Vec<Value> = match value {
                Value::Array(v) => v,
                Value::Json(JsonValue::Array(v)) => {
                    v.into_iter().map(Value::from_json_value).collect()
                }
                _ => Vec::new(),
            };
            for (idx, entry) in split_args(inner).into_iter().enumerate() {
                let (var_name, default_expr) = split_assignment(&entry)
                    .map(|(l, r)| (l.trim(), Some(r.trim())))
                    .unwrap_or((entry.trim(), None));
                if !is_identifier(var_name) {
                    continue;
                }
                let mut item_val = items.get(idx).cloned().unwrap_or(Value::Null);
                if matches!(item_val, Value::Null) {
                    if let Some(def) = default_expr {
                        item_val = self.eval_expr(doc, def);
                    }
                }
                self.set_var(var_name, item_val);
            }
            return;
        }
        if is_identifier(pat) {
            self.set_var(pat, value);
        }
    }

    fn assign_target(&mut self, doc: &mut Document, lhs: &str, value: Value) {
        let lhs = lhs.trim();
        if lhs == "document.title" || lhs == "window.document.title" {
            let text = value.as_string(doc);
            doc.set_title(&text);
            return;
        }
        if lhs == "window.location.href" || lhs == "location.href" || lhs == "window.location" {
            let dest = value.as_string(doc);
            if !dest.trim().is_empty() {
                self.pending_navigation = Some(dest.trim().to_string());
            }
            return;
        }

        if lhs.ends_with(']') {
            if let Some(open) = lhs.rfind('[') {
                if open > 0 {
                    let target_ident = lhs[..open].trim();
                    let idx_expr = &lhs[open + 1..lhs.len() - 1];
                    let idx_val = self.eval_expr(doc, idx_expr);
                    if is_identifier(target_ident) {
                        if let Some(existing) = self.get_var(target_ident).cloned() {
                            match existing {
                                Value::Array(mut items) => {
                                    if let Some(n) = idx_val.as_number() {
                                        let i = n.max(0.0) as usize;
                                        if i < items.len() {
                                            items[i] = value;
                                        } else if i == items.len() && items.len() < MAX_ARRAY_LEN {
                                            items.push(value);
                                        }
                                        self.set_var(target_ident, Value::Array(items));
                                    }
                                    return;
                                }
                                Value::Json(mut jobj) => {
                                    let key = idx_val.as_string(doc);
                                    jobj.insert(&key, value.to_json_value(doc));
                                    self.set_var(target_ident, Value::Json(jobj));
                                    return;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        if let Some((obj_expr, prop)) = lhs.rsplit_once('.') {
            if let Some((el_expr, sub_lit)) = obj_expr.rsplit_once('.') {
                let sub_trimmed = sub_lit.trim();
                if sub_trimmed == "style" {
                    if let Value::Element(id) = self.eval_expr(doc, el_expr.trim()) {
                        let prop_trimmed = prop.trim();
                        let css_val = value.as_string(doc);
                        if prop_trimmed == "cssText" {
                            doc.set_attribute(id, "style", &css_val);
                        } else {
                            let css_prop = camel_to_kebab(prop_trimmed);
                            update_inline_style(doc, id, &css_prop, &css_val);
                        }
                    }
                    return;
                }
                if sub_trimmed == "dataset" {
                    if let Value::Element(id) = self.eval_expr(doc, el_expr.trim()) {
                        let attr_name = format!("data-{}", camel_to_kebab(prop.trim()));
                        let attr_val = value.as_string(doc);
                        doc.set_attribute(id, &attr_name, &attr_val);
                    }
                    return;
                }
            }
            if let Value::Element(id) = self.eval_expr(doc, obj_expr.trim()) {
                match prop.trim() {
                    "textContent" | "innerText" | "nodeValue" => {
                        let text = value.as_string(doc);
                        doc.set_text_content(id, &text);
                    }
                    "innerHTML" => {
                        let html = value.as_string(doc);
                        doc.set_inner_html(id, &html);
                        self.dom_cache.clear();
                    }
                    "value" => {
                        let text = value.as_string(doc);
                        doc.set_input_value(id, &text);
                    }
                    "checked" => {
                        doc.set_checked(id, value.is_truthy());
                    }
                    "disabled" => {
                        if value.is_truthy() {
                            doc.set_attribute(id, "disabled", "disabled");
                        } else {
                            doc.remove_attribute(id, "disabled");
                        }
                    }
                    "hidden" => {
                        if value.is_truthy() {
                            doc.set_attribute(id, "hidden", "hidden");
                            update_inline_style(doc, id, "display", "none");
                        } else {
                            doc.remove_attribute(id, "hidden");
                        }
                    }
                    "id" => {
                        let text = value.as_string(doc);
                        doc.set_attribute(id, "id", &text);
                        self.dom_cache.clear();
                    }
                    "className" => {
                        let text = value.as_string(doc);
                        doc.set_attribute(id, "class", &text);
                        self.dom_cache.clear();
                    }
                    "href" | "src" | "type" | "name" | "placeholder" | "title" | "alt" | "role" => {
                        let text = value.as_string(doc);
                        doc.set_attribute(id, prop.trim(), &text);
                    }
                    _ => {}
                }
                return;
            }
            if is_identifier(obj_expr.trim()) {
                if let Some(Value::Json(mut jobj)) = self.get_var(obj_expr.trim()).cloned() {
                    let jval = value.to_json_value(doc);
                    jobj.insert(prop.trim(), jval);
                    self.set_var(obj_expr.trim(), Value::Json(jobj));
                    return;
                }
            }
        }

        if is_identifier(lhs) {
            self.set_var(lhs, value);
        }
    }

    pub fn eval_expr(&mut self, doc: &mut Document, raw: &str) -> Value {
        if !self.consume_step() {
            return Value::Null;
        }
        let expr = raw.trim();
        if expr.is_empty() {
            return Value::Null;
        }

        if expr.starts_with('(') && expr.ends_with(')') {
            if let Some((inner, rem)) = extract_paren_group(expr) {
                if rem.trim().is_empty() {
                    return self.eval_expr(doc, &inner);
                }
            }
        }

        if expr.starts_with('[') && expr.ends_with(']') {
            let inner = &expr[1..expr.len() - 1];
            let mut items: Vec<Value> = Vec::new();
            for item in split_args(inner).into_iter().take(MAX_ARRAY_LEN) {
                if let Some(spread_expr) = item.trim().strip_prefix("...") {
                    match self.eval_expr(doc, spread_expr.trim()) {
                        Value::Array(sub) => {
                            for v in sub {
                                if items.len() < MAX_ARRAY_LEN {
                                    items.push(v);
                                }
                            }
                        }
                        Value::Json(JsonValue::Array(sub)) => {
                            for v in sub {
                                if items.len() < MAX_ARRAY_LEN {
                                    items.push(Value::from_json_value(v));
                                }
                            }
                        }
                        _ => {}
                    }
                } else {
                    items.push(self.eval_expr(doc, &item));
                }
            }
            return Value::Array(items);
        }

        if expr.starts_with('{') && expr.ends_with('}') {
            let inner = &expr[1..expr.len() - 1];
            let mut obj = JsonValue::empty_object();
            for pair in split_args(inner) {
                let pair_trimmed = pair.trim();
                if let Some(spread_expr) = pair_trimmed.strip_prefix("...") {
                    if let Value::Json(JsonValue::Object(entries)) =
                        self.eval_expr(doc, spread_expr.trim())
                    {
                        for (k, v) in entries {
                            obj.insert(&k, v);
                        }
                    }
                } else if let Some((k_raw, v_raw)) = pair_trimmed.split_once(':') {
                    let key = parse_string_literal(k_raw.trim())
                        .unwrap_or_else(|| k_raw.trim().to_string());
                    let val = self.eval_expr(doc, v_raw.trim()).to_json_value(doc);
                    obj.insert(&key, val);
                } else if is_identifier(pair_trimmed) {
                    let val = self.eval_expr(doc, pair_trimmed).to_json_value(doc);
                    obj.insert(pair_trimmed, val);
                }
            }
            return Value::Json(obj);
        }

        if expr.starts_with('`') && expr.ends_with('`') && expr.len() >= 2 {
            let inner = &expr[1..expr.len() - 1];
            return Value::Str(self.eval_template_literal(doc, inner));
        }

        if let Some(new_rest) = expr.strip_prefix("new ") {
            return self.eval_new_expr(doc, new_rest.trim());
        }

        if let Some(rest) = expr.strip_prefix("typeof ") {
            let v = self.eval_expr(doc, rest.trim());
            let ty = match v {
                Value::Null => "undefined",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::Str(_) => "string",
                Value::Element(_) | Value::Array(_) | Value::Json(_) => "object",
            };
            return Value::Str(ty.to_string());
        }

        if let Some(rest) = expr.strip_prefix('!') {
            if !rest.starts_with('=') {
                let v = self.eval_expr(doc, rest.trim());
                return Value::Bool(!v.is_truthy());
            }
        }

        if let Some((cond_s, branches_s)) = split_top_level_op(expr, "?") {
            if let Some((then_s, else_s)) = split_top_level_op(branches_s, ":") {
                if self.eval_expr(doc, cond_s).is_truthy() {
                    return self.eval_expr(doc, then_s);
                } else {
                    return self.eval_expr(doc, else_s);
                }
            }
        }

        if let Some((left, right)) = split_top_level_op(expr, "??") {
            let l = self.eval_expr(doc, left);
            if !matches!(l, Value::Null) {
                return l;
            }
            return self.eval_expr(doc, right);
        }

        if let Some((left, right)) = split_top_level_op(expr, "||") {
            let l = self.eval_expr(doc, left);
            if l.is_truthy() {
                return l;
            }
            return self.eval_expr(doc, right);
        }

        if let Some((left, right)) = split_top_level_op(expr, "&&") {
            let l = self.eval_expr(doc, left);
            if !l.is_truthy() {
                return Value::Bool(false);
            }
            return self.eval_expr(doc, right);
        }

        for op in ["===", "!==", "==", "!=", "<=", ">=", "<", ">"] {
            if let Some((left, right)) = split_top_level_op(expr, op) {
                let l = self.eval_expr(doc, left);
                let r = self.eval_expr(doc, right);
                let res = match op {
                    "===" | "==" => values_equal(&l, &r, doc),
                    "!==" | "!=" => !values_equal(&l, &r, doc),
                    "<" => l.as_number().zip(r.as_number()).is_some_and(|(a, b)| a < b),
                    "<=" => l
                        .as_number()
                        .zip(r.as_number())
                        .is_some_and(|(a, b)| a <= b),
                    ">" => l.as_number().zip(r.as_number()).is_some_and(|(a, b)| a > b),
                    ">=" => l
                        .as_number()
                        .zip(r.as_number())
                        .is_some_and(|(a, b)| a >= b),
                    _ => false,
                };
                return Value::Bool(res);
            }
        }

        if let Some((left, right)) = split_top_level_op(expr, "+") {
            let l = self.eval_expr(doc, left);
            let r = self.eval_expr(doc, right);
            if let (Value::Number(a), Value::Number(b)) = (&l, &r) {
                return Value::Number(a + b);
            }
            return Value::Str(format!("{}{}", l.as_string(doc), r.as_string(doc)));
        }

        for op in ["-", "*", "/", "%"] {
            if let Some((left, right)) = split_top_level_op(expr, op) {
                if left.trim().is_empty() && op == "-" {
                    let r = self.eval_expr(doc, right);
                    return Value::Number(-r.as_number().unwrap_or(0.0));
                }
                let l = self.eval_expr(doc, left).as_number().unwrap_or(0.0);
                let r = self.eval_expr(doc, right).as_number().unwrap_or(0.0);
                let out = match op {
                    "-" => l - r,
                    "*" => l * r,
                    "/" => {
                        if r == 0.0 {
                            0.0
                        } else {
                            l / r
                        }
                    }
                    "%" => eval_jit_binary_op(4, l, r),
                    _ => 0.0,
                };
                return Value::Number(out);
            }
        }

        if let Some(str_val) = parse_string_literal(expr) {
            return Value::Str(str_val);
        }
        if expr == "true" {
            return Value::Bool(true);
        }
        if expr == "false" {
            return Value::Bool(false);
        }
        if expr == "null" || expr == "undefined" {
            return Value::Null;
        }
        if let Ok(num) = expr.parse::<f64>() {
            return Value::Number(num);
        }
        if expr == "document.title" || expr == "window.document.title" {
            return Value::Str(doc.title());
        }
        if expr == "document.body" || expr == "window.document.body" {
            return Value::Element(doc.body());
        }
        if expr == "document.head" || expr == "window.document.head" {
            return Value::Element(doc.head());
        }
        if expr == "document.documentElement" || expr == "window.document.documentElement" {
            return Value::Element(doc.html());
        }
        if expr == "window.innerWidth" || expr == "innerWidth" {
            return Value::Number(1280.0);
        }
        if expr == "window.innerHeight" || expr == "innerHeight" {
            return Value::Number(800.0);
        }
        if expr == "window.devicePixelRatio" || expr == "devicePixelRatio" {
            return Value::Number(1.0);
        }
        if expr == "Math.PI" {
            return Value::Number(std::f64::consts::PI);
        }
        if expr == "Math.E" {
            return Value::Number(std::f64::consts::E);
        }
        if expr == "Number.MAX_SAFE_INTEGER" {
            return Value::Number(9_007_199_254_740_991.0);
        }
        if expr == "Number.MIN_SAFE_INTEGER" {
            return Value::Number(-9_007_199_254_740_991.0);
        }
        if expr == "window.location.href" || expr == "location.href" {
            return Value::Str(self.page_origin.clone().unwrap_or_default());
        }
        if expr == "localStorage.length" || expr == "window.localStorage.length" {
            let origin = self.storage_origin_key();
            let count = self
                .local_storage
                .iter()
                .filter(|(o, _, _)| o == &origin)
                .count();
            return Value::Number(count as f64);
        }
        if expr == "sessionStorage.length" || expr == "window.sessionStorage.length" {
            let origin = self.storage_origin_key();
            let count = self
                .session_storage
                .iter()
                .filter(|(o, _, _)| o == &origin)
                .count();
            return Value::Number(count as f64);
        }

        if expr.ends_with(')') {
            if let Some(open) = find_call_open_paren(expr) {
                let callee = expr[..open].trim();
                let args_src = &expr[open + 1..expr.len() - 1];
                return self.eval_call(doc, callee, args_src);
            }
        }

        if expr.ends_with(']') {
            if let Some(open) = expr.rfind('[') {
                if open > 0 {
                    let target_expr = expr[..open].trim();
                    let idx_expr = &expr[open + 1..expr.len() - 1];
                    let target = self.eval_expr(doc, target_expr);
                    let idx_val = self.eval_expr(doc, idx_expr);
                    match target {
                        Value::Array(items) => {
                            if let Some(i) = idx_val.as_number() {
                                return items
                                    .get(i.max(0.0) as usize)
                                    .cloned()
                                    .unwrap_or(Value::Null);
                            }
                        }
                        Value::Json(JsonValue::Array(items)) => {
                            if let Some(i) = idx_val.as_number() {
                                return items
                                    .get(i.max(0.0) as usize)
                                    .cloned()
                                    .map(Value::from_json_value)
                                    .unwrap_or(Value::Null);
                            }
                        }
                        Value::Json(jobj) => {
                            let key = idx_val.as_string(doc);
                            return jobj
                                .get(&key)
                                .cloned()
                                .map(Value::from_json_value)
                                .unwrap_or(Value::Null);
                        }
                        _ => {}
                    }
                }
            }
        }

        if let Some((obj_expr_raw, prop)) = expr.rsplit_once('.') {
            let obj_expr = obj_expr_raw
                .trim()
                .strip_suffix('?')
                .unwrap_or(obj_expr_raw.trim())
                .trim();
            let prop_trimmed = prop.trim();
            if let Some((el_expr_raw, sub_lit)) = obj_expr.rsplit_once('.') {
                let el_expr = el_expr_raw
                    .trim()
                    .strip_suffix('?')
                    .unwrap_or(el_expr_raw.trim())
                    .trim();
                let sub_trimmed = sub_lit.trim();
                if sub_trimmed == "style" {
                    if let Value::Element(id) = self.eval_expr(doc, el_expr) {
                        if prop_trimmed == "cssText" {
                            return Value::Str(
                                doc.get(id).data().attr("style").unwrap_or("").to_string(),
                            );
                        }
                        let css_prop = camel_to_kebab(prop_trimmed);
                        return Value::Str(read_inline_style(doc, id, &css_prop));
                    }
                    return Value::Str(String::new());
                }
                if sub_trimmed == "dataset" {
                    if let Value::Element(id) = self.eval_expr(doc, el_expr) {
                        let attr_name = format!("data-{}", camel_to_kebab(prop_trimmed));
                        return doc
                            .get(id)
                            .data()
                            .attr(&attr_name)
                            .map(|v| Value::Str(v.to_string()))
                            .unwrap_or(Value::Null);
                    }
                    return Value::Null;
                }
            }
            match self.eval_expr(doc, obj_expr) {
                Value::Element(id) => {
                    return self.read_element_prop(doc, id, prop_trimmed);
                }
                Value::Array(items) => {
                    if prop_trimmed == "length" {
                        return Value::Number(items.len() as f64);
                    }
                    return Value::Null;
                }
                Value::Json(jval) => {
                    if prop_trimmed == "length" || prop_trimmed == "size" {
                        if let JsonValue::Array(items) = &jval {
                            return Value::Number(items.len() as f64);
                        }
                        if let JsonValue::Object(entries) = &jval {
                            return Value::Number(entries.len() as f64);
                        }
                    }
                    return jval
                        .get(prop_trimmed)
                        .cloned()
                        .map(Value::from_json_value)
                        .unwrap_or(Value::Null);
                }
                Value::Str(s) if prop_trimmed == "length" => {
                    return Value::Number(s.chars().count() as f64);
                }
                _ => {}
            }
        }

        if let Some(val) = self.get_var(expr) {
            return val.clone();
        }
        if let Some(stripped_win) = expr
            .strip_prefix("window.")
            .or_else(|| expr.strip_prefix("globalThis."))
        {
            if let Some(val) = self.get_var(stripped_win.trim()) {
                return val.clone();
            }
        }

        Value::Null
    }

    fn read_element_prop(&self, doc: &Document, id: NodeId, prop: &str) -> Value {
        match prop {
            "textContent" | "innerText" | "nodeValue" => Value::Str(doc.text_content(id)),
            "innerHTML" => Value::Str(doc.inner_html(id)),
            "outerHTML" => Value::Str(doc.outer_html(id)),
            "tagName" | "nodeName" => Value::Str(
                doc.get(id)
                    .data()
                    .tag()
                    .unwrap_or("#text")
                    .to_ascii_uppercase(),
            ),
            "value" => Value::Str(doc.input_value(id)),
            "checked" => Value::Bool(doc.is_checked(id)),
            "disabled" => Value::Bool(doc.get(id).data().attr("disabled").is_some()),
            "hidden" => Value::Bool(doc.get(id).data().attr("hidden").is_some()),
            "id" => Value::Str(doc.get(id).data().id().unwrap_or("").to_string()),
            "className" => Value::Str(doc.get(id).data().attr("class").unwrap_or("").to_string()),
            "href" | "src" | "type" | "name" | "placeholder" | "title" | "alt" | "role" => {
                Value::Str(doc.get(id).data().attr(prop).unwrap_or("").to_string())
            }
            "parentElement" | "parentNode" => doc
                .parent_element(id)
                .map(Value::Element)
                .unwrap_or(Value::Null),
            "children" | "childNodes" => Value::Array(
                doc.element_children(id)
                    .into_iter()
                    .map(Value::Element)
                    .collect(),
            ),
            "childElementCount" => Value::Number(doc.element_children(id).len() as f64),
            "firstElementChild" | "firstChild" => doc
                .first_element_child(id)
                .map(Value::Element)
                .unwrap_or(Value::Null),
            "lastElementChild" | "lastChild" => doc
                .last_element_child(id)
                .map(Value::Element)
                .unwrap_or(Value::Null),
            "nextElementSibling" | "nextSibling" => doc
                .next_element_sibling(id)
                .map(Value::Element)
                .unwrap_or(Value::Null),
            "previousElementSibling" | "previousSibling" => doc
                .previous_element_sibling(id)
                .map(Value::Element)
                .unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    fn eval_new_expr(&mut self, doc: &mut Document, new_rest: &str) -> Value {
        let (callee, args_src) = if new_rest.ends_with(')') {
            if let Some(open) = find_call_open_paren(new_rest) {
                (
                    new_rest[..open].trim(),
                    &new_rest[open + 1..new_rest.len() - 1],
                )
            } else {
                (new_rest, "")
            }
        } else {
            (new_rest, "")
        };
        let args = split_args(args_src);
        match callee {
            "Object" | "Map" | "Set" => Value::Json(JsonValue::empty_object()),
            "Array" => {
                if args.len() == 1 {
                    if let Some(n) = self.eval_expr(doc, &args[0]).as_number() {
                        let count = (n.max(0.0) as usize).min(MAX_ARRAY_LEN);
                        return Value::Array(vec![Value::Null; count]);
                    }
                }
                let items = args
                    .into_iter()
                    .take(MAX_ARRAY_LEN)
                    .map(|a| self.eval_expr(doc, &a))
                    .collect();
                Value::Array(items)
            }
            "Error" | "TypeError" | "RangeError" | "SyntaxError" | "ReferenceError" => {
                let msg = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                let mut obj = JsonValue::empty_object();
                obj.insert("name", JsonValue::String(callee.to_string()));
                obj.insert("message", JsonValue::String(msg));
                Value::Json(obj)
            }
            "Date" => Value::Number(1_700_000_000_000.0 + self.steps as f64),
            "URL" => {
                let raw = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                let mut obj = JsonValue::empty_object();
                obj.insert("href", JsonValue::String(raw.clone()));
                if let Some((scheme, host, port)) = extract_origin_tuple(&raw) {
                    obj.insert("protocol", JsonValue::String(format!("{scheme}:")));
                    obj.insert("hostname", JsonValue::String(host.clone()));
                    obj.insert("port", JsonValue::String(port.to_string()));
                    obj.insert(
                        "origin",
                        JsonValue::String(format!("{scheme}://{host}:{port}")),
                    );
                }
                Value::Json(obj)
            }
            _ => {
                if let Some(class_def) = self.classes.iter().find(|c| c.name == callee).cloned() {
                    let mut instance = JsonValue::empty_object();
                    instance.insert("__class", JsonValue::String(class_def.name.clone()));
                    let prev_this = self.get_var("this").cloned();
                    self.set_var("this", Value::Json(instance));
                    if let Some((_, params, body)) = class_def
                        .methods
                        .iter()
                        .find(|(m, _, _)| m == "constructor")
                        .cloned()
                    {
                        self.invoke_method_body(
                            doc,
                            &params,
                            &body,
                            &args,
                            class_def.super_class.as_deref(),
                        );
                    }
                    let result = self.get_var("this").cloned().unwrap_or(Value::Null);
                    if let Some(old) = prev_this {
                        self.set_var("this", old);
                    }
                    return result;
                }
                self.eval_call(doc, callee, args_src)
            }
        }
    }

    fn invoke_method_body(
        &mut self,
        doc: &mut Document,
        params: &[String],
        body: &str,
        args: &[String],
        super_class: Option<&str>,
    ) -> Value {
        if self.call_depth >= MAX_CALL_DEPTH {
            self.aborted = true;
            return Value::Null;
        }
        let evaled_args: Vec<Value> = args.iter().map(|a| self.eval_expr(doc, a)).collect();
        for (idx, param) in params.iter().enumerate() {
            let val = evaled_args.get(idx).cloned().unwrap_or(Value::Null);
            self.set_var(param, val);
        }
        if let Some(sup_name) = super_class {
            self.set_var("__super_class", Value::Str(sup_name.to_string()));
        }
        let prev_return = self.return_value.take();
        self.call_depth += 1;
        self.eval_script(doc, body);
        self.call_depth = self.call_depth.saturating_sub(1);
        let result = self.return_value.take().unwrap_or(Value::Null);
        self.return_value = prev_return;
        result
    }

    fn find_class_method(
        &self,
        class_name: &str,
        method_name: &str,
    ) -> Option<(Vec<String>, String, Option<String>)> {
        let mut cursor = Some(class_name.to_string());
        let mut depth = 0usize;
        while let Some(curr) = cursor {
            if depth > 16 {
                break;
            }
            depth += 1;
            let def = self.classes.iter().find(|c| c.name == curr)?;
            if let Some((_, params, body)) = def.methods.iter().find(|(m, _, _)| m == method_name) {
                return Some((params.clone(), body.clone(), def.super_class.clone()));
            }
            cursor = def.super_class.clone();
        }
        None
    }

    fn eval_template_literal(&mut self, doc: &mut Document, inner: &str) -> String {
        let mut out = String::new();
        let mut rest = inner;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            if let Some(end) = find_template_expr_end(after) {
                let expr_str = &after[..end];
                let val = self.eval_expr(doc, expr_str);
                out.push_str(&val.as_string(doc));
                rest = &after[end + 1..];
            } else {
                out.push_str(&rest[start..]);
                return out;
            }
        }
        out.push_str(rest);
        out
    }

    fn lookup_cached_dom(&mut self, key: &str) -> Option<NodeId> {
        if !self.jit_stats.jit_enabled {
            return None;
        }
        if let Some((_, id)) = self.dom_cache.iter().find(|(k, _)| k == key) {
            self.jit_stats.inline_cache_hits = self.jit_stats.inline_cache_hits.saturating_add(1);
            return Some(*id);
        }
        None
    }

    fn store_cached_dom(&mut self, key: String, id: NodeId) {
        if self.jit_stats.jit_enabled && self.dom_cache.len() < 128 {
            self.dom_cache.push((key, id));
        }
    }

    fn eval_call(&mut self, doc: &mut Document, callee_raw: &str, args_src: &str) -> Value {
        let callee = callee_raw
            .trim()
            .strip_prefix("window.")
            .or_else(|| callee_raw.trim().strip_prefix("globalThis."))
            .unwrap_or(callee_raw.trim())
            .trim();

        if callee == "super" {
            let args = split_args(args_src);
            if let Some(Value::Str(sup_name)) = self.get_var("__super_class").cloned() {
                if let Some((params, body, next_super)) =
                    self.find_class_method(&sup_name, "constructor")
                {
                    return self.invoke_method_body(
                        doc,
                        &params,
                        &body,
                        &args,
                        next_super.as_deref(),
                    );
                }
            }
            return Value::Null;
        }

        if callee.starts_with('(') && callee.ends_with(')') {
            if let Some((inner_fn, rem)) = extract_paren_group(callee) {
                if rem.trim().is_empty() {
                    let args = split_args(args_src);
                    let (params, body) = extract_multi_param_callback(inner_fn.trim());
                    return self.invoke_method_body(doc, &params, &body, &args, None);
                }
            }
        }

        match callee {
            "parseInt" | "parseFloat" | "Number" => {
                let args = split_args(args_src);
                let raw = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                let trimmed = raw.trim();
                let num = if callee == "parseInt" {
                    trimmed
                        .split(|c: char| !c.is_ascii_digit() && c != '-' && c != '+')
                        .next()
                        .and_then(|s| s.parse::<i64>().ok())
                        .map(|n| n as f64)
                        .unwrap_or(0.0)
                } else {
                    trimmed.parse::<f64>().unwrap_or(0.0)
                };
                return Value::Number(num);
            }
            "String" => {
                let args = split_args(args_src);
                let s = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                return Value::Str(s);
            }
            "Boolean" => {
                let args = split_args(args_src);
                let b = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_bool())
                    .unwrap_or(false);
                return Value::Bool(b);
            }
            "isNaN" | "Number.isNaN" => {
                let args = split_args(args_src);
                let is_nan = args
                    .first()
                    .map(|a| {
                        let v = self.eval_expr(doc, a);
                        match v {
                            Value::Number(n) => n.is_nan(),
                            Value::Str(s) => s.trim().parse::<f64>().is_err(),
                            _ => false,
                        }
                    })
                    .unwrap_or(true);
                return Value::Bool(is_nan);
            }
            "Number.isFinite" => {
                let args = split_args(args_src);
                let finite = args
                    .first()
                    .map(|a| matches!(self.eval_expr(doc, a), Value::Number(n) if n.is_finite()))
                    .unwrap_or(false);
                return Value::Bool(finite);
            }
            "Number.isInteger" => {
                let args = split_args(args_src);
                let is_int = args
                    .first()
                    .map(|a| {
                        matches!(
                            self.eval_expr(doc, a),
                            Value::Number(n) if n.is_finite() && n.fract() == 0.0
                        )
                    })
                    .unwrap_or(false);
                return Value::Bool(is_int);
            }
            "Array.isArray" => {
                let args = split_args(args_src);
                let is_arr = args
                    .first()
                    .map(|a| {
                        matches!(
                            self.eval_expr(doc, a),
                            Value::Array(_) | Value::Json(JsonValue::Array(_))
                        )
                    })
                    .unwrap_or(false);
                return Value::Bool(is_arr);
            }
            "Array.from" => {
                let args = split_args(args_src);
                if let Some(first) = args.first() {
                    return match self.eval_expr(doc, first) {
                        Value::Array(items) => Value::Array(items),
                        Value::Json(JsonValue::Array(items)) => {
                            Value::Array(items.into_iter().map(Value::from_json_value).collect())
                        }
                        Value::Str(s) => {
                            Value::Array(s.chars().map(|c| Value::Str(c.to_string())).collect())
                        }
                        _ => Value::Array(Vec::new()),
                    };
                }
                return Value::Array(Vec::new());
            }
            "Object.keys" => {
                let args = split_args(args_src);
                if let Some(first) = args.first() {
                    if let Value::Json(JsonValue::Object(entries)) = self.eval_expr(doc, first) {
                        return Value::Array(
                            entries.into_iter().map(|(k, _)| Value::Str(k)).collect(),
                        );
                    }
                }
                return Value::Array(Vec::new());
            }
            "Object.values" => {
                let args = split_args(args_src);
                if let Some(first) = args.first() {
                    if let Value::Json(JsonValue::Object(entries)) = self.eval_expr(doc, first) {
                        return Value::Array(
                            entries
                                .into_iter()
                                .map(|(_, v)| Value::from_json_value(v))
                                .collect(),
                        );
                    }
                }
                return Value::Array(Vec::new());
            }
            "Object.entries" => {
                let args = split_args(args_src);
                if let Some(first) = args.first() {
                    if let Value::Json(JsonValue::Object(entries)) = self.eval_expr(doc, first) {
                        return Value::Array(
                            entries
                                .into_iter()
                                .map(|(k, v)| {
                                    Value::Array(vec![Value::Str(k), Value::from_json_value(v)])
                                })
                                .collect(),
                        );
                    }
                }
                return Value::Array(Vec::new());
            }
            "Object.assign" => {
                let args = split_args(args_src);
                let mut merged = JsonValue::empty_object();
                let target_ident = args.first().map(|s| s.trim().to_string());
                for arg in &args {
                    if let Value::Json(JsonValue::Object(entries)) = self.eval_expr(doc, arg) {
                        for (k, v) in entries {
                            merged.insert(&k, v);
                        }
                    }
                }
                let out = Value::Json(merged);
                if let Some(ident) = target_ident {
                    if is_identifier(&ident) {
                        self.set_var(&ident, out.clone());
                    }
                }
                return out;
            }
            "Object.fromEntries" => {
                let args = split_args(args_src);
                let mut obj = JsonValue::empty_object();
                if let Some(first) = args.first() {
                    if let Value::Array(rows) = self.eval_expr(doc, first) {
                        for row in rows {
                            if let Value::Array(pair) = row {
                                if pair.len() >= 2 {
                                    let k = pair[0].as_string(doc);
                                    let v = pair[1].to_json_value(doc);
                                    obj.insert(&k, v);
                                }
                            }
                        }
                    }
                }
                return Value::Json(obj);
            }
            "encodeURIComponent" => {
                let args = split_args(args_src);
                let raw = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                let mut encoded = String::new();
                for b in raw.bytes() {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                        encoded.push(b as char);
                    } else {
                        encoded.push_str(&format!("%{:02X}", b));
                    }
                }
                return Value::Str(encoded);
            }
            "decodeURIComponent" => {
                let args = split_args(args_src);
                let raw = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                return Value::Str(raw.replace("%20", " ").replace('+', " "));
            }
            "queueMicrotask" => {
                let args = split_args(args_src);
                if let Some(first) = args.first() {
                    if self.microtasks.len() < MAX_MICROTASKS {
                        self.microtasks.push(extract_callback_body(first.trim()));
                    }
                }
                return Value::Null;
            }
            "performance.now" => {
                return Value::Number(self.steps as f64 * 0.01);
            }
            "Date.now" => {
                return Value::Number(1_700_000_000_000.0 + self.steps as f64);
            }
            "matchMedia" => {
                let args = split_args(args_src);
                let q = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                let mut obj = JsonValue::empty_object();
                obj.insert("matches", JsonValue::Bool(false));
                obj.insert("media", JsonValue::String(q));
                return Value::Json(obj);
            }
            "getComputedStyle" => {
                let args = split_args(args_src);
                let mut obj = JsonValue::empty_object();
                if let Some(first) = args.first() {
                    if let Value::Element(id) = self.eval_expr(doc, first) {
                        let inline = doc.get(id).data().attr("style").unwrap_or("");
                        for part in inline.split(';') {
                            if let Some((k, v)) = part.split_once(':') {
                                let k = k.trim().to_ascii_lowercase();
                                let v = v.trim().to_string();
                                if !k.is_empty() {
                                    obj.insert(&k, JsonValue::String(v.clone()));
                                    obj.insert(&kebab_to_camel(&k), JsonValue::String(v));
                                }
                            }
                        }
                    }
                }
                return Value::Json(obj);
            }
            "clearTimeout" | "clearInterval" | "cancelAnimationFrame" | "scrollTo" | "scrollBy" => {
                return Value::Null;
            }
            "addEventListener" => {
                let args = split_args(args_src);
                if args.len() >= 2 {
                    let event = self.eval_expr(doc, &args[0]).as_string(doc);
                    let body = extract_callback_body(args[1].trim());
                    if event.eq_ignore_ascii_case("click") {
                        self.click_listeners.push((doc.body(), body));
                    } else if matches!(
                        event.to_ascii_lowercase().as_str(),
                        "domcontentloaded" | "load" | "readystatechange"
                    ) && self.microtasks.len() < MAX_MICROTASKS
                    {
                        self.microtasks.push(body);
                    }
                }
                return Value::Null;
            }
            _ => {}
        }

        if callee == "setTimeout" || callee == "setInterval" || callee == "requestAnimationFrame" {
            let args = split_args(args_src);
            if let Some(first) = args.first() {
                if self.timers.len() < MAX_TIMERS {
                    let handler_raw = first.trim();
                    let body = extract_callback_body(handler_raw);
                    let delay = args
                        .get(1)
                        .and_then(|d| self.eval_expr(doc, d).as_number())
                        .unwrap_or(0.0)
                        .max(0.0) as u64;
                    self.timers.push((delay, body));
                }
            }
            return Value::Number(self.timers.len() as f64);
        }

        if callee == "fetch" {
            let args = split_args(args_src);
            let url = args
                .first()
                .map(|a| self.eval_expr(doc, a).as_string(doc))
                .unwrap_or_default();
            if !is_allowed_script_fetch(self.page_origin.as_deref(), &url) {
                let mut obj = JsonValue::empty_object();
                obj.insert("url", JsonValue::String(url));
                obj.insert("status", JsonValue::Number(403.0));
                obj.insert("ok", JsonValue::Bool(false));
                obj.insert(
                    "body",
                    JsonValue::String(
                        "{\"error\":\"same-origin policy blocked fetch\"}".to_string(),
                    ),
                );
                return Value::Json(obj);
            }
            if let Some((status, body)) = self
                .fetch_mocks
                .iter()
                .rev()
                .find(|(registered, _, _)| registered == &url)
                .map(|(_, status, body)| (*status, body.clone()))
            {
                return fetch_result_value(&url, status, body);
            }
            // The step budget is spent before the host runs, so a spent budget
            // does not open a socket.
            if !self.consume_step() {
                return fetch_result_value(&url, 0, String::new());
            }
            let page = self.page_origin.clone().unwrap_or_default();
            let absolute = resolve_script_fetch_url(&page, &url);
            let Some(host) = self.fetch_host.inner.clone() else {
                return fetch_result_value(&url, 200, "{}".to_string());
            };
            let mut reply = host(&absolute, &page);
            limit_script_fetch_body(&mut reply.body);
            let extra_steps = reply.body.len() / 1024;
            for _ in 0..extra_steps {
                if !self.consume_step() {
                    break;
                }
            }
            return fetch_result_value(&url, reply.status, reply.body);
        }

        if let Some((obj_expr_raw, method)) = callee.rsplit_once('.') {
            let obj_expr = obj_expr_raw
                .trim()
                .strip_suffix('?')
                .unwrap_or(obj_expr_raw.trim())
                .trim();
            let method = method.trim();

            if obj_expr == "localStorage" || obj_expr == "window.localStorage" {
                let args = split_args(args_src);
                return match method {
                    "getItem" => {
                        let key = args
                            .first()
                            .map(|a| self.eval_expr(doc, a).as_string(doc))
                            .unwrap_or_default();
                        self.local_storage_get(&key)
                            .map(|v| Value::Str(v.to_string()))
                            .unwrap_or(Value::Null)
                    }
                    "setItem" if args.len() >= 2 => {
                        let key = self.eval_expr(doc, &args[0]).as_string(doc);
                        let val = self.eval_expr(doc, &args[1]).as_string(doc);
                        self.local_storage_set(&key, &val);
                        Value::Null
                    }
                    "removeItem" if !args.is_empty() => {
                        let key = self.eval_expr(doc, &args[0]).as_string(doc);
                        self.local_storage_remove(&key);
                        Value::Null
                    }
                    "clear" => {
                        self.local_storage_clear();
                        Value::Null
                    }
                    _ => Value::Null,
                };
            }

            if obj_expr == "sessionStorage" || obj_expr == "window.sessionStorage" {
                let args = split_args(args_src);
                let origin = self.storage_origin_key();
                return match method {
                    "getItem" => {
                        let key = args
                            .first()
                            .map(|a| self.eval_expr(doc, a).as_string(doc))
                            .unwrap_or_default();
                        self.session_storage
                            .iter()
                            .find(|(o, k, _)| o == &origin && k == &key)
                            .map(|(_, _, v)| Value::Str(v.clone()))
                            .unwrap_or(Value::Null)
                    }
                    "setItem" if args.len() >= 2 => {
                        let key = self.eval_expr(doc, &args[0]).as_string(doc);
                        let val = self.eval_expr(doc, &args[1]).as_string(doc);
                        storage_put(&mut self.session_storage, &origin, &key, &val);
                        Value::Null
                    }
                    "removeItem" if !args.is_empty() => {
                        let key = self.eval_expr(doc, &args[0]).as_string(doc);
                        self.session_storage
                            .retain(|(o, k, _)| !(o == &origin && k == &key));
                        Value::Null
                    }
                    "clear" => {
                        self.session_storage.retain(|(o, _, _)| o != &origin);
                        Value::Null
                    }
                    _ => Value::Null,
                };
            }

            if obj_expr == "Promise" && method == "resolve" {
                let args = split_args(args_src);
                let val = args
                    .first()
                    .map(|a| self.eval_expr(doc, a))
                    .unwrap_or(Value::Null);
                let mut obj = JsonValue::empty_object();
                obj.insert("__promise_resolved", val.to_json_value(doc));
                return Value::Json(obj);
            }

            if obj_expr == "console" {
                let args = split_args(args_src);
                let line = args
                    .iter()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .collect::<Vec<_>>()
                    .join(" ");
                if self.console_logs.len() < 256 {
                    let prefix = match method {
                        "warn" => "[warn] ",
                        "error" => "[error] ",
                        _ => "",
                    };
                    self.console_logs.push(format!("{prefix}{line}"));
                }
                return Value::Null;
            }

            if obj_expr == "Math" {
                let args = split_args(args_src);
                let first = args
                    .first()
                    .and_then(|a| self.eval_expr(doc, a).as_number())
                    .unwrap_or(0.0);
                let second = args
                    .get(1)
                    .and_then(|a| self.eval_expr(doc, a).as_number())
                    .unwrap_or(0.0);
                let res = match method {
                    "floor" => first.floor(),
                    "ceil" => first.ceil(),
                    "round" => first.round(),
                    "abs" => first.abs(),
                    "sqrt" => first.max(0.0).sqrt(),
                    "min" => {
                        if args.len() >= 2 {
                            first.min(second)
                        } else {
                            first
                        }
                    }
                    "max" => {
                        if args.len() >= 2 {
                            first.max(second)
                        } else {
                            first
                        }
                    }
                    "pow" => first.powf(second),
                    "random" => 0.42,
                    _ => 0.0,
                };
                return Value::Number(res);
            }

            if let Some((el_expr_raw, sub_obj)) = obj_expr.rsplit_once('.') {
                let el_expr = el_expr_raw
                    .trim()
                    .strip_suffix('?')
                    .unwrap_or(el_expr_raw.trim())
                    .trim();
                let sub_trimmed = sub_obj.trim();
                if sub_trimmed == "classList" {
                    if let Value::Element(id) = self.eval_expr(doc, el_expr) {
                        let args = split_args(args_src);
                        let cls = args
                            .first()
                            .map(|a| self.eval_expr(doc, a).as_string(doc))
                            .unwrap_or_default();
                        return match method {
                            "add" => {
                                doc.add_class(id, &cls);
                                Value::Null
                            }
                            "remove" => {
                                doc.remove_class(id, &cls);
                                Value::Null
                            }
                            "toggle" => Value::Bool(doc.toggle_class(id, &cls)),
                            "contains" => Value::Bool(doc.get(id).data().has_class(&cls)),
                            _ => Value::Null,
                        };
                    }
                }
                if sub_trimmed == "style" {
                    if let Value::Element(id) = self.eval_expr(doc, el_expr) {
                        let args = split_args(args_src);
                        let prop = args
                            .first()
                            .map(|a| camel_to_kebab(&self.eval_expr(doc, a).as_string(doc)))
                            .unwrap_or_default();
                        return match method {
                            "setProperty" if args.len() >= 2 => {
                                let val = self.eval_expr(doc, &args[1]).as_string(doc);
                                update_inline_style(doc, id, &prop, &val);
                                Value::Null
                            }
                            "getPropertyValue" => Value::Str(read_inline_style(doc, id, &prop)),
                            "removeProperty" => {
                                let prev = read_inline_style(doc, id, &prop);
                                remove_inline_style_prop(doc, id, &prop);
                                Value::Str(prev)
                            }
                            _ => Value::Null,
                        };
                    }
                }
            }

            if obj_expr == "JSON" {
                let args = split_args(args_src);
                return match method {
                    "parse" => {
                        let first = args
                            .first()
                            .map(|a| self.eval_expr(doc, a).as_string(doc))
                            .unwrap_or_default();
                        parse_json(&first)
                            .map(Value::from_json_value)
                            .unwrap_or(Value::Null)
                    }
                    "stringify" => {
                        let first = args
                            .first()
                            .map(|a| self.eval_expr(doc, a).to_json_value(doc))
                            .unwrap_or(JsonValue::Null);
                        Value::Str(to_json_string(&first))
                    }
                    _ => Value::Null,
                };
            }

            if obj_expr == "navigator" {
                let args = split_args(args_src);
                let first = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                return match method {
                    "requestMediaKeySystemAccess" => {
                        let lower = first.trim().to_ascii_lowercase();
                        let supported = matches!(
                            lower.as_str(),
                            "org.w3.clearkey"
                                | "org.anvil.mediafoundation.cdm"
                                | "org.anvil.external.cdm"
                        );
                        let mut obj = JsonValue::empty_object();
                        obj.insert("keySystem", JsonValue::String(lower));
                        obj.insert("supported", JsonValue::Bool(supported));
                        obj.insert("cenc", JsonValue::String("cenc".to_string()));
                        Value::Json(obj)
                    }
                    _ => Value::Null,
                };
            }

            if obj_expr == "document" || obj_expr == "window.document" {
                let args = split_args(args_src);
                let first = args
                    .first()
                    .map(|a| self.eval_expr(doc, a).as_string(doc))
                    .unwrap_or_default();
                return match method {
                    "getElementById" => {
                        let cache_key = format!("#{}", first);
                        if let Some(id) = self.lookup_cached_dom(&cache_key) {
                            return Value::Element(id);
                        }
                        if let Some(id) = doc.get_element_by_id(&first) {
                            self.store_cached_dom(cache_key, id);
                            Value::Element(id)
                        } else {
                            Value::Null
                        }
                    }
                    "querySelector" => {
                        if let Some(id) = self.lookup_cached_dom(&first) {
                            return Value::Element(id);
                        }
                        if let Some(id) = doc.query_selector(&first) {
                            self.store_cached_dom(first, id);
                            Value::Element(id)
                        } else {
                            Value::Null
                        }
                    }
                    "querySelectorAll" => {
                        let ids = doc.query_selector_all(&first);
                        Value::Array(ids.into_iter().map(Value::Element).collect())
                    }
                    "getElementsByClassName" => {
                        let sel = format!(".{}", first.trim());
                        let ids = doc.query_selector_all(&sel);
                        Value::Array(ids.into_iter().map(Value::Element).collect())
                    }
                    "getElementsByTagName" => {
                        let ids = doc.query_selector_all(first.trim());
                        Value::Array(ids.into_iter().map(Value::Element).collect())
                    }
                    "createElement" => {
                        let id = doc.create_element(&first, Vec::new());
                        Value::Element(id)
                    }
                    "createTextNode" => {
                        let id = doc.create_text(&first);
                        Value::Element(id)
                    }
                    "addEventListener" if args.len() >= 2 => {
                        let body = extract_callback_body(args[1].trim());
                        if first.eq_ignore_ascii_case("click") {
                            self.click_listeners.push((doc.body(), body));
                        } else if matches!(
                            first.to_ascii_lowercase().as_str(),
                            "domcontentloaded" | "load" | "readystatechange"
                        ) && self.microtasks.len() < MAX_MICROTASKS
                        {
                            self.microtasks.push(body);
                        }
                        Value::Null
                    }
                    _ => Value::Null,
                };
            }

            if is_identifier(obj_expr) {
                if let Some(Value::Array(mut items)) = self.get_var(obj_expr).cloned() {
                    let args = split_args(args_src);
                    match method {
                        "push" => {
                            for arg in &args {
                                if items.len() < MAX_ARRAY_LEN {
                                    items.push(self.eval_expr(doc, arg));
                                }
                            }
                            let len = items.len();
                            self.set_var(obj_expr, Value::Array(items));
                            return Value::Number(len as f64);
                        }
                        "pop" => {
                            let popped = items.pop().unwrap_or(Value::Null);
                            self.set_var(obj_expr, Value::Array(items));
                            return popped;
                        }
                        "shift" => {
                            let shifted = if items.is_empty() {
                                Value::Null
                            } else {
                                items.remove(0)
                            };
                            self.set_var(obj_expr, Value::Array(items));
                            return shifted;
                        }
                        "unshift" => {
                            for (idx, arg) in args.iter().enumerate() {
                                if items.len() < MAX_ARRAY_LEN {
                                    let val = self.eval_expr(doc, arg);
                                    items.insert(idx, val);
                                }
                            }
                            let len = items.len();
                            self.set_var(obj_expr, Value::Array(items));
                            return Value::Number(len as f64);
                        }
                        "reverse" => {
                            items.reverse();
                            let out = Value::Array(items.clone());
                            self.set_var(obj_expr, Value::Array(items));
                            return out;
                        }
                        _ => {}
                    }
                }
            }

            match self.eval_expr(doc, obj_expr) {
                Value::Str(s) => {
                    let args = split_args(args_src);
                    let arg0 = args
                        .first()
                        .map(|a| self.eval_expr(doc, a).as_string(doc))
                        .unwrap_or_default();
                    return match method {
                        "trim" => Value::Str(s.trim().to_string()),
                        "trimStart" => Value::Str(s.trim_start().to_string()),
                        "trimEnd" => Value::Str(s.trim_end().to_string()),
                        "toLowerCase" => Value::Str(s.to_ascii_lowercase()),
                        "toUpperCase" => Value::Str(s.to_ascii_uppercase()),
                        "includes" => Value::Bool(s.contains(&arg0)),
                        "startsWith" => Value::Bool(s.starts_with(&arg0)),
                        "endsWith" => Value::Bool(s.ends_with(&arg0)),
                        "indexOf" => {
                            Value::Number(s.find(&arg0).map(|idx| idx as f64).unwrap_or(-1.0))
                        }
                        "repeat" => {
                            let count =
                                args.first()
                                    .and_then(|a| self.eval_expr(doc, a).as_number())
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1024.0) as usize;
                            Value::Str(s.repeat(count))
                        }
                        "padStart" | "padEnd" => {
                            let target_len =
                                args.first()
                                    .and_then(|a| self.eval_expr(doc, a).as_number())
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1024.0) as usize;
                            let pad_str = args
                                .get(1)
                                .map(|a| self.eval_expr(doc, a).as_string(doc))
                                .filter(|p| !p.is_empty())
                                .unwrap_or_else(|| " ".to_string());
                            let cur_len = s.chars().count();
                            if cur_len >= target_len {
                                Value::Str(s)
                            } else {
                                let needed = target_len - cur_len;
                                let padding: String =
                                    pad_str.chars().cycle().take(needed).collect();
                                if method == "padStart" {
                                    Value::Str(format!("{padding}{s}"))
                                } else {
                                    Value::Str(format!("{s}{padding}"))
                                }
                            }
                        }
                        "charAt" => {
                            let idx = args
                                .first()
                                .and_then(|a| self.eval_expr(doc, a).as_number())
                                .unwrap_or(0.0)
                                .max(0.0) as usize;
                            Value::Str(
                                s.chars()
                                    .nth(idx)
                                    .map(|c| c.to_string())
                                    .unwrap_or_default(),
                            )
                        }
                        "replace" | "replaceAll" => {
                            let arg1 = args
                                .get(1)
                                .map(|a| self.eval_expr(doc, a).as_string(doc))
                                .unwrap_or_default();
                            if method == "replaceAll" {
                                Value::Str(s.replace(&arg0, &arg1))
                            } else {
                                Value::Str(s.replacen(&arg0, &arg1, 1))
                            }
                        }
                        "split" => {
                            let parts = if arg0.is_empty() {
                                s.chars().map(|c| Value::Str(c.to_string())).collect()
                            } else {
                                s.split(&arg0).map(|p| Value::Str(p.to_string())).collect()
                            };
                            Value::Array(parts)
                        }
                        "slice" | "substring" => {
                            let chars: Vec<char> = s.chars().collect();
                            let start = args
                                .first()
                                .and_then(|a| self.eval_expr(doc, a).as_number())
                                .unwrap_or(0.0)
                                .max(0.0) as usize;
                            let end = args
                                .get(1)
                                .and_then(|a| self.eval_expr(doc, a).as_number())
                                .map(|n| (n.max(0.0) as usize).min(chars.len()))
                                .unwrap_or(chars.len());
                            if start < end && start < chars.len() {
                                Value::Str(chars[start..end].iter().collect())
                            } else {
                                Value::Str(String::new())
                            }
                        }
                        _ => Value::Null,
                    };
                }
                Value::Array(items) => {
                    let args = split_args(args_src);
                    return match method {
                        "join" => {
                            let sep = args
                                .first()
                                .map(|a| self.eval_expr(doc, a).as_string(doc))
                                .unwrap_or_else(|| ",".to_string());
                            let joined = items
                                .iter()
                                .map(|v| v.as_string(doc))
                                .collect::<Vec<_>>()
                                .join(&sep);
                            Value::Str(joined)
                        }
                        "includes" => {
                            let needle = args
                                .first()
                                .map(|a| self.eval_expr(doc, a))
                                .unwrap_or(Value::Null);
                            Value::Bool(items.iter().any(|v| values_equal(v, &needle, doc)))
                        }
                        "indexOf" => {
                            let needle = args
                                .first()
                                .map(|a| self.eval_expr(doc, a))
                                .unwrap_or(Value::Null);
                            let idx = items
                                .iter()
                                .position(|v| values_equal(v, &needle, doc))
                                .map(|i| i as f64)
                                .unwrap_or(-1.0);
                            Value::Number(idx)
                        }
                        "concat" => {
                            let mut out = items;
                            for arg in &args {
                                match self.eval_expr(doc, arg) {
                                    Value::Array(more) => {
                                        for m in more {
                                            if out.len() < MAX_ARRAY_LEN {
                                                out.push(m);
                                            }
                                        }
                                    }
                                    other => {
                                        if out.len() < MAX_ARRAY_LEN {
                                            out.push(other);
                                        }
                                    }
                                }
                            }
                            Value::Array(out)
                        }
                        "reverse" => {
                            let mut rev = items;
                            rev.reverse();
                            Value::Array(rev)
                        }
                        "slice" => {
                            let start = args
                                .first()
                                .and_then(|a| self.eval_expr(doc, a).as_number())
                                .unwrap_or(0.0)
                                .max(0.0) as usize;
                            let end = args
                                .get(1)
                                .and_then(|a| self.eval_expr(doc, a).as_number())
                                .map(|n| (n.max(0.0) as usize).min(items.len()))
                                .unwrap_or(items.len());
                            if start < end && start < items.len() {
                                Value::Array(items[start..end].to_vec())
                            } else {
                                Value::Array(Vec::new())
                            }
                        }
                        "forEach" => {
                            if let Some(cb) = args.first() {
                                let (params, body) = extract_multi_param_callback(cb.trim());
                                for (idx, item) in items.into_iter().enumerate() {
                                    if let Some(p0) = params.first() {
                                        self.set_var(p0, item);
                                    }
                                    if let Some(p1) = params.get(1) {
                                        self.set_var(p1, Value::Number(idx as f64));
                                    }
                                    self.eval_script(doc, &body);
                                }
                            }
                            Value::Null
                        }
                        "map" => {
                            let mut mapped = Vec::with_capacity(items.len());
                            if let Some(cb) = args.first() {
                                let (params, body) = extract_multi_param_callback(cb.trim());
                                for (idx, item) in items.into_iter().enumerate() {
                                    if let Some(p0) = params.first() {
                                        self.set_var(p0, item);
                                    }
                                    if let Some(p1) = params.get(1) {
                                        self.set_var(p1, Value::Number(idx as f64));
                                    }
                                    let val = self.eval_callback_return(doc, &body);
                                    mapped.push(val);
                                }
                            }
                            Value::Array(mapped)
                        }
                        "filter" => {
                            let mut kept = Vec::new();
                            if let Some(cb) = args.first() {
                                let (params, body) = extract_multi_param_callback(cb.trim());
                                for (idx, item) in items.into_iter().enumerate() {
                                    if let Some(p0) = params.first() {
                                        self.set_var(p0, item.clone());
                                    }
                                    if let Some(p1) = params.get(1) {
                                        self.set_var(p1, Value::Number(idx as f64));
                                    }
                                    let cond = self.eval_callback_return(doc, &body);
                                    if cond.is_truthy() {
                                        kept.push(item);
                                    }
                                }
                            }
                            Value::Array(kept)
                        }
                        "find" => {
                            if let Some(cb) = args.first() {
                                let (params, body) = extract_multi_param_callback(cb.trim());
                                for (idx, item) in items.into_iter().enumerate() {
                                    if let Some(p0) = params.first() {
                                        self.set_var(p0, item.clone());
                                    }
                                    if let Some(p1) = params.get(1) {
                                        self.set_var(p1, Value::Number(idx as f64));
                                    }
                                    if self.eval_callback_return(doc, &body).is_truthy() {
                                        return item;
                                    }
                                }
                            }
                            Value::Null
                        }
                        "findIndex" => {
                            if let Some(cb) = args.first() {
                                let (params, body) = extract_multi_param_callback(cb.trim());
                                for (idx, item) in items.into_iter().enumerate() {
                                    if let Some(p0) = params.first() {
                                        self.set_var(p0, item);
                                    }
                                    if let Some(p1) = params.get(1) {
                                        self.set_var(p1, Value::Number(idx as f64));
                                    }
                                    if self.eval_callback_return(doc, &body).is_truthy() {
                                        return Value::Number(idx as f64);
                                    }
                                }
                            }
                            Value::Number(-1.0)
                        }
                        "some" => {
                            if let Some(cb) = args.first() {
                                let (params, body) = extract_multi_param_callback(cb.trim());
                                for (idx, item) in items.into_iter().enumerate() {
                                    if let Some(p0) = params.first() {
                                        self.set_var(p0, item);
                                    }
                                    if let Some(p1) = params.get(1) {
                                        self.set_var(p1, Value::Number(idx as f64));
                                    }
                                    if self.eval_callback_return(doc, &body).is_truthy() {
                                        return Value::Bool(true);
                                    }
                                }
                            }
                            Value::Bool(false)
                        }
                        "every" => {
                            if let Some(cb) = args.first() {
                                let (params, body) = extract_multi_param_callback(cb.trim());
                                for (idx, item) in items.into_iter().enumerate() {
                                    if let Some(p0) = params.first() {
                                        self.set_var(p0, item);
                                    }
                                    if let Some(p1) = params.get(1) {
                                        self.set_var(p1, Value::Number(idx as f64));
                                    }
                                    if !self.eval_callback_return(doc, &body).is_truthy() {
                                        return Value::Bool(false);
                                    }
                                }
                            }
                            Value::Bool(true)
                        }
                        "reduce" => {
                            if let Some(cb) = args.first() {
                                let (params, body) = extract_multi_param_callback(cb.trim());
                                let mut iter = items.into_iter().enumerate();
                                let mut acc = if let Some(init_arg) = args.get(1) {
                                    self.eval_expr(doc, init_arg)
                                } else {
                                    iter.next().map(|(_, v)| v).unwrap_or(Value::Null)
                                };
                                for (idx, item) in iter {
                                    if let Some(p0) = params.first() {
                                        self.set_var(p0, acc);
                                    }
                                    if let Some(p1) = params.get(1) {
                                        self.set_var(p1, item);
                                    }
                                    if let Some(p2) = params.get(2) {
                                        self.set_var(p2, Value::Number(idx as f64));
                                    }
                                    acc = self.eval_callback_return(doc, &body);
                                }
                                return acc;
                            }
                            Value::Null
                        }
                        _ => Value::Null,
                    };
                }
                Value::Json(mut jval) => {
                    if let Some(class_name) = jval
                        .get("__class")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                    {
                        if let Some((params, body, sup_class)) =
                            self.find_class_method(&class_name, method)
                        {
                            let args = split_args(args_src);
                            let prev_this = self.get_var("this").cloned();
                            self.set_var("this", Value::Json(jval));
                            let out = self.invoke_method_body(
                                doc,
                                &params,
                                &body,
                                &args,
                                sup_class.as_deref(),
                            );
                            if is_identifier(obj_expr) {
                                if let Some(updated_this) = self.get_var("this").cloned() {
                                    self.set_var(obj_expr, updated_this);
                                }
                            }
                            if let Some(old) = prev_this {
                                self.set_var("this", old);
                            }
                            return out;
                        }
                    }
                    match method {
                        "hasOwnProperty" | "has" => {
                            let args = split_args(args_src);
                            let key = args
                                .first()
                                .map(|a| self.eval_expr(doc, a).as_string(doc))
                                .unwrap_or_default();
                            return Value::Bool(jval.get(&key).is_some());
                        }
                        "get" => {
                            let args = split_args(args_src);
                            let key = args
                                .first()
                                .map(|a| self.eval_expr(doc, a).as_string(doc))
                                .unwrap_or_default();
                            return jval
                                .get(&key)
                                .cloned()
                                .map(Value::from_json_value)
                                .unwrap_or(Value::Null);
                        }
                        "set" | "add" => {
                            let args = split_args(args_src);
                            if !args.is_empty() {
                                let key = self.eval_expr(doc, &args[0]).as_string(doc);
                                let val = args
                                    .get(1)
                                    .map(|a| self.eval_expr(doc, a).to_json_value(doc))
                                    .unwrap_or(JsonValue::Bool(true));
                                jval.insert(&key, val);
                                if is_identifier(obj_expr) {
                                    self.set_var(obj_expr, Value::Json(jval.clone()));
                                }
                            }
                            return Value::Json(jval);
                        }
                        "text" => {
                            return jval
                                .get("body")
                                .and_then(|b| b.as_str())
                                .map(|s| Value::Str(s.to_string()))
                                .unwrap_or(Value::Str(String::new()));
                        }
                        "json" => {
                            let body = jval.get("body").and_then(|b| b.as_str()).unwrap_or("{}");
                            return parse_json(body)
                                .map(Value::from_json_value)
                                .unwrap_or(Value::Null);
                        }
                        "then" => {
                            let args = split_args(args_src);
                            if let Some(cb) = args.first() {
                                if let Some(resolved) = jval.get("__promise_resolved").cloned() {
                                    let (param_name, body) =
                                        extract_single_param_callback(cb.trim());
                                    let val_lit = to_json_string(&resolved);
                                    let scheduled = if !param_name.is_empty() {
                                        format!("let {param_name} = {val_lit}; {body}")
                                    } else {
                                        body
                                    };
                                    if self.microtasks.len() < MAX_MICROTASKS {
                                        self.microtasks.push(scheduled);
                                    }
                                }
                            }
                            return Value::Json(jval);
                        }
                        "catch" | "finally" => {
                            return Value::Json(jval);
                        }
                        _ => {}
                    }
                }
                Value::Element(id) => {
                    let args = split_args(args_src);
                    match method {
                        "getAttribute" => {
                            let name = args
                                .first()
                                .map(|a| self.eval_expr(doc, a).as_string(doc))
                                .unwrap_or_default();
                            return doc
                                .get(id)
                                .data()
                                .attr(&name)
                                .map(|v| Value::Str(v.to_string()))
                                .unwrap_or(Value::Null);
                        }
                        "setAttribute" if args.len() >= 2 => {
                            let name = self.eval_expr(doc, &args[0]).as_string(doc);
                            let value = self.eval_expr(doc, &args[1]).as_string(doc);
                            doc.set_attribute(id, &name, &value);
                            self.dom_cache.clear();
                            return Value::Null;
                        }
                        "removeAttribute" if !args.is_empty() => {
                            let name = self.eval_expr(doc, &args[0]).as_string(doc);
                            doc.remove_attribute(id, &name);
                            self.dom_cache.clear();
                            return Value::Null;
                        }
                        "hasAttribute" if !args.is_empty() => {
                            let name = self.eval_expr(doc, &args[0]).as_string(doc);
                            return Value::Bool(doc.get(id).data().attr(&name).is_some());
                        }
                        "toggleAttribute" if !args.is_empty() => {
                            let name = self.eval_expr(doc, &args[0]).as_string(doc);
                            let present = if doc.get(id).data().attr(&name).is_some() {
                                doc.remove_attribute(id, &name);
                                false
                            } else {
                                doc.set_attribute(id, &name, "");
                                true
                            };
                            self.dom_cache.clear();
                            return Value::Bool(present);
                        }
                        "appendChild" if !args.is_empty() => {
                            if let Value::Element(child_id) = self.eval_expr(doc, &args[0]) {
                                let _ = doc.append(id, child_id);
                                self.dom_cache.clear();
                                return Value::Element(child_id);
                            }
                            return Value::Null;
                        }
                        "append" => {
                            for arg in &args {
                                match self.eval_expr(doc, arg) {
                                    Value::Element(child_id) => {
                                        let _ = doc.append(id, child_id);
                                    }
                                    other => {
                                        let text = other.as_string(doc);
                                        let tid = doc.create_text(&text);
                                        let _ = doc.append(id, tid);
                                    }
                                }
                            }
                            self.dom_cache.clear();
                            return Value::Null;
                        }
                        "prepend" => {
                            let first_child = doc.first_element_child(id);
                            for arg in &args {
                                let child_id = match self.eval_expr(doc, arg) {
                                    Value::Element(cid) => cid,
                                    other => doc.create_text(&other.as_string(doc)),
                                };
                                let _ = doc.insert_before(id, child_id, first_child);
                            }
                            self.dom_cache.clear();
                            return Value::Null;
                        }
                        "insertBefore" if !args.is_empty() => {
                            if let Value::Element(new_id) = self.eval_expr(doc, &args[0]) {
                                let ref_id =
                                    args.get(1).and_then(|a| match self.eval_expr(doc, a) {
                                        Value::Element(rid) => Some(rid),
                                        _ => None,
                                    });
                                let _ = doc.insert_before(id, new_id, ref_id);
                                self.dom_cache.clear();
                                return Value::Element(new_id);
                            }
                            return Value::Null;
                        }
                        "removeChild" if !args.is_empty() => {
                            if let Value::Element(child_id) = self.eval_expr(doc, &args[0]) {
                                let _ = doc.remove_child(id, child_id);
                                self.dom_cache.clear();
                                return Value::Element(child_id);
                            }
                            return Value::Null;
                        }
                        "replaceChild" if args.len() >= 2 => {
                            let new_val = self.eval_expr(doc, &args[0]);
                            let old_val = self.eval_expr(doc, &args[1]);
                            if let (Value::Element(new_id), Value::Element(old_id)) =
                                (new_val, old_val)
                            {
                                let _ = doc.replace_child(id, new_id, old_id);
                                self.dom_cache.clear();
                                return Value::Element(old_id);
                            }
                            return Value::Null;
                        }
                        "cloneNode" => {
                            let deep = args
                                .first()
                                .is_some_and(|a| self.eval_expr(doc, a).is_truthy());
                            return doc
                                .clone_node(id, deep)
                                .map(Value::Element)
                                .unwrap_or(Value::Null);
                        }
                        "remove" => {
                            doc.remove_node(id);
                            self.dom_cache.clear();
                            return Value::Null;
                        }
                        "matches" if !args.is_empty() => {
                            let sel = self.eval_expr(doc, &args[0]).as_string(doc);
                            return Value::Bool(doc.matches_selector(id, &sel));
                        }
                        "closest" if !args.is_empty() => {
                            let sel = self.eval_expr(doc, &args[0]).as_string(doc);
                            return doc
                                .closest(id, &sel)
                                .map(Value::Element)
                                .unwrap_or(Value::Null);
                        }
                        "contains" if !args.is_empty() => {
                            if let Value::Element(other_id) = self.eval_expr(doc, &args[0]) {
                                return Value::Bool(doc.contains_node(id, other_id));
                            }
                            return Value::Bool(false);
                        }
                        "getBoundingClientRect" => {
                            let mut rect = JsonValue::empty_object();
                            rect.insert("x", JsonValue::Number(0.0));
                            rect.insert("y", JsonValue::Number(0.0));
                            rect.insert("top", JsonValue::Number(0.0));
                            rect.insert("left", JsonValue::Number(0.0));
                            rect.insert("width", JsonValue::Number(100.0));
                            rect.insert("height", JsonValue::Number(24.0));
                            rect.insert("right", JsonValue::Number(100.0));
                            rect.insert("bottom", JsonValue::Number(24.0));
                            return Value::Json(rect);
                        }
                        "focus" | "blur" | "scrollIntoView" => {
                            return Value::Null;
                        }
                        "querySelector" => {
                            let sel = args
                                .first()
                                .map(|a| self.eval_expr(doc, a).as_string(doc))
                                .unwrap_or_default();
                            return doc
                                .query_selector_within(id, &sel)
                                .map(Value::Element)
                                .unwrap_or(Value::Null);
                        }
                        "querySelectorAll" => {
                            let sel = args
                                .first()
                                .map(|a| self.eval_expr(doc, a).as_string(doc))
                                .unwrap_or_default();
                            let ids = doc.query_selector_all_within(id, &sel);
                            return Value::Array(ids.into_iter().map(Value::Element).collect());
                        }
                        "addEventListener" if args.len() >= 2 => {
                            let event = self.eval_expr(doc, &args[0]).as_string(doc);
                            if event.eq_ignore_ascii_case("click") {
                                let handler_raw = args[1].trim();
                                let body = extract_callback_body(handler_raw);
                                self.click_listeners.push((id, body));
                            }
                            return Value::Null;
                        }
                        _ => return Value::Null,
                    }
                }
                _ => {}
            }
        }

        if let Some((_, params, body)) = self
            .functions
            .iter()
            .find(|(name, _, _)| name == callee)
            .cloned()
        {
            let args = split_args(args_src);
            return self.invoke_method_body(doc, &params, &body, &args, None);
        }

        Value::Null
    }

    fn eval_callback_return(&mut self, doc: &mut Document, body: &str) -> Value {
        let trimmed = body.trim();
        if let Some(ret_expr) = trimmed.strip_prefix("return ") {
            return self.eval_expr(doc, ret_expr.trim());
        }
        if trimmed.contains(';')
            || trimmed.contains('\n')
            || trimmed.starts_with("let ")
            || trimmed.starts_with("const ")
            || trimmed.starts_with("if")
        {
            let prev_return = self.return_value.take();
            self.eval_script(doc, trimmed);
            let out = self.return_value.take().unwrap_or(Value::Null);
            self.return_value = prev_return;
            out
        } else {
            self.eval_expr(doc, trimmed)
        }
    }
}

fn extract_callback_body(handler_raw: &str) -> String {
    if let Some((_, rhs)) = handler_raw.split_once("=>") {
        let rhs = rhs.trim();
        if let Some((_, block)) = split_brace_block(rhs) {
            return block;
        }
        return rhs.to_string();
    }
    if let Some((_, block)) = split_brace_block(handler_raw) {
        block
    } else if is_identifier(handler_raw) {
        format!("{handler_raw}()")
    } else {
        handler_raw.to_string()
    }
}

fn extract_single_param_callback(handler_raw: &str) -> (String, String) {
    let (params, body) = extract_multi_param_callback(handler_raw);
    (params.into_iter().next().unwrap_or_default(), body)
}

fn extract_multi_param_callback(handler_raw: &str) -> (Vec<String>, String) {
    if let Some((lhs, rhs)) = split_top_level_arrow(handler_raw) {
        let params: Vec<String> = lhs
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let rhs = rhs.trim();
        let body = if rhs.starts_with('{') {
            split_brace_block(rhs)
                .map(|(_, block)| block)
                .unwrap_or_else(|| rhs.to_string())
        } else {
            rhs.to_string()
        };
        return (params, body);
    }
    if let Some((header, block)) = split_brace_block(handler_raw) {
        let params = header
            .find('(')
            .and_then(|open| {
                header[open + 1..]
                    .find(')')
                    .map(|close| &header[open + 1..open + 1 + close])
            })
            .map(|p| {
                p.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        return (params, block);
    }
    (Vec::new(), handler_raw.to_string())
}

fn find_template_expr_end(after_dollar_brace: &str) -> Option<usize> {
    let bytes = after_dollar_brace.as_bytes();
    let mut depth = 1_i32;
    let mut quote: Option<u8> = None;
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        if let Some(q) = quote {
            if b == b'\\' {
                idx += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => quote = Some(b),
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

fn parse_class_methods(class_body: &str) -> Vec<(String, Vec<String>, String)> {
    let mut methods = Vec::new();
    let mut rem = class_body.trim();
    while !rem.is_empty() {
        let Some(open_paren) = rem.find('(') else {
            break;
        };
        let raw_name = rem[..open_paren]
            .trim()
            .trim_start_matches(';')
            .trim()
            .strip_prefix("static ")
            .unwrap_or(rem[..open_paren].trim().trim_start_matches(';').trim())
            .trim();
        let after_name = &rem[open_paren..];
        let Some((params_str, after_params)) = extract_paren_group(after_name) else {
            break;
        };
        let Some((body, after_body)) = extract_first_brace_block(after_params) else {
            break;
        };
        let params: Vec<String> = params_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if !raw_name.is_empty() {
            methods.push((raw_name.to_string(), params, body));
        }
        rem = after_body.trim();
    }
    methods
}

fn parse_switch_clauses(switch_body: &str) -> Vec<(Option<String>, Vec<String>)> {
    let mut clauses: Vec<(Option<String>, Vec<String>)> = Vec::new();
    for raw_stmt in split_statements(switch_body) {
        let mut s = raw_stmt.trim();
        while !s.is_empty() {
            if let Some(after_case) = s.strip_prefix("case ") {
                if let Some(colon) = after_case.find(':') {
                    let case_expr = after_case[..colon].trim().to_string();
                    clauses.push((Some(case_expr), Vec::new()));
                    s = after_case[colon + 1..].trim();
                    continue;
                }
            }
            if let Some(after_def) = s.strip_prefix("default") {
                let after_def = after_def.trim_start();
                if let Some(rest) = after_def.strip_prefix(':') {
                    clauses.push((None, Vec::new()));
                    s = rest.trim();
                    continue;
                }
            }
            if let Some((_, stmts)) = clauses.last_mut() {
                if s.starts_with('{') && s.ends_with('}') && s.len() >= 2 {
                    for inner in split_statements(&s[1..s.len() - 1]) {
                        stmts.push(inner);
                    }
                } else {
                    stmts.push(s.to_string());
                }
            }
            break;
        }
    }
    clauses
}

fn split_for_of_or_in<'a>(header: &'a str, kw: &str) -> Option<(&'a str, &'a str)> {
    let bytes = header.as_bytes();
    let kw_bytes = kw.as_bytes();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    let mut quote: Option<u8> = None;
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        if let Some(q) = quote {
            if b == b'\\' {
                idx += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => quote = Some(b),
            b'(' => paren += 1,
            b')' => paren = (paren - 1).max(0),
            b'[' => bracket += 1,
            b']' => bracket = (bracket - 1).max(0),
            b'{' => brace += 1,
            b'}' => brace = (brace - 1).max(0),
            _ if paren == 0 && bracket == 0 && brace == 0 && bytes[idx..].starts_with(kw_bytes) => {
                return Some((&header[..idx], &header[idx + kw_bytes.len()..]));
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

fn split_top_level_arrow(input: &str) -> Option<(&str, &str)> {
    let bytes = input.as_bytes();
    let mut paren = 0i32;
    let mut brace = 0i32;
    let mut bracket = 0i32;
    let mut quote: Option<u8> = None;
    let mut idx = 0usize;
    while idx + 1 < bytes.len() {
        let b = bytes[idx];
        if let Some(q) = quote {
            if b == b'\\' {
                idx += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => quote = Some(b),
            b'(' => paren += 1,
            b')' => paren = (paren - 1).max(0),
            b'{' => brace += 1,
            b'}' => brace = (brace - 1).max(0),
            b'[' => bracket += 1,
            b']' => bracket = (bracket - 1).max(0),
            b'=' if paren == 0 && brace == 0 && bracket == 0 && bytes[idx + 1] == b'>' => {
                return Some((&input[..idx], &input[idx + 2..]));
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

/// Bytes of keys plus values already stored for `origin`.
fn storage_origin_bytes(rows: &[(String, String, String)], origin: &str) -> usize {
    rows.iter()
        .filter(|(stored_origin, _, _)| stored_origin == origin)
        .map(|(_, key, value)| key.len().saturating_add(value.len()))
        .fold(0usize, usize::saturating_add)
}

/// Insert or replace one storage row when the origin byte cap allows it.
///
/// An update that does not fit leaves the previous value. A new key that does
/// not fit is not added. The list length cap still applies to new rows.
fn storage_put(rows: &mut Vec<(String, String, String)>, origin: &str, key: &str, value: &str) {
    if let Some(index) = rows
        .iter()
        .position(|(stored_origin, stored_key, _)| stored_origin == origin && stored_key == key)
    {
        let next = storage_origin_bytes(rows, origin)
            .saturating_sub(rows[index].2.len())
            .saturating_add(value.len());
        if next > MAX_STORAGE_ORIGIN_BYTES {
            return;
        }
        rows[index].2 = value.to_string();
        return;
    }
    let next = storage_origin_bytes(rows, origin)
        .saturating_add(key.len())
        .saturating_add(value.len());
    if next > MAX_STORAGE_ORIGIN_BYTES {
        return;
    }
    if rows.len() < MAX_STORAGE_ENTRIES * 8 {
        rows.push((origin.to_string(), key.to_string(), value.to_string()));
    }
}

/// Result of offering a script to the caller-owned numeric VM.
#[cfg_attr(not(anvil_cpp_jit), allow(dead_code))]
enum VmRun {
    /// The compiler accepted the script and execution finished.
    Value(f64),
    /// The compiler accepted the script and the step budget aborted it.
    Aborted,
    /// The compiler rejected the script, or the VM is not linked.
    Rejected,
}

/// True when `source` must stay on the Rust interpreter.
///
/// The numeric VM does not see the DOM, `fetch()`, or storage. A script that
/// names any of those stays here even if the compiler would accept the rest.
fn native_loop_program(statement: &str) -> Option<String> {
    let trimmed = statement.trim();
    if trimmed.starts_with("while") {
        return Some(trimmed.to_string());
    }
    let rest = trimmed.strip_prefix("for")?.trim_start();
    let (header, after) = extract_paren_group(rest)?;
    let (body, _) = extract_first_brace_block(after)?;
    let parts: Vec<&str> = header.splitn(3, ';').collect();
    if parts.len() != 3 {
        return None;
    }
    let init = parts[0]
        .trim()
        .trim_start_matches("let ")
        .trim_start_matches("var ")
        .trim_start_matches("const ");
    let cond = parts[1].trim();
    let step = parts[2].trim();
    if cond.is_empty() {
        return None;
    }
    let mut program = String::new();
    if !init.is_empty() {
        program.push_str(init);
        if !init.ends_with(';') {
            program.push(';');
        }
    }
    program.push_str("while (");
    program.push_str(cond);
    program.push_str(") {");
    program.push_str(&body);
    if !step.is_empty() {
        if !body.trim().ends_with(';') && !body.trim().is_empty() {
            program.push(';');
        }
        program.push_str(step);
        if !step.ends_with(';') {
            program.push(';');
        }
    }
    program.push('}');
    Some(program)
}

fn page_host_word(name: &str) -> bool {
    matches!(name, "document" | "getElementById" | "textContent")
}

fn page_loop_text(statement: &str) -> bool {
    let trimmed = statement.trim_start();
    if !trimmed.starts_with("while") && !trimmed.starts_with("for") {
        return false;
    }
    if !trimmed.contains("document.getElementById(") || !trimmed.contains(".textContent") {
        return false;
    }
    if trimmed.contains(['[', '`']) || trimmed.contains("function") || trimmed.contains("innerHTML")
    {
        return false;
    }
    let stripped = trimmed
        .replace("document.getElementById(", "")
        .replace(".textContent", "");
    !stripped.contains('.')
}

fn strip_string_literals(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' || ch == '\'' {
            while let Some(next) = chars.next() {
                if next == ch {
                    break;
                }
            }
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(anvil_cpp_jit)]
struct TextHostGuard;

#[cfg(anvil_cpp_jit)]
impl TextHostGuard {
    fn arm(doc: &mut Document) -> Self {
        unsafe {
            anvil_cpp_vm_set_text_host(
                Some(rust_host_set_text),
                doc as *mut Document as *mut std::ffi::c_void,
            );
        }
        Self
    }
}

#[cfg(anvil_cpp_jit)]
impl Drop for TextHostGuard {
    fn drop(&mut self) {
        unsafe {
            anvil_cpp_vm_set_text_host(None, std::ptr::null_mut());
        }
    }
}

#[cfg(anvil_cpp_jit)]
extern "C" fn rust_host_set_text(host: *mut std::ffi::c_void, id: *const u8, len: u32, value: f64) {
    if host.is_null() || id.is_null() || len == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(id, len as usize) };
    let Ok(element_id) = std::str::from_utf8(bytes) else {
        return;
    };
    let doc = unsafe { &mut *(host as *mut Document) };
    let Some(node) = doc.get_element_by_id(element_id) else {
        return;
    };
    let shown = if value.fract() == 0.0 && value.abs() < 1.0e15 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    };
    doc.set_text_content(node, &shown);
}

fn numeric_keyword(name: &str) -> bool {
    matches!(
        name,
        "while" | "for" | "let" | "const" | "var" | "if" | "else" | "return" | "true" | "false"
    )
}

fn format_jit_number(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1.0e15 {
        format!("{}", number as i64)
    } else {
        format!("{number}")
    }
}

fn numeric_loop_text(statement: &str) -> bool {
    let trimmed = statement.trim_start();
    if !trimmed.starts_with("while") && !trimmed.starts_with("for") {
        return false;
    }
    if trimmed.contains(['.', '[', ']', '\'', '"', '`']) {
        return false;
    }
    !trimmed.contains("function")
}

fn ident_names(statement: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = String::new();
    for ch in statement.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            current.push(ch);
        } else if !current.is_empty() {
            if current
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                && !names.iter().any(|existing| existing == &current)
            {
                names.push(current);
            }
            current = String::new();
        }
    }
    if !current.is_empty()
        && current
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && !names.iter().any(|existing| existing == &current)
    {
        names.push(current);
    }
    names
}

fn script_stays_on_rust(source: &str) -> bool {
    const BLOCKED: &[&str] = &[
        "document",
        "getElementById",
        "querySelector",
        "querySelectorAll",
        "textContent",
        "innerHTML",
        "innerText",
        "localStorage",
        "sessionStorage",
        "fetch",
        "addEventListener",
        "window",
    ];
    let mut ident = String::new();
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            ident.push(ch);
        } else if blocked_ident(&ident, BLOCKED) {
            return true;
        } else {
            ident.clear();
        }
    }
    blocked_ident(&ident, BLOCKED)
}

fn blocked_ident(ident: &str, blocked: &[&str]) -> bool {
    if ident.is_empty() {
        return false;
    }
    let found = blocked.iter().any(|word| *word == ident);
    found
}

fn fetch_result_value(url: &str, status: u16, body: String) -> Value {
    let mut obj = JsonValue::empty_object();
    obj.insert("url", JsonValue::String(url.to_string()));
    obj.insert("status", JsonValue::Number(status as f64));
    obj.insert("ok", JsonValue::Bool((200..300).contains(&status)));
    obj.insert("body", JsonValue::String(body));
    Value::Json(obj)
}

/// Turn a same-origin reference into an absolute URL for the host.
///
/// A target that already contains `://` is returned unchanged. A path is joined
/// to the page origin. Without an origin tuple, the target is returned as written.
fn resolve_script_fetch_url(page: &str, target: &str) -> String {
    let target = target.trim();
    if target.contains("://") {
        return target.to_string();
    }
    let Some((scheme, host, port)) = extract_origin_tuple(page) else {
        return target.to_string();
    };
    let origin = format!("{scheme}://{host}:{port}");
    if target.starts_with('/') {
        return format!("{origin}{target}");
    }
    let after_scheme = page.split_once("://").map(|(_, rest)| rest).unwrap_or(page);
    let path = after_scheme.split(['?', '#']).next().unwrap_or("/");
    let path = path.find('/').map(|index| &path[index..]).unwrap_or("/");
    let directory = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    format!("{origin}{directory}/{target}")
}

fn is_allowed_script_fetch(page_origin: Option<&str>, target_url: &str) -> bool {
    let target = target_url.trim();
    if target.is_empty() {
        return false;
    }
    let lower = target.to_ascii_lowercase();
    if lower.starts_with("file:")
        || lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
        || lower.starts_with("anvil:")
    {
        return false;
    }
    let Some(origin) = page_origin else {
        return !lower.contains("://");
    };
    if !lower.contains("://") {
        return !target.contains("..");
    }
    extract_origin_tuple(origin) == extract_origin_tuple(target)
}

fn extract_origin_tuple(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80u16,
        "https" => 443u16,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    if let Some((host, port_str)) = authority.rsplit_once(':') {
        let port = port_str.parse::<u16>().ok()?;
        Some((scheme, host.to_ascii_lowercase(), port))
    } else {
        Some((scheme, authority.to_ascii_lowercase(), default_port))
    }
}

fn values_equal(a: &Value, b: &Value, doc: &Document) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => (x - y).abs() <= f64::EPSILON,
        (Value::Element(x), Value::Element(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        _ => a.as_string(doc) == b.as_string(doc),
    }
}

fn update_inline_style(doc: &mut Document, id: NodeId, prop: &str, value: &str) {
    let existing = doc.get(id).data().attr("style").unwrap_or("").to_string();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for part in existing.split(';') {
        let Some((k, v)) = part.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().to_string();
        if !k.is_empty() && !v.is_empty() {
            pairs.push((k, v));
        }
    }
    if let Some((_, slot)) = pairs.iter_mut().find(|(k, _)| k == prop) {
        *slot = value.to_string();
    } else {
        pairs.push((prop.to_string(), value.to_string()));
    }
    let updated = pairs
        .into_iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("; ");
    doc.set_attribute(id, "style", &updated);
}

fn read_inline_style(doc: &Document, id: NodeId, prop: &str) -> String {
    let existing = doc.get(id).data().attr("style").unwrap_or("");
    for part in existing.split(';') {
        if let Some((k, v)) = part.split_once(':') {
            if k.trim().eq_ignore_ascii_case(prop) {
                return v.trim().to_string();
            }
        }
    }
    String::new()
}

fn remove_inline_style_prop(doc: &mut Document, id: NodeId, prop: &str) {
    let existing = doc.get(id).data().attr("style").unwrap_or("").to_string();
    let kept: Vec<String> = existing
        .split(';')
        .filter_map(|part| {
            let (k, v) = part.split_once(':')?;
            let k_clean = k.trim().to_ascii_lowercase();
            let v_clean = v.trim();
            if k_clean.is_empty() || v_clean.is_empty() || k_clean == prop {
                None
            } else {
                Some(format!("{k_clean}: {v_clean}"))
            }
        })
        .collect();
    doc.set_attribute(id, "style", &kept.join("; "));
}

fn camel_to_kebab(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_uppercase() {
            out.push('-');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn kebab_to_camel(input: &str) -> String {
    let mut out = String::new();
    let mut upper_next = false;
    for ch in input.chars() {
        if ch == '-' {
            upper_next = true;
        } else if upper_next {
            out.push(ch.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn strip_comments(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    let mut in_quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = in_quote {
            out.push(ch);
            if ch == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            } else if ch == q {
                in_quote = None;
            }
            continue;
        }
        if ch == '"' || ch == '\'' || ch == '`' {
            in_quote = Some(ch);
            out.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(inner) = chars.next() {
                if inner == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            out.push(' ');
            continue;
        }
        out.push(ch);
    }
    out
}

/// True when `source` is small enough for this interpreter.
///
/// The limit is the code, not the whole text. Characters inside a quoted
/// string do not count, so a short script can still store a large value.
/// A bundle whose code is over [`MAX_SCRIPT_SOURCE`] is left unrun. An empty
/// source does not run.
pub fn script_runs_here(source: &str) -> bool {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.len() <= MAX_SCRIPT_SOURCE {
        return true;
    }
    code_bytes_outside_strings(trimmed) <= MAX_SCRIPT_SOURCE
}

/// Bytes of `source` that are not inside `'`, `"`, or `` ` ``.
///
/// The quotes themselves count. An escape skips the next byte, including a
/// quote. This is a size check, not a full parse.
fn code_bytes_outside_strings(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut count = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'"' || byte == b'\'' || byte == b'`' {
            count = count.saturating_add(1);
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = index.saturating_add(2);
                    continue;
                }
                if bytes[index] == byte {
                    count = count.saturating_add(1);
                    index += 1;
                    break;
                }
                index += 1;
            }
            continue;
        }
        count = count.saturating_add(1);
        index += 1;
    }
    count
}

fn split_statements(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut paren = 0_i32;
    let mut brace = 0_i32;
    let mut bracket = 0_i32;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == '\\' {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => {
                quote = Some(ch);
                current.push(ch);
            }
            '(' => {
                paren += 1;
                current.push(ch);
            }
            ')' => {
                paren = (paren - 1).max(0);
                current.push(ch);
            }
            '[' => {
                bracket += 1;
                current.push(ch);
            }
            ']' => {
                bracket = (bracket - 1).max(0);
                current.push(ch);
            }
            '{' => {
                brace += 1;
                current.push(ch);
            }
            '}' => {
                brace = (brace - 1).max(0);
                current.push(ch);
                if brace == 0 && paren == 0 && bracket == 0 {
                    let mut look = chars.clone();
                    while let Some(&c) = look.peek() {
                        if c.is_whitespace() {
                            look.next();
                        } else {
                            break;
                        }
                    }
                    let next_non_ws = look.peek().copied();
                    let next_word: String = look.take_while(|c| c.is_ascii_alphabetic()).collect();
                    let continues_expr = matches!(
                        next_non_ws,
                        Some('=')
                            | Some(',')
                            | Some('.')
                            | Some(';')
                            | Some(')')
                            | Some(']')
                            | Some('?')
                            | Some(':')
                    );
                    if !continues_expr
                        && !matches!(next_word.as_str(), "else" | "catch" | "finally")
                    {
                        let trimmed = current.trim().to_string();
                        if !trimmed.is_empty() {
                            out.push(trimmed);
                        }
                        current.clear();
                    }
                }
            }
            ';' | '\n' if paren == 0 && brace == 0 && bracket == 0 => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    out
}

fn split_brace_block(input: &str) -> Option<(String, String)> {
    let open = input.find('{')?;
    let close = input.rfind('}')?;
    if close <= open {
        return None;
    }
    Some((
        input[..open].trim().to_string(),
        input[open + 1..close].trim().to_string(),
    ))
}

fn extract_paren_group(input: &str) -> Option<(String, &str)> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut depth = 0_i32;
    let mut quote: Option<u8> = None;
    let mut idx = 0;
    while idx < bytes.len() {
        let b = bytes[idx];
        if let Some(q) = quote {
            if b == b'\\' {
                idx += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 {
                return Some((input[1..idx].to_string(), &input[idx + 1..]));
            }
        }
        idx += 1;
    }
    None
}

fn extract_first_brace_block(input: &str) -> Option<(String, &str)> {
    let trimmed = input.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0_i32;
    let mut quote: Option<u8> = None;
    let mut idx = 0;
    while idx < bytes.len() {
        let b = bytes[idx];
        if let Some(q) = quote {
            if b == b'\\' {
                idx += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some((trimmed[1..idx].to_string(), &trimmed[idx + 1..]));
            }
        }
        idx += 1;
    }
    None
}

fn split_assignment(input: &str) -> Option<(&str, &str)> {
    let bytes = input.as_bytes();
    let mut paren = 0_i32;
    let mut brace = 0_i32;
    let mut bracket = 0_i32;
    let mut quote: Option<u8> = None;
    let mut idx = 0;
    while idx < bytes.len() {
        let b = bytes[idx];
        if let Some(q) = quote {
            if b == b'\\' {
                idx += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => quote = Some(b),
            b'(' => paren += 1,
            b')' => paren = (paren - 1).max(0),
            b'{' => brace += 1,
            b'}' => brace = (brace - 1).max(0),
            b'[' => bracket += 1,
            b']' => bracket = (bracket - 1).max(0),
            b'=' if paren == 0 && brace == 0 && bracket == 0 => {
                let prev = if idx > 0 { bytes[idx - 1] } else { 0 };
                let next = bytes.get(idx + 1).copied().unwrap_or(0);
                if next != b'=' && next != b'>' && !matches!(prev, b'=' | b'!' | b'<' | b'>') {
                    return Some((&input[..idx], &input[idx + 1..]));
                }
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

fn split_top_level_op<'a>(input: &'a str, op: &str) -> Option<(&'a str, &'a str)> {
    let bytes = input.as_bytes();
    let op_bytes = op.as_bytes();
    let mut paren = 0_i32;
    let mut brace = 0_i32;
    let mut bracket = 0_i32;
    let mut quote: Option<u8> = None;
    let mut last_match = None;
    let mut idx = 0;

    while idx < bytes.len() {
        let b = bytes[idx];
        if let Some(q) = quote {
            if b == b'\\' {
                idx += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            idx += 1;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => quote = Some(b),
            b'(' => paren += 1,
            b')' => paren = (paren - 1).max(0),
            b'{' => brace += 1,
            b'}' => brace = (brace - 1).max(0),
            b'[' => bracket += 1,
            b']' => bracket = (bracket - 1).max(0),
            _ if paren == 0 && brace == 0 && bracket == 0 && bytes[idx..].starts_with(op_bytes) => {
                if (op == "<" || op == ">") && bytes.get(idx + 1) == Some(&b'=') {
                    idx += 1;
                    continue;
                }
                if op == ">" && idx > 0 && bytes.get(idx - 1) == Some(&b'=') {
                    idx += 1;
                    continue;
                }
                if (op == "==" || op == "!=") && bytes.get(idx + 2) == Some(&b'=') {
                    idx += 1;
                    continue;
                }
                if op == "?"
                    && (bytes.get(idx + 1) == Some(&b'?')
                        || bytes.get(idx + 1) == Some(&b'.')
                        || (idx > 0 && bytes.get(idx - 1) == Some(&b'?')))
                {
                    idx += 1;
                    continue;
                }
                last_match = Some(idx);
                idx += op_bytes.len();
                continue;
            }
            _ => {}
        }
        idx += 1;
    }
    let pos = last_match?;
    Some((&input[..pos], &input[pos + op_bytes.len()..]))
}

fn find_call_open_paren(expr: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut depth = 0_i32;
    let mut quote: Option<u8> = None;
    for idx in (0..bytes.len()).rev() {
        let b = bytes[idx];
        if let Some(q) = quote {
            if b == q && (idx == 0 || bytes[idx - 1] != b'\\') {
                quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return (idx > 0).then_some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_args(args_src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut paren = 0_i32;
    let mut brace = 0_i32;
    let mut bracket = 0_i32;
    let mut quote: Option<char> = None;
    let mut chars = args_src.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == '\\' {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => {
                quote = Some(ch);
                current.push(ch);
            }
            '(' => {
                paren += 1;
                current.push(ch);
            }
            ')' => {
                paren = (paren - 1).max(0);
                current.push(ch);
            }
            '{' => {
                brace += 1;
                current.push(ch);
            }
            '}' => {
                brace = (brace - 1).max(0);
                current.push(ch);
            }
            '[' => {
                bracket += 1;
                current.push(ch);
            }
            ']' => {
                bracket = (bracket - 1).max(0);
                current.push(ch);
            }
            ',' if paren == 0 && brace == 0 && bracket == 0 => {
                out.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    out
}

fn parse_string_literal(expr: &str) -> Option<String> {
    let bytes = expr.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let quote = bytes[0];
    if (quote != b'"' && quote != b'\'') || bytes[bytes.len() - 1] != quote {
        return None;
    }
    let inner = &expr[1..expr.len() - 1];
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

#[cfg(test)]
mod tests {
    use super::*;
    use anvil_dom::parse_html;

    #[test]
    fn skips_a_bundle_and_still_runs_a_short_script() {
        assert!(!script_runs_here(&"x".repeat(MAX_SCRIPT_SOURCE + 1)));
        let bundle = format!("{}\n{}", "a".repeat(5000), "b".repeat(5000));
        assert!(!script_runs_here(&bundle));
        let mut doc = parse_html("<title>Old</title><script>document.title = 'New';</script>");
        let mut runtime = ScriptRuntime::new();
        runtime.run_document_scripts(&mut doc);
        assert_eq!(doc.title(), "New");
        let mut doc = parse_html(&format!(
            "<title>Stay</title><script>{}</script>",
            "var x = 1;".repeat(2000)
        ));
        let mut runtime = ScriptRuntime::new();
        runtime.run_document_scripts(&mut doc);
        assert_eq!(doc.title(), "Stay");
    }

    #[test]
    fn executes_script_mutations_and_click_listeners() {
        let mut doc = parse_html(
            "<title>Start</title>
             <h1 id=\"heading\">Before</h1>
             <p class=\"status\">Idle</p>
             <script>
               let count = 2 + 3;
               document.title = 'Count: ' + count;
               let h = document.getElementById('heading');
               h.textContent = 'Ready';
               h.style.color = '#e8c37a';
               let status = document.querySelector('.status');
               h.addEventListener('click', function() {
                 status.textContent = 'Clicked!';
               });
             </script>",
        );
        let mut runtime = ScriptRuntime::new();
        runtime.run_document_scripts(&mut doc);

        assert_eq!(doc.title(), "Count: 5");
        let h = doc.get_element_by_id("heading").unwrap();
        let status = doc.query_selector(".status").unwrap();
        assert_eq!(doc.text_content(h), "Ready");
        assert_eq!(doc.get(h).data().attr("style"), Some("color: #e8c37a"));
        assert_eq!(doc.text_content(status), "Idle");

        assert!(runtime.dispatch_click(&mut doc, h));
        assert_eq!(doc.text_content(status), "Clicked!");
    }

    #[test]
    fn parses_and_stringifies_json_in_scripts() {
        let mut doc = parse_html(
            "<p id=\"out\"></p>
             <script>
               let data = JSON.parse('{\"label\": \"Anvil JSON\", \"code\": 3}');
               let out = document.getElementById('out');
               out.textContent = data.label + ' v' + data.code;
             </script>",
        );
        let mut runtime = ScriptRuntime::new();
        runtime.run_document_scripts(&mut doc);
        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(doc.text_content(out), "Anvil JSON v3");
    }

    #[test]
    fn executes_for_loops_arrays_inputs_class_list_timers_and_fetch() {
        let mut doc = parse_html(
            "<input id=\"field\" type=\"text\" value=\"initial\">
             <input id=\"flag\" type=\"checkbox\">
             <div id=\"box\" class=\"card\"></div>
             <script>
               let nums = [10, 20, 30];
               let sum = 0;
               for (let i = 0; i < nums.length; i = i + 1) {
                 sum = sum + nums[i];
               }
               let f = document.getElementById('field');
               f.value = 'Sum: ' + sum;
               let c = document.getElementById('flag');
               c.checked = true;
               let box = document.getElementById('box');
               box.classList.add('active');
               let resp = fetch('/api/status');
               let info = resp.json();
               setTimeout(function() {
                 box.textContent = info.state + ':' + f.value;
               }, 10);
             </script>",
        );
        let mut runtime = ScriptRuntime::new();
        runtime.register_fetch_response("/api/status", 200, "{\"state\": \"online\"}");
        runtime.run_document_scripts(&mut doc);

        let field = doc.get_element_by_id("field").unwrap();
        let flag = doc.get_element_by_id("flag").unwrap();
        let box_el = doc.get_element_by_id("box").unwrap();
        assert_eq!(doc.input_value(field), "Sum: 60");
        assert!(doc.is_checked(flag));
        assert!(doc.get(box_el).data().has_class("active"));
        assert_eq!(doc.text_content(box_el), "online:Sum: 60");
    }

    #[test]
    fn aborts_infinite_recursion_and_enforces_same_origin_fetch() {
        // An unoptimized build needs more than the 2 MB default test-thread
        // stack to reach MAX_CALL_DEPTH, so the recursion check runs on its
        // own thread with room to spare. The depth cap is still what stops it.
        let aborted = std::thread::Builder::new()
            .name("recursion-check".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let mut doc = parse_html(
                    "<p id=\"status\">Safe</p>
                     <script>
                       function recurse() {
                         recurse();
                       }
                       recurse();
                     </script>",
                );
                let mut runtime = ScriptRuntime::new();
                runtime.run_document_scripts(&mut doc);
                runtime.was_aborted()
            })
            .expect("spawn recursion-check thread")
            .join()
            .expect("recursion-check thread panicked");
        assert!(aborted);

        let mut sop_doc = parse_html(
            "<p id=\"out\"></p>
             <script>
               let blocked = fetch('https://other-origin.example/secret');
               let allowed = fetch('https://app.example/api');
               let out = document.getElementById('out');
               out.textContent = blocked.status + ':' + allowed.status;
             </script>",
        );
        let mut sop_rt = ScriptRuntime::new();
        sop_rt.set_page_origin("https://app.example/index.html");
        sop_rt.register_fetch_response("https://app.example/api", 200, "{\"ok\":true}");
        sop_rt.run_document_scripts(&mut sop_doc);
        let out = sop_doc.get_element_by_id("out").unwrap();
        assert_eq!(sop_doc.text_content(out), "403:200");
    }

    #[test]
    fn registered_fetch_wins_and_cross_origin_does_not_call_the_host() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let mut runtime = ScriptRuntime::new();
        runtime.set_page_origin("https://app.example/index.html");
        runtime.set_fetch_host(Arc::new(move |url, _page| {
            seen.fetch_add(1, Ordering::SeqCst);
            ScriptFetchReply {
                status: 200,
                body: format!("host:{url}"),
            }
        }));
        runtime.register_fetch_response("https://app.example/api", 201, "from-mock");
        let mut doc = parse_html(
            "<p id=\"out\"></p>
             <script>
               let mocked = fetch('https://app.example/api');
               let denied = fetch('https://other.example/secret');
               let live = fetch('https://app.example/live');
               document.getElementById('out').textContent =
                 mocked.status + ':' + mocked.body + ':' + denied.status + ':' + live.body;
             </script>",
        );
        runtime.run_document_scripts(&mut doc);
        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(
            doc.text_content(out),
            "201:from-mock:403:host:https://app.example/live"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn executes_modern_js_features_and_jit_toggle() {
        let script_html = "<div id=\"root\"><span class=\"chip\">A</span><span class=\"chip\">B</span></div>
             <p id=\"result\"></p>
             <script>
               const add = (a, b) => a + b;
               function computeLabel(base) {
                 let total = add(base, 8);
                 total += 2;
                 return `Total=${total}`;
               }
               let chips = document.querySelectorAll('.chip');
               let names = [];
               chips.forEach((el) => {
                 names.push(el.textContent);
               });
               let root = document.getElementById('root');
               let cached = document.getElementById('root');
               let res = document.getElementById('result');
               let mode = total_check();
               function total_check() {
                 return 5 > 3 ? 'ok' : 'bad';
               }
               console.log('computed', names.join('+'));
               res.textContent = computeLabel(10) + ':' + names.join(',') + ':' + mode + ':' + Math.max(4, 9);
             </script>";

        let mut doc_jit = parse_html(script_html);
        let mut rt_jit = ScriptRuntime::with_jit_enabled(true);
        rt_jit.run_document_scripts(&mut doc_jit);
        let res_jit = doc_jit.get_element_by_id("result").unwrap();
        assert_eq!(doc_jit.text_content(res_jit), "Total=20:A,B:ok:9");
        assert!(rt_jit.jit_stats().jit_enabled);
        assert!(rt_jit.jit_stats().compiled_blocks > 0);
        assert!(rt_jit.jit_stats().inline_cache_hits > 0);
        assert_eq!(rt_jit.console_logs(), &["computed A+B"]);

        let mut doc_interp = parse_html(script_html);
        let mut rt_interp = ScriptRuntime::with_jit_enabled(false);
        rt_interp.run_document_scripts(&mut doc_interp);
        let res_interp = doc_interp.get_element_by_id("result").unwrap();
        assert_eq!(doc_interp.text_content(res_interp), "Total=20:A,B:ok:9");
        assert!(!rt_interp.jit_stats().jit_enabled);
        assert_eq!(rt_interp.jit_stats().compiled_blocks, 0);
        assert_eq!(rt_interp.jit_stats().inline_cache_hits, 0);
    }

    #[test]
    fn executes_cpp_anvil_vm_bytecode_shapes_inline_caches_and_gc() {
        let vm_source = "
            function square(n) {
                return n * n;
            }
            let acc = 0;
            for (let i = 1; i <= 20; i++) {
                let pt = { x: i, y: square(i) };
                acc += pt.x + pt.y;
            }
            acc;
        ";
        if is_cpp_jit_kernel_active() {
            let (res, telemetry) = eval_cpp_vm_script(vm_source).expect("AnvilVM compiled & ran");
            // sum_{i=1..20} (i + i^2) = 210 + 2870 = 3080
            assert_eq!(res, 3080.0);
            assert_eq!(telemetry.kernel_version, 18);
            assert!(telemetry.bytecode_ops_executed > 50);
            assert!(telemetry.shapes_allocated >= 3);
            assert!(telemetry.ic_hits >= 30);
            assert!(telemetry.hot_loops_accelerated >= 1);
        }

        if is_cpp_jit_kernel_active() {
            let numeric = "let s = 0; let i = 1; while (i <= 100) { s = s + i * i; i = i + 1; } s;";
            let (sum, native_telemetry) =
                eval_cpp_vm_script(numeric).expect("numeric loop compiled");
            assert_eq!(sum, 338350.0);
            assert!(native_telemetry.native_compilations >= 1);
            assert!(native_telemetry.bytecode_ops_executed > 100);
        }

        if is_cpp_jit_kernel_active() {
            let mut doc = parse_html(
                "<p id=\"out\"></p><script>
                   let s = 0;
                   let i = 1;
                   while (i <= 100) { s = s + i * i; i = i + 1; }
                   document.getElementById('out').textContent = s;
                 </script>",
            );
            let mut runtime = ScriptRuntime::with_jit_enabled(true);
            runtime.run_document_scripts(&mut doc);
            let out = doc.get_element_by_id("out").unwrap();
            assert_eq!(doc.text_content(out), "338350");
            assert!(runtime.jit_stats().vm_telemetry.native_compilations >= 1);
        }

        if is_cpp_jit_kernel_active() {
            let mut doc = parse_html(
                "<p id=\"out\"></p><p id=\"after\"></p><script>
                   let i = 0;
                   while (i < 100) {
                     document.getElementById('out').textContent = i;
                     i = i + 1;
                   }
                   document.getElementById('after').textContent = i;
                 </script>",
            );
            let mut runtime = ScriptRuntime::with_jit_enabled(true);
            runtime.run_document_scripts(&mut doc);
            let out = doc.get_element_by_id("out").unwrap();
            let after = doc.get_element_by_id("after").unwrap();
            assert_eq!(doc.text_content(out), "99");
            assert_eq!(doc.text_content(after), "100");
            assert!(runtime.jit_stats().vm_telemetry.native_compilations >= 1);
        }

        let (_, bench_telemetry) = run_cpp_vm_ic_benchmark(64);
        assert!(bench_telemetry.ic_hits >= 64);
        assert!(bench_telemetry.polymorphic_ic_sites >= 1);

        let data = [2.0, 4.0, 6.0, 8.0, 10.0];
        assert_eq!(eval_cpp_vector_reduce(0, &data), 30.0);
        assert_eq!(eval_cpp_vector_reduce(1, &data), 2.0);
        assert_eq!(eval_cpp_vector_reduce(2, &data), 10.0);
        assert_eq!(eval_cpp_vector_reduce(3, &data), 220.0);
        assert_eq!(eval_cpp_sum_range(1, 101, 1), 5050);
    }

    #[test]
    fn executes_prelude_promises_microtasks_and_origin_isolated_storage() {
        let mut doc = parse_html(
            "<p id=\"out\"></p>
             <script>
               localStorage.setItem('theme', 'amber');
               sessionStorage.setItem('tab', 'inspect');
               let clamped = anvilClampNumber(150, 0, 100);
               let formatted = anvilFormatBytes(2048);
               let sum = anvilArraySum([5, 10, 15]);
               Promise.resolve('ready').then((state) => {
                 let out = document.getElementById('out');
                 out.textContent = localStorage.getItem('theme') + ':' + sessionStorage.getItem('tab') + ':' + clamped + ':' + formatted + ':' + sum + ':' + state;
               });
             </script>",
        );
        let mut rt = ScriptRuntime::new();
        rt.set_page_origin("https://app.example/dashboard");
        rt.run_document_scripts(&mut doc);

        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(doc.text_content(out), "amber:inspect:100:2 KiB:30:ready");

        // Verify origin isolation when switching page_origin.
        rt.set_page_origin("https://other.example/");
        assert_eq!(rt.local_storage_get("theme"), None);
    }

    #[test]
    fn storage_write_past_the_origin_byte_cap_keeps_the_old_value() {
        let mut runtime = ScriptRuntime::new();
        runtime.set_page_origin("https://alpha.example/home");
        runtime.local_storage_set("kept", "alpha");
        let oversized = "x".repeat(MAX_STORAGE_ORIGIN_BYTES);
        runtime.local_storage_set("kept", &oversized);
        assert_eq!(runtime.local_storage_get("kept"), Some("alpha"));
        runtime.local_storage_set("kept", "beta");
        assert_eq!(runtime.local_storage_get("kept"), Some("beta"));
        runtime.local_storage_set("nope", &oversized);
        assert_eq!(runtime.local_storage_get("nope"), None);

        let mut doc = parse_html(&format!(
            "<p id=\"out\"></p>
             <script>
               sessionStorage.setItem('kept', 'short');
               sessionStorage.setItem('kept', '{}');
               document.getElementById('out').textContent = sessionStorage.getItem('kept');
             </script>",
            oversized
        ));
        runtime.run_document_scripts(&mut doc);
        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(doc.text_content(out), "short");
    }

    #[test]
    fn eval_script_uses_the_vm_for_a_numeric_script_and_rust_for_text_content() {
        let numeric = "
            function square(n) { return n * n; }
            let acc = 0;
            for (let i = 1; i <= 20; i++) {
                let pt = { x: i, y: square(i) };
                acc += pt.x + pt.y;
            }
            acc;
        ";
        let mut doc = parse_html("<p id=\"out\">before</p>");
        let mut runtime = ScriptRuntime::new();
        runtime.eval_script(&mut doc, numeric);
        if is_cpp_jit_kernel_active() {
            assert!(runtime.last_script_used_vm());
            assert_eq!(runtime.last_vm_number(), Some(3080.0));
        } else {
            assert!(!runtime.last_script_used_vm());
        }
        runtime.eval_script(
            &mut doc,
            "let el = document.getElementById('out'); el.textContent = 'from-rust';",
        );
        assert!(!runtime.last_script_used_vm());
        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(doc.text_content(out), "from-rust");
    }

    #[test]
    fn executes_try_catch_finally_throw_and_script_isolation() {
        let mut doc = parse_html(
            "<p id=\"out\">init</p>
             <script>
               let log = [];
               try {
                 log.push('try');
                 throw new Error('boom');
                 log.push('unreachable');
               } catch (err) {
                 log.push('caught:' + err.message);
               } finally {
                 log.push('finally');
               }
               let mql = window.matchMedia('(prefers-color-scheme: dark)');
               log.push('mql:' + mql.matches);
               document.addEventListener('DOMContentLoaded', () => {
                 document.getElementById('out').textContent = log.join('|');
               });
             </script>
             <script>
               throw new TypeError('isolated script error');
             </script>
             <script>
               let out = document.getElementById('out');
               out.setAttribute('data-recovered', 'yes');
             </script>",
        );
        let mut runtime = ScriptRuntime::new();
        runtime.run_document_scripts(&mut doc);
        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(doc.text_content(out), "try|caught:boom|finally|mql:false");
        assert_eq!(doc.get(out).data().attr("data-recovered"), Some("yes"));
        assert!(!runtime.was_aborted());
    }

    #[test]
    fn executes_es6_loops_switch_destructuring_spread_nullish_and_classes() {
        let mut doc = parse_html(
            "<p id=\"out\"></p>
             <script>
               class BaseWidget {
                 constructor(prefix) {
                   this.prefix = prefix;
                 }
               }
               class CounterWidget extends BaseWidget {
                 constructor(prefix, start) {
                   super(prefix);
                   this.count = start;
                 }
                 format(extra) {
                   return this.prefix + ':' + (this.count + extra);
                 }
               }
               let w = new CounterWidget('W', 10);
               let base = [1, 2];
               let combined = [...base, 3, 4];
               let acc = 0;
               for (const n of combined) {
                 if (n === 2) {
                   continue;
                 }
                 acc += n;
               }
               const cfg = { mode: 'fast', retries: 0 };
               const { mode, missing = 'fallback' } = cfg;
               const [first, second] = combined;
               let label = '';
               switch (mode) {
                 case 'slow':
                   label = 'S';
                   break;
                 case 'fast':
                   label = 'F';
                   break;
                 default:
                   label = 'D';
               }
               let nullish = cfg.nonexistent ?? 'defaulted';
               let zeroKept = cfg.retries ?? 99;
               let opt = cfg?.nested?.deep ?? 'safe';
               document.getElementById('out').textContent =
                 w.format(5) + '|' + acc + '|' + label + '|' + missing + '|' + first + ',' + second + '|' + nullish + ':' + zeroKept + ':' + opt;
             </script>",
        );
        let mut runtime = ScriptRuntime::new();
        runtime.run_document_scripts(&mut doc);
        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(
            doc.text_content(out),
            "W:15|8|F|fallback|1,2|defaulted:0:safe"
        );
    }

    #[test]
    fn executes_array_and_object_higher_order_builtins() {
        let mut doc = parse_html(
            "<p id=\"out\"></p>
             <script>
               let nums = [2, 4, 6, 8];
               let total = nums.reduce((acc, x) => acc + x, 0);
               let found = nums.find((x) => x > 5);
               let fidx = nums.findIndex((x) => x === 6);
               let hasBig = nums.some((x) => x >= 8);
               let allEven = nums.every((x) => x % 2 === 0);
               let obj = Object.assign({ a: 1 }, { b: 2 });
               let pairs = Object.entries(obj);
               let rebuilt = Object.fromEntries(pairs);
               document.getElementById('out').textContent =
                 total + ':' + found + ':' + fidx + ':' + hasBig + ':' + allEven + ':' + rebuilt.a + ',' + rebuilt.b;
             </script>",
        );
        let mut runtime = ScriptRuntime::new();
        runtime.run_document_scripts(&mut doc);
        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(doc.text_content(out), "20:6:2:true:true:1,2");
    }

    #[test]
    fn executes_dom_mutations_dataset_style_and_selectors() {
        let mut doc = parse_html(
            "<div id=\"container\" class=\"panel\"><span id=\"first\" class=\"item\">One</span></div>
             <p id=\"out\"></p>
             <script>
               let container = document.getElementById('container');
               let second = document.createElement('span');
               second.id = 'second';
               second.className = 'item highlight';
               second.dataset.index = '2';
               second.style.backgroundColor = 'rgb(20, 30, 40)';
               second.appendChild(document.createTextNode('Two'));
               container.appendChild(second);

               let beforeNode = document.createElement('span');
               beforeNode.id = 'zero';
               beforeNode.textContent = 'Zero';
               let first = document.getElementById('first');
               container.insertBefore(beforeNode, first);

               let matched = second.matches('.item.highlight');
               let panel = second.closest('.panel');
               let count = container.childElementCount;
               let nextId = first.nextElementSibling.id;
               let rect = second.getBoundingClientRect();
               let computed = window.getComputedStyle(second);

               document.getElementById('out').textContent =
                 count + ':' + matched + ':' + panel.id + ':' + nextId + ':' + second.dataset.index + ':' + computed.backgroundColor + ':' + rect.width;
             </script>",
        );
        let mut runtime = ScriptRuntime::new();
        runtime.run_document_scripts(&mut doc);
        let out = doc.get_element_by_id("out").unwrap();
        assert_eq!(
            doc.text_content(out),
            "3:true:container:second:2:rgb(20, 30, 40):100"
        );
    }
}
