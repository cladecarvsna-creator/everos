//! JavaScript, through the QuickJS engine (kernel/quickjs, compiled by
//! build.rs). Scripts call into the kernel with `__native(op, ...args)`,
//! which lands in the current [`Host`].

mod libc;

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;

pub use libc::now_ms;

use crate::interrupts;

extern "C" {
    fn ejs_new(memory_limit: usize, stack_size: usize) -> *mut c_void;
    fn ejs_free(ctx: *mut c_void);
    fn ejs_eval(
        ctx: *mut c_void,
        src: *const u8,
        len: usize,
        filename: *const c_char,
        out: *mut *mut u8,
        out_len: *mut usize,
    ) -> c_int;
    fn ejs_run_jobs(ctx: *mut c_void);
}

/// What a native call returns to JavaScript.
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    /// JSON text, parsed on the JavaScript side.
    Json(String),
    /// Thrown as a TypeError.
    Error(String),
}

/// The embedder: answers `__native` calls from scripts.
pub trait Host {
    fn call(&mut self, op: &str, args: &[Option<&str>]) -> Value;
}

/// The host of the script that is running now.
static mut HOST: Option<*mut dyn Host> = None;
/// Scripts are stopped when the timer passes this tick.
static mut DEADLINE: u64 = u64::MAX;

pub struct Context {
    ctx: *mut c_void,
}

/// How long one script, event handler or timer may run.
pub const SCRIPT_MS: u64 = 4000;

impl Context {
    pub fn new() -> Option<Context> {
        let ctx = unsafe { ejs_new(96 * 1024 * 1024, 600 * 1024) };
        if ctx.is_null() {
            None
        } else {
            Some(Context { ctx })
        }
    }

    /// Run `source` as a global script with `host` answering native calls.
    /// Returns the completion value as text, or the error with its stack.
    pub fn eval(&mut self, host: &mut dyn Host, source: &str, filename: &str) -> Result<String, String> {
        let mut name = Vec::with_capacity(filename.len() + 1);
        name.extend_from_slice(filename.as_bytes());
        name.retain(|&b| b != 0);
        name.push(0);
        let mut out: *mut u8 = null_mut();
        let mut out_len = 0usize;
        let r = self.with_host(host, |ctx| unsafe {
            ejs_eval(
                ctx,
                source.as_ptr(),
                source.len(),
                name.as_ptr() as *const c_char,
                &mut out,
                &mut out_len,
            )
        });
        let text = if out.is_null() {
            String::new()
        } else {
            let s = String::from_utf8_lossy(unsafe { core::slice::from_raw_parts(out, out_len) }).into_owned();
            unsafe { libc::free(out) };
            s
        };
        if r == 0 {
            Ok(text)
        } else {
            Err(text)
        }
    }

    /// Run queued promise reactions.
    pub fn run_jobs(&mut self, host: &mut dyn Host) {
        self.with_host(host, |ctx| unsafe { ejs_run_jobs(ctx) });
    }

    fn with_host<R>(&mut self, host: &mut dyn Host, f: impl FnOnce(*mut c_void) -> R) -> R {
        // Safety: the host outlives the call, and HOST is cleared (or
        // restored, for nested calls) before it returns.
        let host: *mut (dyn Host + '_) = host;
        let host: *mut (dyn Host + 'static) = unsafe { core::mem::transmute(host) };
        unsafe {
            let saved = (HOST, DEADLINE);
            HOST = Some(host);
            if saved.0.is_none() {
                DEADLINE = interrupts::ticks() + SCRIPT_MS * interrupts::TIMER_HZ / 1000;
            }
            let r = f(self.ctx);
            HOST = saved.0;
            DEADLINE = saved.1;
            r
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { ejs_free(self.ctx) };
    }
}

#[no_mangle]
unsafe extern "C" fn everos_js_native(
    op: *const u8,
    op_len: usize,
    argc: c_int,
    argv: *const *const u8,
    lens: *const usize,
    out: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    let text = |p: *const u8, n: usize| -> Option<&str> {
        if p.is_null() {
            None
        } else {
            Some(core::str::from_utf8(core::slice::from_raw_parts(p, n)).unwrap_or(""))
        }
    };
    let op = text(op, op_len).unwrap_or("");
    let mut args = Vec::with_capacity(argc as usize);
    for i in 0..argc as usize {
        args.push(text(*argv.add(i), *lens.add(i)));
    }
    let Some(host) = HOST else {
        return 0;
    };
    let value = (*host).call(op, &args);
    let (kind, s) = match value {
        Value::Undefined => (0, None),
        Value::Str(s) => (1, Some(s)),
        Value::Int(v) => (2, Some(alloc::format!("{}", v))),
        Value::Json(s) => (3, Some(s)),
        Value::Bool(true) => (4, None),
        Value::Bool(false) => (5, None),
        Value::Null => (6, None),
        Value::Error(s) => (7, Some(s)),
    };
    if let Some(s) = s {
        let p = libc::malloc(s.len().max(1));
        if !p.is_null() {
            core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            *out = p;
            *out_len = s.len();
        }
    }
    kind
}

#[no_mangle]
extern "C" fn everos_js_interrupt() -> c_int {
    (interrupts::ticks() > unsafe { DEADLINE }) as c_int
}

/// A host for the shell's `js` command: console output only.
pub struct ConsoleHost;

impl Host for ConsoleHost {
    fn call(&mut self, op: &str, args: &[Option<&str>]) -> Value {
        match op {
            "log" => {
                crate::println!("{}", args.first().copied().flatten().unwrap_or(""));
                Value::Undefined
            }
            "now" => Value::Int(now_ms()),
            _ => Value::Undefined,
        }
    }
}

/// Glue every page and the shell get: console.log and friends.
pub const BASE_PRELUDE: &str = r#"
globalThis.window = globalThis; globalThis.self = globalThis;
(function(){
  const fmt = a => a.map(v => typeof v === 'string' ? v : (() => { try { return JSON.stringify(v) ?? String(v); } catch(e) { return String(v); } })()).join(' ');
  const log = (...a) => __native('log', fmt(a));
  globalThis.console = { log, info: log, warn: log, error: log, debug: log, trace: log, dir: log, table: log, group: log, groupCollapsed: log, groupEnd(){}, time(){}, timeEnd(){}, assert(c, ...a){ if(!c) log('Assertion failed:', ...a); } };
})();
"#;
