// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! WebAssembly 1.0 (`\0asm` version `1`) binary decoder, validator, linear memory,
//! and stack virtual machine for `anvil-script`.
//!
//! Supported sections:
//! - `Type` (`0x01`), `Import` (`0x02`), `Function` (`0x03`), `Table` (`0x04`),
//!   `Memory` (`0x05`), `Global` (`0x06`), `Export` (`0x07`), `Start` (`0x08`),
//!   `Element` (`0x09`), `Code` (`0x0A`), and `Data` (`0x0B`), plus custom sections (`0x00`).
//!
//! Resource caps:
//! - [`WASM_PAGE_SIZE`] is `65,536` bytes (64 KiB).
//! - [`MAX_WASM_PAGES`] is `256` pages (16 MiB linear memory per instance).
//! - [`MAX_WASM_STEPS`] is `500,000` instructions per invocation.
//! - [`MAX_WASM_CALL_DEPTH`] is `64` nested frames.
//! - [`MAX_WASM_STACK`] is `4,096` operand values.

use std::collections::HashMap;
use std::fmt;

/// Standard WebAssembly linear memory page size (64 KiB).
pub const WASM_PAGE_SIZE: usize = 65_536;

/// Maximum linear memory pages allowed per instance (`256` pages = 16 MiB).
pub const MAX_WASM_PAGES: u32 = 256;

/// Maximum instructions executed in one exported function call.
pub const MAX_WASM_STEPS: usize = 500_000;

/// Maximum call frame recursion depth in one invocation.
pub const MAX_WASM_CALL_DEPTH: usize = 64;

/// Maximum operand stack depth during execution.
pub const MAX_WASM_STACK: usize = 4_096;

/// WebAssembly 1.0 magic header (`\0asm`).
pub const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6D];

/// WebAssembly 1.0 binary format version (`1`).
pub const WASM_VERSION: [u8; 4] = [0x01, 0x00, 0x00, 0x00];

/// WebAssembly value type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmValType {
    I32,
    I64,
    F32,
    F64,
}

impl WasmValType {
    /// Decode a single value-type byte (`0x7F`..=`0x7C`).
    pub fn from_byte(byte: u8) -> Result<Self, WasmError> {
        match byte {
            0x7F => Ok(Self::I32),
            0x7E => Ok(Self::I64),
            0x7D => Ok(Self::F32),
            0x7C => Ok(Self::F64),
            _ => Err(WasmError::InvalidModule("unsupported value type")),
        }
    }

    /// Default zero value for this type.
    pub fn zero_value(self) -> WasmValue {
        match self {
            Self::I32 => WasmValue::I32(0),
            Self::I64 => WasmValue::I64(0),
            Self::F32 => WasmValue::F32(0.0),
            Self::F64 => WasmValue::F64(0.0),
        }
    }
}

/// Runtime value on the WebAssembly operand stack, locals, or globals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WasmValue {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl WasmValue {
    /// Convert a JavaScript `f64` number into the target `WasmValType`.
    pub fn from_f64_as(ty: WasmValType, n: f64) -> Self {
        if !n.is_finite() {
            return ty.zero_value();
        }
        match ty {
            WasmValType::I32 => Self::I32(n as i64 as i32),
            WasmValType::I64 => Self::I64(n as i64),
            WasmValType::F32 => Self::F32(n as f32),
            WasmValType::F64 => Self::F64(n),
        }
    }

    /// Convert this value to `f64` for returning to JavaScript.
    pub fn as_f64(self) -> f64 {
        match self {
            Self::I32(v) => f64::from(v),
            Self::I64(v) => v as f64,
            Self::F32(v) => f64::from(v),
            Self::F64(v) => v,
        }
    }

    /// Extract as `i32`, coercing other numeric types if needed.
    pub fn as_i32(self) -> i32 {
        match self {
            Self::I32(v) => v,
            Self::I64(v) => v as i32,
            Self::F32(v) => v as i32,
            Self::F64(v) => v as i32,
        }
    }

    /// Extract as `i64`, coercing other numeric types if needed.
    pub fn as_i64(self) -> i64 {
        match self {
            Self::I32(v) => i64::from(v),
            Self::I64(v) => v,
            Self::F32(v) => v as i64,
            Self::F64(v) => v as i64,
        }
    }

    /// Extract as `f32`.
    pub fn as_f32(self) -> f32 {
        match self {
            Self::I32(v) => v as f32,
            Self::I64(v) => v as f32,
            Self::F32(v) => v,
            Self::F64(v) => v as f32,
        }
    }
}

/// Failure during module decoding, instantiation, or trap execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WasmError {
    /// Binary module header, section, or instruction layout is malformed.
    InvalidModule(&'static str),
    /// Referenced function, memory, table, or global does not exist.
    UnknownExport(String),
    /// Runtime trap (`unreachable`, division by zero, out-of-bounds memory, stack overflow).
    Trap(&'static str),
    /// Execution step or recursion budget exceeded.
    BudgetExceeded,
}

impl fmt::Display for WasmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModule(msg) => write!(f, "CompileError: {msg}"),
            Self::UnknownExport(name) => write!(f, "LinkError: unknown export {name}"),
            Self::Trap(msg) => write!(f, "RuntimeError: {msg}"),
            Self::BudgetExceeded => write!(f, "RuntimeError: execution budget exceeded"),
        }
    }
}

/// Function signature `(params) -> (results)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmFuncType {
    pub params: Vec<WasmValType>,
    pub results: Vec<WasmValType>,
}

/// Export kind in the Export section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmExportKind {
    Func,
    Table,
    Memory,
    Global,
}

/// One exported item from a module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmExport {
    pub name: String,
    pub kind: WasmExportKind,
    pub index: u32,
}

/// Imported function descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmImportFunc {
    pub module: String,
    pub name: String,
    pub type_idx: u32,
}

/// Decoded function body from the Code section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmCodeBody {
    pub locals: Vec<WasmValType>,
    pub code: Vec<u8>,
}

/// Global variable definition in a module.
#[derive(Clone, Debug, PartialEq)]
pub struct WasmGlobalDef {
    pub val_type: WasmValType,
    pub mutable: bool,
    pub init: WasmValue,
}

/// Active data segment copied into linear memory at instantiation time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmDataSegment {
    pub offset: usize,
    pub bytes: Vec<u8>,
}

/// Active element segment copied into the function table at instantiation time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmElementSegment {
    pub offset: usize,
    pub func_indices: Vec<u32>,
}

/// Validated, decoded WebAssembly 1.0 module.
#[derive(Clone, Debug, PartialEq)]
pub struct WasmModule {
    pub types: Vec<WasmFuncType>,
    pub imported_funcs: Vec<WasmImportFunc>,
    pub func_type_indices: Vec<u32>,
    pub table_initial: u32,
    pub table_max: Option<u32>,
    pub memory_initial: u32,
    pub memory_max: Option<u32>,
    pub globals: Vec<WasmGlobalDef>,
    pub exports: Vec<WasmExport>,
    pub start_func: Option<u32>,
    pub elements: Vec<WasmElementSegment>,
    pub codes: Vec<WasmCodeBody>,
    pub data_segments: Vec<WasmDataSegment>,
}

/// Bounded linear memory (`WebAssembly.Memory`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmMemory {
    pub bytes: Vec<u8>,
    pub max_pages: u32,
}

impl WasmMemory {
    /// Allocate `initial_pages` 64 KiB pages (clamped to [`MAX_WASM_PAGES`]).
    pub fn new(initial_pages: u32, max_pages: Option<u32>) -> Result<Self, WasmError> {
        let max_p = max_pages.unwrap_or(MAX_WASM_PAGES).min(MAX_WASM_PAGES);
        if initial_pages > MAX_WASM_PAGES || initial_pages > max_p {
            return Err(WasmError::InvalidModule(
                "memory initial pages exceed limit",
            ));
        }
        let len = (initial_pages as usize).saturating_mul(WASM_PAGE_SIZE);
        Ok(Self {
            bytes: vec![0u8; len],
            max_pages: max_p,
        })
    }

    /// Number of 64 KiB pages currently allocated.
    pub fn pages(&self) -> u32 {
        (self.bytes.len() / WASM_PAGE_SIZE) as u32
    }

    /// Grow linear memory by `delta_pages`, returning the previous page count or `-1` on failure.
    pub fn grow(&mut self, delta_pages: u32) -> i32 {
        let old_pages = self.pages();
        let Some(new_pages) = old_pages.checked_add(delta_pages) else {
            return -1;
        };
        if new_pages > self.max_pages || new_pages > MAX_WASM_PAGES {
            return -1;
        }
        let new_len = (new_pages as usize).saturating_mul(WASM_PAGE_SIZE);
        self.bytes.resize(new_len, 0);
        old_pages as i32
    }

    /// Read `N` bytes at `addr`, trapping on out-of-bounds access.
    pub fn read_bytes<const N: usize>(&self, addr: usize) -> Result<[u8; N], WasmError> {
        let end = addr
            .checked_add(N)
            .ok_or(WasmError::Trap("out of bounds memory access"))?;
        let slice = self
            .bytes
            .get(addr..end)
            .ok_or(WasmError::Trap("out of bounds memory access"))?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Ok(out)
    }

    /// Write `src` bytes at `addr`, trapping on out-of-bounds access.
    pub fn write_bytes(&mut self, addr: usize, src: &[u8]) -> Result<(), WasmError> {
        let end = addr
            .checked_add(src.len())
            .ok_or(WasmError::Trap("out of bounds memory access"))?;
        let dst = self
            .bytes
            .get_mut(addr..end)
            .ok_or(WasmError::Trap("out of bounds memory access"))?;
        dst.copy_from_slice(src);
        Ok(())
    }
}

/// Live instantiated WebAssembly module (`WebAssembly.Instance`).
#[derive(Clone, Debug, PartialEq)]
pub struct WasmInstance {
    pub module: WasmModule,
    pub memory: WasmMemory,
    pub globals: Vec<WasmValue>,
    pub table: Vec<Option<u32>>,
    /// Imported numeric constants or host return values keyed by `"module.name"`.
    pub host_constants: HashMap<String, f64>,
}

/// Check whether `bytes` is a valid WebAssembly 1.0 module.
pub fn validate_wasm(bytes: &[u8]) -> bool {
    WasmModule::decode(bytes).is_ok()
}

impl WasmModule {
    /// Decode and validate a WebAssembly 1.0 binary module.
    pub fn decode(bytes: &[u8]) -> Result<Self, WasmError> {
        if bytes.len() < 8 {
            return Err(WasmError::InvalidModule("module shorter than 8 bytes"));
        }
        if bytes[0..4] != WASM_MAGIC {
            return Err(WasmError::InvalidModule("invalid wasm magic header"));
        }
        if bytes[4..8] != WASM_VERSION {
            return Err(WasmError::InvalidModule("unsupported wasm version"));
        }

        let mut pos = 8usize;
        let mut last_section_id = 0u8;
        let mut module = Self {
            types: Vec::new(),
            imported_funcs: Vec::new(),
            func_type_indices: Vec::new(),
            table_initial: 0,
            table_max: None,
            memory_initial: 1,
            memory_max: None,
            globals: Vec::new(),
            exports: Vec::new(),
            start_func: None,
            elements: Vec::new(),
            codes: Vec::new(),
            data_segments: Vec::new(),
        };

        while pos < bytes.len() {
            let section_id = read_u8(bytes, &mut pos)?;
            let section_len = read_u32_leb128(bytes, &mut pos)? as usize;
            let section_end = pos
                .checked_add(section_len)
                .ok_or(WasmError::InvalidModule("section length overflow"))?;
            if section_end > bytes.len() {
                return Err(WasmError::InvalidModule("section truncated"));
            }
            if section_id != 0 {
                if section_id <= last_section_id {
                    return Err(WasmError::InvalidModule("sections out of order"));
                }
                last_section_id = section_id;
            }

            let mut sec_pos = pos;
            match section_id {
                0 => {
                    // Custom section: skip payload.
                }
                1 => {
                    // Type section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    if count > 4096 {
                        return Err(WasmError::InvalidModule("too many types"));
                    }
                    for _ in 0..count {
                        let form = read_u8(bytes, &mut sec_pos)?;
                        if form != 0x60 {
                            return Err(WasmError::InvalidModule("expected functype 0x60"));
                        }
                        let param_count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                        if param_count > 128 {
                            return Err(WasmError::InvalidModule("too many parameters"));
                        }
                        let mut params = Vec::with_capacity(param_count);
                        for _ in 0..param_count {
                            params.push(WasmValType::from_byte(read_u8(bytes, &mut sec_pos)?)?);
                        }
                        let result_count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                        if result_count > 16 {
                            return Err(WasmError::InvalidModule("too many results"));
                        }
                        let mut results = Vec::with_capacity(result_count);
                        for _ in 0..result_count {
                            results.push(WasmValType::from_byte(read_u8(bytes, &mut sec_pos)?)?);
                        }
                        module.types.push(WasmFuncType { params, results });
                    }
                }
                2 => {
                    // Import section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    if count > 1024 {
                        return Err(WasmError::InvalidModule("too many imports"));
                    }
                    for _ in 0..count {
                        let mod_name = read_name(bytes, &mut sec_pos)?;
                        let field_name = read_name(bytes, &mut sec_pos)?;
                        let kind = read_u8(bytes, &mut sec_pos)?;
                        match kind {
                            0x00 => {
                                let type_idx = read_u32_leb128(bytes, &mut sec_pos)?;
                                module.imported_funcs.push(WasmImportFunc {
                                    module: mod_name,
                                    name: field_name,
                                    type_idx,
                                });
                            }
                            0x01 => {
                                // Table import
                                let _elem_type = read_u8(bytes, &mut sec_pos)?;
                                let (init, max) = read_limits(bytes, &mut sec_pos)?;
                                module.table_initial = init;
                                module.table_max = max;
                            }
                            0x02 => {
                                // Memory import
                                let (init, max) = read_limits(bytes, &mut sec_pos)?;
                                module.memory_initial = init;
                                module.memory_max = max;
                            }
                            0x03 => {
                                // Global import
                                let val_type =
                                    WasmValType::from_byte(read_u8(bytes, &mut sec_pos)?)?;
                                let mutable = read_u8(bytes, &mut sec_pos)? != 0;
                                module.globals.push(WasmGlobalDef {
                                    val_type,
                                    mutable,
                                    init: val_type.zero_value(),
                                });
                            }
                            _ => return Err(WasmError::InvalidModule("invalid import kind")),
                        }
                    }
                }
                3 => {
                    // Function section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    if count > 8192 {
                        return Err(WasmError::InvalidModule("too many functions"));
                    }
                    for _ in 0..count {
                        let idx = read_u32_leb128(bytes, &mut sec_pos)?;
                        if (idx as usize) >= module.types.len() {
                            return Err(WasmError::InvalidModule(
                                "function type index out of bounds",
                            ));
                        }
                        module.func_type_indices.push(idx);
                    }
                }
                4 => {
                    // Table section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    for _ in 0..count {
                        let elem_type = read_u8(bytes, &mut sec_pos)?;
                        if elem_type != 0x70 {
                            return Err(WasmError::InvalidModule("unsupported table element type"));
                        }
                        let (init, max) = read_limits(bytes, &mut sec_pos)?;
                        module.table_initial = init.min(4096);
                        module.table_max = max.map(|m| m.min(4096));
                    }
                }
                5 => {
                    // Memory section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    if count > 1 {
                        return Err(WasmError::InvalidModule("multiple memories not supported"));
                    }
                    for _ in 0..count {
                        let (init, max) = read_limits(bytes, &mut sec_pos)?;
                        if init > MAX_WASM_PAGES {
                            return Err(WasmError::InvalidModule("memory limit exceeds cap"));
                        }
                        module.memory_initial = init;
                        module.memory_max = max;
                    }
                }
                6 => {
                    // Global section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    if count > 4096 {
                        return Err(WasmError::InvalidModule("too many globals"));
                    }
                    for _ in 0..count {
                        let val_type = WasmValType::from_byte(read_u8(bytes, &mut sec_pos)?)?;
                        let mut_byte = read_u8(bytes, &mut sec_pos)?;
                        if mut_byte > 1 {
                            return Err(WasmError::InvalidModule("invalid mutability byte"));
                        }
                        let init = eval_const_expr(bytes, &mut sec_pos, &module.globals)?;
                        module.globals.push(WasmGlobalDef {
                            val_type,
                            mutable: mut_byte == 1,
                            init,
                        });
                    }
                }
                7 => {
                    // Export section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    if count > 4096 {
                        return Err(WasmError::InvalidModule("too many exports"));
                    }
                    for _ in 0..count {
                        let name = read_name(bytes, &mut sec_pos)?;
                        let kind = match read_u8(bytes, &mut sec_pos)? {
                            0x00 => WasmExportKind::Func,
                            0x01 => WasmExportKind::Table,
                            0x02 => WasmExportKind::Memory,
                            0x03 => WasmExportKind::Global,
                            _ => return Err(WasmError::InvalidModule("invalid export kind")),
                        };
                        let index = read_u32_leb128(bytes, &mut sec_pos)?;
                        module.exports.push(WasmExport { name, kind, index });
                    }
                }
                8 => {
                    // Start section
                    let idx = read_u32_leb128(bytes, &mut sec_pos)?;
                    module.start_func = Some(idx);
                }
                9 => {
                    // Element section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    for _ in 0..count {
                        let _table_idx = read_u32_leb128(bytes, &mut sec_pos)?;
                        let offset_val = eval_const_expr(bytes, &mut sec_pos, &module.globals)?;
                        let offset = offset_val.as_i32().max(0) as usize;
                        let num_elem = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                        let mut func_indices = Vec::with_capacity(num_elem.min(4096));
                        for _ in 0..num_elem {
                            func_indices.push(read_u32_leb128(bytes, &mut sec_pos)?);
                        }
                        module.elements.push(WasmElementSegment {
                            offset,
                            func_indices,
                        });
                    }
                }
                10 => {
                    // Code section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    if count != module.func_type_indices.len() {
                        return Err(WasmError::InvalidModule(
                            "code count does not match function section",
                        ));
                    }
                    for _ in 0..count {
                        let body_size = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                        let body_end = sec_pos
                            .checked_add(body_size)
                            .ok_or(WasmError::InvalidModule("code body size overflow"))?;
                        if body_end > section_end {
                            return Err(WasmError::InvalidModule("code body truncated"));
                        }
                        let local_decl_count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                        let mut locals = Vec::new();
                        for _ in 0..local_decl_count {
                            let repeat = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                            let ty = WasmValType::from_byte(read_u8(bytes, &mut sec_pos)?)?;
                            if locals.len().saturating_add(repeat) > 2048 {
                                return Err(WasmError::InvalidModule("too many local variables"));
                            }
                            locals.extend(std::iter::repeat_n(ty, repeat));
                        }
                        if sec_pos > body_end
                            || bytes.get(body_end.saturating_sub(1)) != Some(&0x0B)
                        {
                            return Err(WasmError::InvalidModule("code body missing end 0x0B"));
                        }
                        let code = bytes[sec_pos..body_end].to_vec();
                        module.codes.push(WasmCodeBody { locals, code });
                        sec_pos = body_end;
                    }
                }
                11 => {
                    // Data section
                    let count = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                    for _ in 0..count {
                        let mode = read_u32_leb128(bytes, &mut sec_pos)?;
                        let offset = if mode == 0 {
                            eval_const_expr(bytes, &mut sec_pos, &module.globals)?
                                .as_i32()
                                .max(0) as usize
                        } else {
                            0
                        };
                        let data_len = read_u32_leb128(bytes, &mut sec_pos)? as usize;
                        let data_end = sec_pos
                            .checked_add(data_len)
                            .ok_or(WasmError::InvalidModule("data segment overflow"))?;
                        if data_end > section_end {
                            return Err(WasmError::InvalidModule("data segment truncated"));
                        }
                        module.data_segments.push(WasmDataSegment {
                            offset,
                            bytes: bytes[sec_pos..data_end].to_vec(),
                        });
                        sec_pos = data_end;
                    }
                }
                _ => return Err(WasmError::InvalidModule("unknown section id")),
            }
            pos = section_end;
        }

        if module.codes.len() != module.func_type_indices.len() {
            return Err(WasmError::InvalidModule(
                "function and code section counts mismatch",
            ));
        }

        Ok(module)
    }

    /// Names of all exported items.
    pub fn export_names(&self) -> Vec<String> {
        self.exports.iter().map(|e| e.name.clone()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Func,
    Block,
    Loop,
    If,
}

#[derive(Clone, Copy, Debug)]
struct ControlFrame {
    kind: BlockKind,
    stack_height: usize,
    arity: usize,
    start_pc: usize,
    end_pc: usize,
}

impl WasmInstance {
    /// Instantiate `module`, initializing linear memory, globals, element tables, and running `start` if present.
    pub fn instantiate(
        module: WasmModule,
        host_constants: HashMap<String, f64>,
    ) -> Result<Self, WasmError> {
        let mut memory = WasmMemory::new(module.memory_initial, module.memory_max)?;
        for seg in &module.data_segments {
            memory.write_bytes(seg.offset, &seg.bytes)?;
        }
        let mut globals: Vec<WasmValue> = module.globals.iter().map(|g| g.init).collect();
        for (idx, imp) in module.imported_funcs.iter().enumerate() {
            let key = format!("{}.{}", imp.module, imp.name);
            if let Some(&val) = host_constants.get(&key)
                && idx < globals.len()
            {
                globals[idx] = WasmValue::F64(val);
            }
        }
        let mut table = vec![None; module.table_initial as usize];
        for elem in &module.elements {
            for (i, &func_idx) in elem.func_indices.iter().enumerate() {
                let slot = elem.offset.saturating_add(i);
                if slot >= table.len() && slot < 4096 {
                    table.resize(slot + 1, None);
                }
                if let Some(entry) = table.get_mut(slot) {
                    *entry = Some(func_idx);
                }
            }
        }

        let mut instance = Self {
            module,
            memory,
            globals,
            table,
            host_constants,
        };
        if let Some(start_idx) = instance.module.start_func {
            let mut steps = MAX_WASM_STEPS;
            let _ = instance.call_func_index(start_idx, &[], &mut steps, 0)?;
        }
        Ok(instance)
    }

    /// Invoke an exported function by `name` with numeric arguments, returning its result values.
    pub fn call_export(&mut self, name: &str, args: &[f64]) -> Result<Vec<WasmValue>, WasmError> {
        let export = self
            .module
            .exports
            .iter()
            .find(|e| e.name == name && e.kind == WasmExportKind::Func)
            .cloned()
            .ok_or_else(|| WasmError::UnknownExport(name.to_string()))?;

        let sig = self.func_signature(export.index)?;
        let mut typed_args = Vec::with_capacity(sig.params.len());
        for (idx, &param_ty) in sig.params.iter().enumerate() {
            let raw = args.get(idx).copied().unwrap_or(0.0);
            typed_args.push(WasmValue::from_f64_as(param_ty, raw));
        }

        let mut steps = MAX_WASM_STEPS;
        self.call_func_index(export.index, &typed_args, &mut steps, 0)
    }

    /// Read an exported global value by `name`.
    pub fn get_exported_global(&self, name: &str) -> Option<WasmValue> {
        let exp = self
            .module
            .exports
            .iter()
            .find(|e| e.name == name && e.kind == WasmExportKind::Global)?;
        self.globals.get(exp.index as usize).copied()
    }

    /// Write an exported mutable global value by `name`.
    pub fn set_exported_global(&mut self, name: &str, value: f64) -> bool {
        let Some(exp) = self
            .module
            .exports
            .iter()
            .find(|e| e.name == name && e.kind == WasmExportKind::Global)
            .cloned()
        else {
            return false;
        };
        let idx = exp.index as usize;
        let Some(def) = self.module.globals.get(idx) else {
            return false;
        };
        if !def.mutable {
            return false;
        }
        let ty = def.val_type;
        if let Some(slot) = self.globals.get_mut(idx) {
            *slot = WasmValue::from_f64_as(ty, value);
            return true;
        }
        false
    }

    fn func_signature(&self, func_idx: u32) -> Result<WasmFuncType, WasmError> {
        let imported_len = self.module.imported_funcs.len();
        let idx = func_idx as usize;
        let type_idx = if idx < imported_len {
            self.module.imported_funcs[idx].type_idx as usize
        } else {
            let local_idx = idx.saturating_sub(imported_len);
            *self
                .module
                .func_type_indices
                .get(local_idx)
                .ok_or(WasmError::InvalidModule("function index out of bounds"))?
                as usize
        };
        self.module
            .types
            .get(type_idx)
            .cloned()
            .ok_or(WasmError::InvalidModule("type index out of bounds"))
    }

    fn call_func_index(
        &mut self,
        func_idx: u32,
        args: &[WasmValue],
        steps: &mut usize,
        depth: usize,
    ) -> Result<Vec<WasmValue>, WasmError> {
        if depth >= MAX_WASM_CALL_DEPTH {
            return Err(WasmError::Trap("call stack exhausted"));
        }
        let imported_len = self.module.imported_funcs.len();
        let idx = func_idx as usize;
        if idx < imported_len {
            let imp = &self.module.imported_funcs[idx];
            let sig = self.func_signature(func_idx)?;
            let key = format!("{}.{}", imp.module, imp.name);
            let base_val = self
                .host_constants
                .get(&key)
                .or_else(|| self.host_constants.get(&imp.name))
                .copied()
                .unwrap_or_else(|| args.first().map(|v| v.as_f64()).unwrap_or(0.0));
            return Ok(sig
                .results
                .iter()
                .map(|&ty| WasmValue::from_f64_as(ty, base_val))
                .collect());
        }

        let local_func_idx = idx - imported_len;
        let sig = self.func_signature(func_idx)?;
        let body = self
            .module
            .codes
            .get(local_func_idx)
            .cloned()
            .ok_or(WasmError::InvalidModule("missing function code"))?;

        let mut locals = Vec::with_capacity(sig.params.len() + body.locals.len());
        for (i, &ty) in sig.params.iter().enumerate() {
            locals.push(args.get(i).copied().unwrap_or_else(|| ty.zero_value()));
        }
        for &ty in &body.locals {
            locals.push(ty.zero_value());
        }

        let code = &body.code;
        let mut stack: Vec<WasmValue> = Vec::with_capacity(32);
        let mut control: Vec<ControlFrame> = vec![ControlFrame {
            kind: BlockKind::Func,
            stack_height: 0,
            arity: sig.results.len(),
            start_pc: 0,
            end_pc: code.len().saturating_sub(1),
        }];

        let mut pc = 0usize;
        while pc < code.len() {
            if *steps == 0 {
                return Err(WasmError::BudgetExceeded);
            }
            *steps -= 1;
            let op = read_u8(code, &mut pc)?;

            match op {
                0x00 => return Err(WasmError::Trap("unreachable executed")),
                0x01 => {} // nop
                0x02..=0x04 => {
                    let block_ty = read_u8(code, &mut pc)?;
                    let arity = usize::from(block_ty != 0x40);
                    let (end_pc, else_pc) = scan_block_bounds(code, pc)?;
                    let kind = match op {
                        0x02 => BlockKind::Block,
                        0x03 => BlockKind::Loop,
                        _ => BlockKind::If,
                    };
                    if kind == BlockKind::If {
                        let cond = pop_val(&mut stack)?.as_i32();
                        control.push(ControlFrame {
                            kind,
                            stack_height: stack.len(),
                            arity,
                            start_pc: pc,
                            end_pc,
                        });
                        if cond == 0 {
                            if let Some(ep) = else_pc {
                                pc = ep + 1;
                            } else {
                                control.pop();
                                pc = end_pc + 1;
                            }
                        }
                    } else {
                        control.push(ControlFrame {
                            kind,
                            stack_height: stack.len(),
                            arity,
                            start_pc: pc,
                            end_pc,
                        });
                    }
                }
                0x05 => {
                    // else: reached at the end of a taken `if` branch, jump past `end`
                    let frame = control
                        .pop()
                        .ok_or(WasmError::InvalidModule("unbalanced else"))?;
                    unwind_stack(&mut stack, frame.stack_height, frame.arity)?;
                    pc = frame.end_pc + 1;
                }
                0x0B => {
                    // end
                    let Some(frame) = control.pop() else {
                        break;
                    };
                    unwind_stack(&mut stack, frame.stack_height, frame.arity)?;
                    if control.is_empty() {
                        break;
                    }
                }
                0x0C => {
                    // br
                    let label_depth = read_u32_leb128(code, &mut pc)? as usize;
                    branch_to_label(label_depth, &mut control, &mut stack, &mut pc)?;
                }
                0x0D => {
                    // br_if
                    let label_depth = read_u32_leb128(code, &mut pc)? as usize;
                    let cond = pop_val(&mut stack)?.as_i32();
                    if cond != 0 {
                        branch_to_label(label_depth, &mut control, &mut stack, &mut pc)?;
                    }
                }
                0x0E => {
                    // br_table
                    let count = read_u32_leb128(code, &mut pc)? as usize;
                    let mut targets = Vec::with_capacity(count.min(1024));
                    for _ in 0..count {
                        targets.push(read_u32_leb128(code, &mut pc)? as usize);
                    }
                    let default_target = read_u32_leb128(code, &mut pc)? as usize;
                    let idx = pop_val(&mut stack)?.as_i32();
                    let chosen = if idx >= 0 && (idx as usize) < targets.len() {
                        targets[idx as usize]
                    } else {
                        default_target
                    };
                    branch_to_label(chosen, &mut control, &mut stack, &mut pc)?;
                }
                0x0F => {
                    // return
                    let arity = sig.results.len();
                    unwind_stack(&mut stack, 0, arity)?;
                    break;
                }
                0x10 => {
                    // call
                    let callee_idx = read_u32_leb128(code, &mut pc)?;
                    let callee_sig = self.func_signature(callee_idx)?;
                    let mut call_args = vec![WasmValue::I32(0); callee_sig.params.len()];
                    for slot in call_args.iter_mut().rev() {
                        *slot = pop_val(&mut stack)?;
                    }
                    let results = self.call_func_index(callee_idx, &call_args, steps, depth + 1)?;
                    for r in results {
                        push_val(&mut stack, r)?;
                    }
                }
                0x11 => {
                    // call_indirect
                    let expected_type_idx = read_u32_leb128(code, &mut pc)?;
                    let _table_idx = read_u8(code, &mut pc)?;
                    let elem_idx = pop_val(&mut stack)?.as_i32();
                    if elem_idx < 0 {
                        return Err(WasmError::Trap("undefined element in call_indirect"));
                    }
                    let target_func = self
                        .table
                        .get(elem_idx as usize)
                        .and_then(|opt| *opt)
                        .ok_or(WasmError::Trap("uninitialized table element"))?;
                    let expected_sig = self
                        .module
                        .types
                        .get(expected_type_idx as usize)
                        .cloned()
                        .ok_or(WasmError::InvalidModule("invalid type index"))?;
                    let actual_sig = self.func_signature(target_func)?;
                    if expected_sig != actual_sig {
                        return Err(WasmError::Trap("indirect call type mismatch"));
                    }
                    let mut call_args = vec![WasmValue::I32(0); actual_sig.params.len()];
                    for slot in call_args.iter_mut().rev() {
                        *slot = pop_val(&mut stack)?;
                    }
                    let results =
                        self.call_func_index(target_func, &call_args, steps, depth + 1)?;
                    for r in results {
                        push_val(&mut stack, r)?;
                    }
                }
                0x1A => {
                    // drop
                    let _ = pop_val(&mut stack)?;
                }
                0x1B => {
                    // select
                    let c = pop_val(&mut stack)?.as_i32();
                    let val2 = pop_val(&mut stack)?;
                    let val1 = pop_val(&mut stack)?;
                    push_val(&mut stack, if c != 0 { val1 } else { val2 })?;
                }
                0x20 => {
                    // local.get
                    let idx = read_u32_leb128(code, &mut pc)? as usize;
                    let val = *locals
                        .get(idx)
                        .ok_or(WasmError::InvalidModule("local index out of bounds"))?;
                    push_val(&mut stack, val)?;
                }
                0x21 => {
                    // local.set
                    let idx = read_u32_leb128(code, &mut pc)? as usize;
                    let val = pop_val(&mut stack)?;
                    let slot = locals
                        .get_mut(idx)
                        .ok_or(WasmError::InvalidModule("local index out of bounds"))?;
                    *slot = val;
                }
                0x22 => {
                    // local.tee
                    let idx = read_u32_leb128(code, &mut pc)? as usize;
                    let val = *stack
                        .last()
                        .ok_or(WasmError::InvalidModule("stack underflow"))?;
                    let slot = locals
                        .get_mut(idx)
                        .ok_or(WasmError::InvalidModule("local index out of bounds"))?;
                    *slot = val;
                }
                0x23 => {
                    // global.get
                    let idx = read_u32_leb128(code, &mut pc)? as usize;
                    let val = *self
                        .globals
                        .get(idx)
                        .ok_or(WasmError::InvalidModule("global index out of bounds"))?;
                    push_val(&mut stack, val)?;
                }
                0x24 => {
                    // global.set
                    let idx = read_u32_leb128(code, &mut pc)? as usize;
                    let val = pop_val(&mut stack)?;
                    let slot = self
                        .globals
                        .get_mut(idx)
                        .ok_or(WasmError::InvalidModule("global index out of bounds"))?;
                    *slot = val;
                }
                0x28..=0x35 => {
                    // memory loads
                    let _align = read_u32_leb128(code, &mut pc)?;
                    let offset = read_u32_leb128(code, &mut pc)? as usize;
                    let base = pop_val(&mut stack)?.as_i32() as u32 as usize;
                    let addr = base
                        .checked_add(offset)
                        .ok_or(WasmError::Trap("out of bounds memory access"))?;
                    let val = match op {
                        0x28 => {
                            WasmValue::I32(i32::from_le_bytes(self.memory.read_bytes::<4>(addr)?))
                        }
                        0x29 => {
                            WasmValue::I64(i64::from_le_bytes(self.memory.read_bytes::<8>(addr)?))
                        }
                        0x2A => {
                            WasmValue::F32(f32::from_le_bytes(self.memory.read_bytes::<4>(addr)?))
                        }
                        0x2B => {
                            WasmValue::F64(f64::from_le_bytes(self.memory.read_bytes::<8>(addr)?))
                        }
                        0x2C => {
                            WasmValue::I32(i32::from(self.memory.read_bytes::<1>(addr)?[0] as i8))
                        }
                        0x2D => WasmValue::I32(i32::from(self.memory.read_bytes::<1>(addr)?[0])),
                        0x2E => WasmValue::I32(i32::from(i16::from_le_bytes(
                            self.memory.read_bytes::<2>(addr)?,
                        ))),
                        0x2F => WasmValue::I32(i32::from(u16::from_le_bytes(
                            self.memory.read_bytes::<2>(addr)?,
                        ))),
                        0x30 => {
                            WasmValue::I64(i64::from(self.memory.read_bytes::<1>(addr)?[0] as i8))
                        }
                        0x31 => WasmValue::I64(i64::from(self.memory.read_bytes::<1>(addr)?[0])),
                        0x32 => WasmValue::I64(i64::from(i16::from_le_bytes(
                            self.memory.read_bytes::<2>(addr)?,
                        ))),
                        0x33 => WasmValue::I64(i64::from(u16::from_le_bytes(
                            self.memory.read_bytes::<2>(addr)?,
                        ))),
                        0x34 => WasmValue::I64(i64::from(i32::from_le_bytes(
                            self.memory.read_bytes::<4>(addr)?,
                        ))),
                        _ => WasmValue::I64(i64::from(u32::from_le_bytes(
                            self.memory.read_bytes::<4>(addr)?,
                        ))),
                    };
                    push_val(&mut stack, val)?;
                }
                0x36..=0x3E => {
                    // memory stores
                    let _align = read_u32_leb128(code, &mut pc)?;
                    let offset = read_u32_leb128(code, &mut pc)? as usize;
                    let val = pop_val(&mut stack)?;
                    let base = pop_val(&mut stack)?.as_i32() as u32 as usize;
                    let addr = base
                        .checked_add(offset)
                        .ok_or(WasmError::Trap("out of bounds memory access"))?;
                    match op {
                        0x36 => self.memory.write_bytes(addr, &val.as_i32().to_le_bytes())?,
                        0x37 => self.memory.write_bytes(addr, &val.as_i64().to_le_bytes())?,
                        0x38 => self.memory.write_bytes(addr, &val.as_f32().to_le_bytes())?,
                        0x39 => self.memory.write_bytes(addr, &val.as_f64().to_le_bytes())?,
                        0x3A => self.memory.write_bytes(addr, &[val.as_i32() as u8])?,
                        0x3B => self
                            .memory
                            .write_bytes(addr, &(val.as_i32() as u16).to_le_bytes())?,
                        0x3C => self.memory.write_bytes(addr, &[val.as_i64() as u8])?,
                        0x3D => self
                            .memory
                            .write_bytes(addr, &(val.as_i64() as u16).to_le_bytes())?,
                        _ => self
                            .memory
                            .write_bytes(addr, &(val.as_i64() as u32).to_le_bytes())?,
                    }
                }
                0x3F => {
                    // memory.size
                    let _mem_idx = read_u8(code, &mut pc)?;
                    push_val(&mut stack, WasmValue::I32(self.memory.pages() as i32))?;
                }
                0x40 => {
                    // memory.grow
                    let _mem_idx = read_u8(code, &mut pc)?;
                    let delta = pop_val(&mut stack)?.as_i32();
                    let prev = if delta < 0 {
                        -1
                    } else {
                        self.memory.grow(delta as u32)
                    };
                    push_val(&mut stack, WasmValue::I32(prev))?;
                }
                0x41 => {
                    let v = read_i32_leb128(code, &mut pc)?;
                    push_val(&mut stack, WasmValue::I32(v))?;
                }
                0x42 => {
                    let v = read_i64_leb128(code, &mut pc)?;
                    push_val(&mut stack, WasmValue::I64(v))?;
                }
                0x43 => {
                    let bytes = read_fixed::<4>(code, &mut pc)?;
                    push_val(&mut stack, WasmValue::F32(f32::from_le_bytes(bytes)))?;
                }
                0x44 => {
                    let bytes = read_fixed::<8>(code, &mut pc)?;
                    push_val(&mut stack, WasmValue::F64(f64::from_le_bytes(bytes)))?;
                }
                0x45 => {
                    let a = pop_val(&mut stack)?.as_i32();
                    push_val(&mut stack, WasmValue::I32(i32::from(a == 0)))?;
                }
                0x46..=0x4F => {
                    let b = pop_val(&mut stack)?.as_i32();
                    let a = pop_val(&mut stack)?.as_i32();
                    let r = match op {
                        0x46 => a == b,
                        0x47 => a != b,
                        0x48 => a < b,
                        0x49 => (a as u32) < (b as u32),
                        0x4A => a > b,
                        0x4B => (a as u32) > (b as u32),
                        0x4C => a <= b,
                        0x4D => (a as u32) <= (b as u32),
                        0x4E => a >= b,
                        _ => (a as u32) >= (b as u32),
                    };
                    push_val(&mut stack, WasmValue::I32(i32::from(r)))?;
                }
                0x50 => {
                    let a = pop_val(&mut stack)?.as_i64();
                    push_val(&mut stack, WasmValue::I32(i32::from(a == 0)))?;
                }
                0x51..=0x5A => {
                    let b = pop_val(&mut stack)?.as_i64();
                    let a = pop_val(&mut stack)?.as_i64();
                    let r = match op {
                        0x51 => a == b,
                        0x52 => a != b,
                        0x53 => a < b,
                        0x54 => (a as u64) < (b as u64),
                        0x55 => a > b,
                        0x56 => (a as u64) > (b as u64),
                        0x57 => a <= b,
                        0x58 => (a as u64) <= (b as u64),
                        0x59 => a >= b,
                        _ => (a as u64) >= (b as u64),
                    };
                    push_val(&mut stack, WasmValue::I32(i32::from(r)))?;
                }
                0x5B..=0x60 => {
                    let b = pop_val(&mut stack)?.as_f32();
                    let a = pop_val(&mut stack)?.as_f32();
                    let r = match op {
                        0x5B => a == b,
                        0x5C => a != b,
                        0x5D => a < b,
                        0x5E => a > b,
                        0x5F => a <= b,
                        _ => a >= b,
                    };
                    push_val(&mut stack, WasmValue::I32(i32::from(r)))?;
                }
                0x61..=0x66 => {
                    let b = pop_val(&mut stack)?.as_f64();
                    let a = pop_val(&mut stack)?.as_f64();
                    let r = match op {
                        0x61 => a == b,
                        0x62 => a != b,
                        0x63 => a < b,
                        0x64 => a > b,
                        0x65 => a <= b,
                        _ => a >= b,
                    };
                    push_val(&mut stack, WasmValue::I32(i32::from(r)))?;
                }
                0x67..=0x69 => {
                    let a = pop_val(&mut stack)?.as_i32() as u32;
                    let r = match op {
                        0x67 => a.leading_zeros() as i32,
                        0x68 => a.trailing_zeros() as i32,
                        _ => a.count_ones() as i32,
                    };
                    push_val(&mut stack, WasmValue::I32(r))?;
                }
                0x6A..=0x78 => {
                    let b = pop_val(&mut stack)?.as_i32();
                    let a = pop_val(&mut stack)?.as_i32();
                    let r = match op {
                        0x6A => a.wrapping_add(b),
                        0x6B => a.wrapping_sub(b),
                        0x6C => a.wrapping_mul(b),
                        0x6D => {
                            if b == 0 {
                                return Err(WasmError::Trap("integer divide by zero"));
                            }
                            a.checked_div(b)
                                .ok_or(WasmError::Trap("integer overflow"))?
                        }
                        0x6E => {
                            if b == 0 {
                                return Err(WasmError::Trap("integer divide by zero"));
                            }
                            ((a as u32) / (b as u32)) as i32
                        }
                        0x6F => {
                            if b == 0 {
                                return Err(WasmError::Trap("integer divide by zero"));
                            }
                            a.wrapping_rem(b)
                        }
                        0x70 => {
                            if b == 0 {
                                return Err(WasmError::Trap("integer divide by zero"));
                            }
                            ((a as u32) % (b as u32)) as i32
                        }
                        0x71 => a & b,
                        0x72 => a | b,
                        0x73 => a ^ b,
                        0x74 => a.wrapping_shl(b as u32),
                        0x75 => a.wrapping_shr(b as u32),
                        0x76 => ((a as u32).wrapping_shr(b as u32)) as i32,
                        0x77 => a.rotate_left(b as u32),
                        _ => a.rotate_right(b as u32),
                    };
                    push_val(&mut stack, WasmValue::I32(r))?;
                }
                0x79..=0x7B => {
                    let a = pop_val(&mut stack)?.as_i64() as u64;
                    let r = match op {
                        0x79 => i64::from(a.leading_zeros()),
                        0x7A => i64::from(a.trailing_zeros()),
                        _ => i64::from(a.count_ones()),
                    };
                    push_val(&mut stack, WasmValue::I64(r))?;
                }
                0x7C..=0x8A => {
                    let b = pop_val(&mut stack)?.as_i64();
                    let a = pop_val(&mut stack)?.as_i64();
                    let r = match op {
                        0x7C => a.wrapping_add(b),
                        0x7D => a.wrapping_sub(b),
                        0x7E => a.wrapping_mul(b),
                        0x7F => {
                            if b == 0 {
                                return Err(WasmError::Trap("integer divide by zero"));
                            }
                            a.checked_div(b)
                                .ok_or(WasmError::Trap("integer overflow"))?
                        }
                        0x80 => {
                            if b == 0 {
                                return Err(WasmError::Trap("integer divide by zero"));
                            }
                            ((a as u64) / (b as u64)) as i64
                        }
                        0x81 => {
                            if b == 0 {
                                return Err(WasmError::Trap("integer divide by zero"));
                            }
                            a.wrapping_rem(b)
                        }
                        0x82 => {
                            if b == 0 {
                                return Err(WasmError::Trap("integer divide by zero"));
                            }
                            ((a as u64) % (b as u64)) as i64
                        }
                        0x83 => a & b,
                        0x84 => a | b,
                        0x85 => a ^ b,
                        0x86 => a.wrapping_shl(b as u32),
                        0x87 => a.wrapping_shr(b as u32),
                        0x88 => ((a as u64).wrapping_shr(b as u32)) as i64,
                        0x89 => a.rotate_left(b as u32),
                        _ => a.rotate_right(b as u32),
                    };
                    push_val(&mut stack, WasmValue::I64(r))?;
                }
                0x8B..=0x91 => {
                    let a = pop_val(&mut stack)?.as_f32();
                    let r = match op {
                        0x8B => a.abs(),
                        0x8C => -a,
                        0x8D => a.ceil(),
                        0x8E => a.floor(),
                        0x8F => a.trunc(),
                        0x90 => a.round_ties_even(),
                        _ => a.sqrt(),
                    };
                    push_val(&mut stack, WasmValue::F32(r))?;
                }
                0x92..=0x98 => {
                    let b = pop_val(&mut stack)?.as_f32();
                    let a = pop_val(&mut stack)?.as_f32();
                    let r = match op {
                        0x92 => a + b,
                        0x93 => a - b,
                        0x94 => a * b,
                        0x95 => a / b,
                        0x96 => a.min(b),
                        0x97 => a.max(b),
                        _ => a.copysign(b),
                    };
                    push_val(&mut stack, WasmValue::F32(r))?;
                }
                0x99..=0x9F => {
                    let a = pop_val(&mut stack)?.as_f64();
                    let r = match op {
                        0x99 => a.abs(),
                        0x9A => -a,
                        0x9B => a.ceil(),
                        0x9C => a.floor(),
                        0x9D => a.trunc(),
                        0x9E => a.round_ties_even(),
                        _ => a.sqrt(),
                    };
                    push_val(&mut stack, WasmValue::F64(r))?;
                }
                0xA0..=0xA6 => {
                    let b = pop_val(&mut stack)?.as_f64();
                    let a = pop_val(&mut stack)?.as_f64();
                    let r = match op {
                        0xA0 => a + b,
                        0xA1 => a - b,
                        0xA2 => a * b,
                        0xA3 => a / b,
                        0xA4 => a.min(b),
                        0xA5 => a.max(b),
                        _ => a.copysign(b),
                    };
                    push_val(&mut stack, WasmValue::F64(r))?;
                }
                0xA7..=0xC4 => {
                    let v = pop_val(&mut stack)?;
                    let out = match op {
                        0xA7 => WasmValue::I32(v.as_i64() as i32),
                        0xA8 => WasmValue::I32(v.as_f32() as i32),
                        0xA9 => WasmValue::I32((v.as_f32() as u32) as i32),
                        0xAA => WasmValue::I32(v.as_f64() as i32),
                        0xAB => WasmValue::I32((v.as_f64() as u32) as i32),
                        0xAC => WasmValue::I64(i64::from(v.as_i32())),
                        0xAD => WasmValue::I64(i64::from(v.as_i32() as u32)),
                        0xAE => WasmValue::I64(v.as_f32() as i64),
                        0xAF => WasmValue::I64((v.as_f32() as u64) as i64),
                        0xB0 => WasmValue::I64(v.as_f64() as i64),
                        0xB1 => WasmValue::I64((v.as_f64() as u64) as i64),
                        0xB2 => WasmValue::F32(v.as_i32() as f32),
                        0xB3 => WasmValue::F32((v.as_i32() as u32) as f32),
                        0xB4 => WasmValue::F32(v.as_i64() as f32),
                        0xB5 => WasmValue::F32((v.as_i64() as u64) as f32),
                        0xB6 => WasmValue::F32(v.as_f64() as f32),
                        0xB7 => WasmValue::F64(f64::from(v.as_i32())),
                        0xB8 => WasmValue::F64(f64::from(v.as_i32() as u32)),
                        0xB9 => WasmValue::F64(v.as_i64() as f64),
                        0xBA => WasmValue::F64((v.as_i64() as u64) as f64),
                        0xBB => WasmValue::F64(f64::from(v.as_f32())),
                        0xBC => WasmValue::I32(v.as_f32().to_bits() as i32),
                        0xBD => WasmValue::I64(v.as_f64().to_bits() as i64),
                        0xBE => WasmValue::F32(f32::from_bits(v.as_i32() as u32)),
                        0xBF => WasmValue::F64(f64::from_bits(v.as_i64() as u64)),
                        0xC0 => WasmValue::I32(i32::from(v.as_i32() as i8)),
                        0xC1 => WasmValue::I32(i32::from(v.as_i32() as i16)),
                        0xC2 => WasmValue::I64(i64::from(v.as_i64() as i8)),
                        0xC3 => WasmValue::I64(i64::from(v.as_i64() as i16)),
                        _ => WasmValue::I64(i64::from(v.as_i64() as i32)),
                    };
                    push_val(&mut stack, out)?;
                }
                0xFC => {
                    let sub_op = read_u32_leb128(code, &mut pc)?;
                    match sub_op {
                        10 => {
                            // memory.copy
                            let _dst_mem = read_u8(code, &mut pc)?;
                            let _src_mem = read_u8(code, &mut pc)?;
                            let len = pop_val(&mut stack)?.as_i32() as u32 as usize;
                            let src = pop_val(&mut stack)?.as_i32() as u32 as usize;
                            let dst = pop_val(&mut stack)?.as_i32() as u32 as usize;
                            let src_end = src
                                .checked_add(len)
                                .ok_or(WasmError::Trap("out of bounds memory access"))?;
                            let dst_end = dst
                                .checked_add(len)
                                .ok_or(WasmError::Trap("out of bounds memory access"))?;
                            if src_end > self.memory.bytes.len()
                                || dst_end > self.memory.bytes.len()
                            {
                                return Err(WasmError::Trap("out of bounds memory access"));
                            }
                            self.memory.bytes.copy_within(src..src_end, dst);
                        }
                        11 => {
                            // memory.fill
                            let _mem = read_u8(code, &mut pc)?;
                            let len = pop_val(&mut stack)?.as_i32() as u32 as usize;
                            let val = pop_val(&mut stack)?.as_i32() as u8;
                            let dst = pop_val(&mut stack)?.as_i32() as u32 as usize;
                            let end = dst
                                .checked_add(len)
                                .ok_or(WasmError::Trap("out of bounds memory access"))?;
                            let slice = self
                                .memory
                                .bytes
                                .get_mut(dst..end)
                                .ok_or(WasmError::Trap("out of bounds memory access"))?;
                            slice.fill(val);
                        }
                        _ => return Err(WasmError::InvalidModule("unsupported 0xFC opcode")),
                    }
                }
                _ => return Err(WasmError::InvalidModule("unsupported wasm opcode")),
            }
        }

        let arity = sig.results.len();
        if stack.len() < arity {
            return Err(WasmError::InvalidModule(
                "stack underflow at function return",
            ));
        }
        Ok(stack.split_off(stack.len() - arity))
    }
}

fn push_val(stack: &mut Vec<WasmValue>, val: WasmValue) -> Result<(), WasmError> {
    if stack.len() >= MAX_WASM_STACK {
        return Err(WasmError::Trap("operand stack overflow"));
    }
    stack.push(val);
    Ok(())
}

fn pop_val(stack: &mut Vec<WasmValue>) -> Result<WasmValue, WasmError> {
    stack
        .pop()
        .ok_or(WasmError::InvalidModule("operand stack underflow"))
}

fn unwind_stack(
    stack: &mut Vec<WasmValue>,
    target_height: usize,
    arity: usize,
) -> Result<(), WasmError> {
    if stack.len() < target_height.saturating_add(arity) {
        return Err(WasmError::InvalidModule("stack underflow on block exit"));
    }
    let kept: Vec<WasmValue> = stack.split_off(stack.len() - arity);
    stack.truncate(target_height);
    stack.extend(kept);
    Ok(())
}

fn branch_to_label(
    label_depth: usize,
    control: &mut Vec<ControlFrame>,
    stack: &mut Vec<WasmValue>,
    pc: &mut usize,
) -> Result<(), WasmError> {
    if label_depth >= control.len() {
        return Err(WasmError::InvalidModule("branch label depth out of bounds"));
    }
    let target_idx = control.len() - 1 - label_depth;
    let target = control[target_idx];
    if target.kind == BlockKind::Loop {
        // Branching to a loop restarts at start_pc with 0 label arguments
        control.truncate(target_idx + 1);
        unwind_stack(stack, target.stack_height, 0)?;
        *pc = target.start_pc;
    } else {
        control.truncate(target_idx);
        unwind_stack(stack, target.stack_height, target.arity)?;
        *pc = target.end_pc + 1;
    }
    Ok(())
}

fn scan_block_bounds(code: &[u8], start_pc: usize) -> Result<(usize, Option<usize>), WasmError> {
    let mut p = start_pc;
    let mut depth = 1usize;
    let mut else_pc = None;
    while p < code.len() {
        let op = read_u8(code, &mut p)?;
        match op {
            0x02..=0x04 => {
                let _ = read_u8(code, &mut p)?;
                depth += 1;
            }
            0x05 => {
                if depth == 1 && else_pc.is_none() {
                    else_pc = Some(p - 1);
                }
            }
            0x0B => {
                depth -= 1;
                if depth == 0 {
                    return Ok((p - 1, else_pc));
                }
            }
            0x0C | 0x0D | 0x10 | 0x20..=0x24 | 0x41 => {
                let _ = read_i32_leb128(code, &mut p)?;
            }
            0x42 => {
                let _ = read_i64_leb128(code, &mut p)?;
            }
            0x0E => {
                let count = read_u32_leb128(code, &mut p)? as usize;
                for _ in 0..=count {
                    let _ = read_u32_leb128(code, &mut p)?;
                }
            }
            0x11 => {
                let _ = read_u32_leb128(code, &mut p)?;
                let _ = read_u8(code, &mut p)?;
            }
            0x28..=0x3E => {
                let _ = read_u32_leb128(code, &mut p)?;
                let _ = read_u32_leb128(code, &mut p)?;
            }
            0x3F | 0x40 => {
                let _ = read_u8(code, &mut p)?;
            }
            0x43 => {
                let _ = read_fixed::<4>(code, &mut p)?;
            }
            0x44 => {
                let _ = read_fixed::<8>(code, &mut p)?;
            }
            0xFC => {
                let sub = read_u32_leb128(code, &mut p)?;
                if sub == 10 {
                    let _ = read_u8(code, &mut p)?;
                    let _ = read_u8(code, &mut p)?;
                } else if sub == 11 {
                    let _ = read_u8(code, &mut p)?;
                }
            }
            _ => {}
        }
    }
    Err(WasmError::InvalidModule("unclosed block in wasm bytecode"))
}

fn eval_const_expr(
    bytes: &[u8],
    pos: &mut usize,
    globals: &[WasmGlobalDef],
) -> Result<WasmValue, WasmError> {
    let op = read_u8(bytes, pos)?;
    let val = match op {
        0x41 => WasmValue::I32(read_i32_leb128(bytes, pos)?),
        0x42 => WasmValue::I64(read_i64_leb128(bytes, pos)?),
        0x43 => WasmValue::F32(f32::from_le_bytes(read_fixed::<4>(bytes, pos)?)),
        0x44 => WasmValue::F64(f64::from_le_bytes(read_fixed::<8>(bytes, pos)?)),
        0x23 => {
            let idx = read_u32_leb128(bytes, pos)? as usize;
            globals
                .get(idx)
                .map(|g| g.init)
                .ok_or(WasmError::InvalidModule("global.get index out of bounds"))?
        }
        _ => {
            return Err(WasmError::InvalidModule(
                "invalid constant expression opcode",
            ));
        }
    };
    let end = read_u8(bytes, pos)?;
    if end != 0x0B {
        return Err(WasmError::InvalidModule("constant expression missing 0x0B"));
    }
    Ok(val)
}

fn read_u8(bytes: &[u8], pos: &mut usize) -> Result<u8, WasmError> {
    let b = *bytes
        .get(*pos)
        .ok_or(WasmError::InvalidModule("unexpected end of wasm binary"))?;
    *pos += 1;
    Ok(b)
}

fn read_fixed<const N: usize>(bytes: &[u8], pos: &mut usize) -> Result<[u8; N], WasmError> {
    let end = pos
        .checked_add(N)
        .ok_or(WasmError::InvalidModule("unexpected end of wasm binary"))?;
    let slice = bytes
        .get(*pos..end)
        .ok_or(WasmError::InvalidModule("unexpected end of wasm binary"))?;
    let mut out = [0u8; N];
    out.copy_from_slice(slice);
    *pos = end;
    Ok(out)
}

/// Read an unsigned 32-bit LEB128 integer.
pub fn read_u32_leb128(bytes: &[u8], pos: &mut usize) -> Result<u32, WasmError> {
    let mut result = 0u32;
    let mut shift = 0u32;
    for _ in 0..5 {
        let byte = read_u8(bytes, pos)?;
        result |= u32::from(byte & 0x7F) << shift;
        if (byte & 0x80) == 0 {
            return Ok(result);
        }
        shift += 7;
    }
    Err(WasmError::InvalidModule("invalid u32 leb128"))
}

/// Read a signed 32-bit LEB128 integer.
pub fn read_i32_leb128(bytes: &[u8], pos: &mut usize) -> Result<i32, WasmError> {
    let mut result = 0i64;
    let mut shift = 0u32;
    for _ in 0..5 {
        let byte = read_u8(bytes, pos)?;
        result |= i64::from(byte & 0x7F) << shift;
        shift += 7;
        if (byte & 0x80) == 0 {
            if shift < 64 && (byte & 0x40) != 0 {
                result |= !0i64 << shift;
            }
            return Ok(result as i32);
        }
    }
    Err(WasmError::InvalidModule("invalid i32 leb128"))
}

/// Read a signed 64-bit LEB128 integer.
pub fn read_i64_leb128(bytes: &[u8], pos: &mut usize) -> Result<i64, WasmError> {
    let mut result = 0i128;
    let mut shift = 0u32;
    for _ in 0..10 {
        let byte = read_u8(bytes, pos)?;
        result |= i128::from(byte & 0x7F) << shift;
        shift += 7;
        if (byte & 0x80) == 0 {
            if shift < 128 && (byte & 0x40) != 0 {
                result |= !0i128 << shift;
            }
            return Ok(result as i64);
        }
    }
    Err(WasmError::InvalidModule("invalid i64 leb128"))
}

fn read_name(bytes: &[u8], pos: &mut usize) -> Result<String, WasmError> {
    let len = read_u32_leb128(bytes, pos)? as usize;
    let end = pos
        .checked_add(len)
        .ok_or(WasmError::InvalidModule("name length overflow"))?;
    let slice = bytes
        .get(*pos..end)
        .ok_or(WasmError::InvalidModule("name truncated"))?;
    *pos = end;
    std::str::from_utf8(slice)
        .map(str::to_string)
        .map_err(|_| WasmError::InvalidModule("invalid utf-8 name"))
}

fn read_limits(bytes: &[u8], pos: &mut usize) -> Result<(u32, Option<u32>), WasmError> {
    let flags = read_u8(bytes, pos)?;
    let initial = read_u32_leb128(bytes, pos)?;
    let max = if (flags & 0x01) != 0 {
        Some(read_u32_leb128(bytes, pos)?)
    } else {
        None
    };
    Ok((initial, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_validates_and_executes_wasm_fibonacci_and_linear_memory() {
        // Hand-assembled WebAssembly 1.0 module with:
        // - memory (1 page), exported as "memory"
        // - func $add(i32, i32) -> i32, exported as "add"
        // - func $fib(i32) -> i32 (iterative loop), exported as "fib"
        // - func $store_and_load(i32, i32) -> i32, exported as "mem_roundtrip"
        let wasm_bytes: Vec<u8> = vec![
            0x00, 0x61, 0x73, 0x6D, // magic "\0asm"
            0x01, 0x00, 0x00, 0x00, // version 1
            // Type section (id=1): 2 types
            // type 0: (i32, i32) -> i32
            // type 1: (i32) -> i32
            0x01, 0x0C, 0x02, 0x60, 0x02, 0x7F, 0x7F, 0x01, 0x7F, 0x60, 0x01, 0x7F, 0x01, 0x7F,
            // Function section (id=3): 3 functions (types 0, 1, 0)
            0x03, 0x04, 0x03, 0x00, 0x01, 0x00,
            // Memory section (id=5): 1 memory (initial=1)
            0x05, 0x03, 0x01, 0x00, 0x01,
            // Export section (id=7): 4 exports ("memory", "add", "fib", "mem_roundtrip")
            0x07, 0x26, 0x04, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x03, b'a',
            b'd', b'd', 0x00, 0x00, 0x03, b'f', b'i', b'b', 0x00, 0x01, 0x0D, b'm', b'e', b'm',
            b'_', b'r', b'o', b'u', b'n', b'd', b't', b'r', b'i', b'p', 0x00, 0x02,
            // Code section (id=10): 3 function bodies
            0x0A, 0x4A, 0x03, // func 0 ($add): local.get 0, local.get 1, i32.add, end
            0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6A, 0x0B,
            // func 1 ($fib): (param $n i32) (local $a i32, $b i32, $tmp i32)
            // $a = 0, $b = 1; loop while $n > 0: $tmp = $a + $b; $a = $b; $b = $tmp; $n = $n - 1
            0x31, 0x01, 0x03, 0x7F, 0x41, 0x00, 0x21, 0x01, 0x41, 0x01, 0x21, 0x02, 0x02, 0x40,
            0x03, 0x40, 0x20, 0x00, 0x45, 0x0D, 0x01, 0x20, 0x01, 0x20, 0x02, 0x6A, 0x21, 0x03,
            0x20, 0x02, 0x21, 0x01, 0x20, 0x03, 0x21, 0x02, 0x20, 0x00, 0x41, 0x01, 0x6B, 0x21,
            0x00, 0x0C, 0x00, 0x0B, 0x0B, 0x20, 0x01, 0x0B,
            // func 2 ($mem_roundtrip): local.get 0, local.get 1, i32.store, local.get 0, i32.load, end
            0x0E, 0x00, 0x20, 0x00, 0x20, 0x01, 0x36, 0x02, 0x00, 0x20, 0x00, 0x28, 0x02, 0x00,
            0x0B,
        ];

        assert!(validate_wasm(&wasm_bytes));
        let module = WasmModule::decode(&wasm_bytes).unwrap();
        let mut instance = WasmInstance::instantiate(module, HashMap::new()).unwrap();

        let sum = instance.call_export("add", &[19.0, 23.0]).unwrap();
        assert_eq!(sum, vec![WasmValue::I32(42)]);

        let fib10 = instance.call_export("fib", &[10.0]).unwrap();
        assert_eq!(fib10, vec![WasmValue::I32(55)]);

        let rt = instance
            .call_export("mem_roundtrip", &[64.0, 1337.0])
            .unwrap();
        assert_eq!(rt, vec![WasmValue::I32(1337)]);
    }

    #[test]
    fn executes_webassembly_js_api_typed_arrays_and_script_comments() {
        use crate::ScriptRuntime;
        use anvil_dom::parse_html;

        let mut doc = parse_html(
            "<html><body>\
             <div id=\"out\">init</div>\
             <button id=\"btn\" onclick=\"bump()\">Click</button>\
             <script>\
             // Single-line comment inside inline script\n\
             let count = 0;\n\
             function bump() { count = count + 1; document.getElementById('out').textContent = 'clicked:' + count; }\n\
             const bytes = new Uint8Array([\
                 0, 97, 115, 109, 1, 0, 0, 0,\
                 1, 7, 1, 96, 2, 127, 127, 1, 127,\
                 3, 2, 1, 0,\
                 5, 3, 1, 0, 1,\
                 7, 16, 2, 6, 109, 101, 109, 111, 114, 121, 2, 0, 3, 97, 100, 100, 0, 0,\
                 10, 9, 1, 7, 0, 32, 0, 32, 1, 106, 11\
             ]);\n\
             const valid = WebAssembly.validate(bytes);\n\
             const mod = new WebAssembly.Module(bytes);\n\
             const inst = new WebAssembly.Instance(mod);\n\
             const sum = inst.exports.add(18, 24);\n\
             const enc = new TextEncoder();\n\
             const dec = new TextDecoder();\n\
             const greeting = dec.decode(enc.encode('Hello, ' + 'WASM'));\n\
             const b64 = btoa('ok');\n\
             document.getElementById('out').textContent = greeting + ':' + valid + ':' + sum + ':' + atob(b64);\n\
             </script>\
             </body></html>",
        );

        let mut rt = ScriptRuntime::new();
        rt.run_document_scripts(&mut doc);
        let out_id = doc.get_element_by_id("out").unwrap();
        assert_eq!(doc.text_content(out_id), "Hello, WASM:true:42:ok");

        let btn_id = doc.get_element_by_id("btn").unwrap();
        let mut hydrated_rt = ScriptRuntime::new();
        hydrated_rt.hydrate_interactive_bindings(&mut doc);
        assert!(hydrated_rt.dispatch_click(&mut doc, btn_id));
        assert_eq!(doc.text_content(out_id), "clicked:1");
    }
}
