//! The Node-API addon of the GameStream server. Each function forwards one
//! call of bindings/node.js to the C interface of the library
//! (src/native.rs); the server's doorbell reaches JavaScript through a
//! thread-safe function. Node's FFI is experimental, and Bun's thread-safe
//! FFI callbacks are unsafe off the JavaScript thread, so both load this
//! addon instead of the library.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_void};
use std::ptr::{null, null_mut};

use sandom_gamestream::native::{self as library, NativeServer};

type napi_env = *mut c_void;
type napi_value = *mut c_void;
type napi_callback_info = *mut c_void;
type napi_threadsafe_function = *mut c_void;
type napi_status = i32;
type napi_callback = unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value;
type napi_threadsafe_function_call_js = unsafe extern "C" fn(napi_env, napi_value, *mut c_void, *mut c_void);

const NAPI_OK: napi_status = 0;
const NAPI_TSFN_RELEASE: i32 = 0;
const NAPI_TSFN_NONBLOCKING: i32 = 0;
const NAPI_UINT8_ARRAY: i32 = 1;

unsafe extern "C" {
    fn napi_create_function(env: napi_env, name: *const c_char, length: usize, callback: napi_callback, data: *mut c_void,
        result: *mut napi_value) -> napi_status;
    fn napi_set_named_property(env: napi_env, object: napi_value, name: *const c_char, value: napi_value) -> napi_status;
    fn napi_get_cb_info(env: napi_env, info: napi_callback_info, argc: *mut usize, argv: *mut napi_value, this: *mut napi_value,
        data: *mut *mut c_void) -> napi_status;
    fn napi_get_value_uint32(env: napi_env, value: napi_value, result: *mut u32) -> napi_status;
    fn napi_get_value_int64(env: napi_env, value: napi_value, result: *mut i64) -> napi_status;
    fn napi_create_int32(env: napi_env, value: i32, result: *mut napi_value) -> napi_status;
    fn napi_create_int64(env: napi_env, value: i64, result: *mut napi_value) -> napi_status;
    fn napi_create_string_utf8(env: napi_env, text: *const c_char, length: usize, result: *mut napi_value) -> napi_status;
    fn napi_get_typedarray_info(env: napi_env, value: napi_value, kind: *mut i32, length: *mut usize, data: *mut *mut c_void,
        buffer: *mut napi_value, offset: *mut usize) -> napi_status;
    fn napi_create_external(env: napi_env, data: *mut c_void, finalize: *mut c_void, hint: *mut c_void,
        result: *mut napi_value) -> napi_status;
    fn napi_get_value_external(env: napi_env, value: napi_value, result: *mut *mut c_void) -> napi_status;
    fn napi_get_undefined(env: napi_env, result: *mut napi_value) -> napi_status;
    fn napi_throw_error(env: napi_env, code: *const c_char, message: *const c_char) -> napi_status;
    fn napi_call_function(env: napi_env, receiver: napi_value, function: napi_value, argc: usize, argv: *const napi_value,
        result: *mut napi_value) -> napi_status;
    fn napi_create_threadsafe_function(env: napi_env, function: napi_value, resource: napi_value, resource_name: napi_value,
        max_queue_size: usize, initial_thread_count: usize, finalize_data: *mut c_void, finalize: *mut c_void,
        context: *mut c_void, call_js: Option<napi_threadsafe_function_call_js>, result: *mut napi_threadsafe_function)
        -> napi_status;
    fn napi_call_threadsafe_function(function: napi_threadsafe_function, data: *mut c_void, mode: i32) -> napi_status;
    fn napi_release_threadsafe_function(function: napi_threadsafe_function, mode: i32) -> napi_status;
    fn napi_ref_threadsafe_function(env: napi_env, function: napi_threadsafe_function) -> napi_status;
    fn napi_unref_threadsafe_function(env: napi_env, function: napi_threadsafe_function) -> napi_status;
}

/// Throws a JavaScript error with `message` and returns no value.
unsafe fn throw(env: napi_env, message: &str) -> napi_value {
    let text = std::ffi::CString::new(message.replace('\0', " ")).unwrap();
    unsafe { napi_throw_error(env, null(), text.as_ptr()) };
    null_mut()
}

fn checked(status: napi_status, operation: &str) -> Result<(), String> {
    if status == NAPI_OK { Ok(()) } else { Err(format!("Node-API {operation} failed with status {status}")) }
}

/// The arguments of one call.
struct Arguments {
    env: napi_env,
    values: [napi_value; 6],
}

impl Arguments {
    unsafe fn u32(&self, index: usize) -> Result<u32, String> {
        let mut value = 0;
        checked(unsafe { napi_get_value_uint32(self.env, self.values[index], &mut value) }, &format!("argument {index} as a number"))?;
        Ok(value)
    }

    unsafe fn i64(&self, index: usize) -> Result<i64, String> {
        let mut value = 0;
        checked(unsafe { napi_get_value_int64(self.env, self.values[index], &mut value) }, &format!("argument {index} as an integer"))?;
        Ok(value)
    }

    unsafe fn external(&self, index: usize) -> Result<*mut c_void, String> {
        let mut value = null_mut();
        checked(unsafe { napi_get_value_external(self.env, self.values[index], &mut value) }, &format!("argument {index} as a handle"))?;
        Ok(value)
    }

    unsafe fn server(&self) -> Result<*mut NativeServer, String> {
        Ok(unsafe { self.external(0) }? as *mut NativeServer)
    }

    /// The bytes of a Uint8Array: its first byte and its length.
    unsafe fn bytes(&self, index: usize) -> Result<(*mut u8, usize), String> {
        let (mut kind, mut length, mut data, mut buffer, mut offset) = (0, 0, null_mut(), null_mut(), 0);
        checked(unsafe { napi_get_typedarray_info(self.env, self.values[index], &mut kind, &mut length, &mut data,
            &mut buffer, &mut offset) }, &format!("argument {index} as a typed array"))?;
        if kind != NAPI_UINT8_ARRAY {
            return Err(format!("argument {index} is a typed array of kind {kind}; the addon takes a Uint8Array"));
        }
        Ok((data as *mut u8, length))
    }
}

/// Runs `body` with the arguments of the call; a failure throws.
unsafe fn call(env: napi_env, info: napi_callback_info, body: impl FnOnce(&Arguments) -> Result<napi_value, String>) -> napi_value {
    let mut arguments = Arguments { env, values: [null_mut(); 6] };
    let mut count = arguments.values.len();
    if let Err(message) = checked(unsafe { napi_get_cb_info(env, info, &mut count, arguments.values.as_mut_ptr(), null_mut(),
        null_mut()) }, "reading the arguments") {
        return unsafe { throw(env, &message) };
    }
    match body(&arguments) {
        Ok(value) => value,
        Err(message) => unsafe { throw(env, &message) },
    }
}

unsafe fn int32(env: napi_env, value: i32) -> Result<napi_value, String> {
    let mut result = null_mut();
    checked(unsafe { napi_create_int32(env, value, &mut result) }, "creating a number")?;
    Ok(result)
}

unsafe fn string(env: napi_env, bytes: &[u8]) -> Result<napi_value, String> {
    let mut result = null_mut();
    checked(unsafe { napi_create_string_utf8(env, bytes.as_ptr() as *const c_char, bytes.len(), &mut result) }, "creating a string")?;
    Ok(result)
}

unsafe fn external(env: napi_env, value: *mut c_void) -> Result<napi_value, String> {
    let mut result = null_mut();
    checked(unsafe { napi_create_external(env, value, null_mut(), null_mut(), &mut result) }, "creating a handle")?;
    Ok(result)
}

unsafe fn undefined(env: napi_env) -> Result<napi_value, String> {
    let mut result = null_mut();
    checked(unsafe { napi_get_undefined(env, &mut result) }, "reading undefined")?;
    Ok(result)
}

/// The server thread's ring: queues a call of the doorbell function on the
/// JavaScript thread, without blocking.
extern "C" fn ring(context: *mut c_void) {
    let status = unsafe { napi_call_threadsafe_function(context, null_mut(), NAPI_TSFN_NONBLOCKING) };
    if status != NAPI_OK {
        panic!("Node-API refused the GameStream server's doorbell: status {status}");
    }
}

/// The JavaScript thread's call: runs the doorbell function.
unsafe extern "C" fn ring_to_javascript(env: napi_env, function: napi_value, _context: *mut c_void, _data: *mut c_void) {
    if env.is_null() { return; }
    let mut receiver = null_mut();
    let mut result = null_mut();
    unsafe {
        napi_get_undefined(env, &mut receiver);
        napi_call_function(env, receiver, function, 0, null(), &mut result);
    }
}

/// `doorbell(doorbellFunction)`: the thread-safe function that the server
/// rings, unreferenced, so that it keeps no process alive by itself.
unsafe extern "C" fn doorbell(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| {
        let label = b"sandom-gamestream doorbell";
        let name = string(env, label)?;
        let mut function = null_mut();
        checked(napi_create_threadsafe_function(env, arguments.values[0], null_mut(), name, 0, 1, null_mut(), null_mut(),
            null_mut(), Some(ring_to_javascript), &mut function), "creating the doorbell function")?;
        checked(napi_unref_threadsafe_function(env, function), "releasing the doorbell function's reference")?;
        external(env, function)
    }) }
}

unsafe extern "C" fn doorbell_ref(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| {
        checked(napi_ref_threadsafe_function(env, arguments.external(0)?), "referencing the doorbell function")?;
        undefined(env)
    }) }
}

unsafe extern "C" fn doorbell_unref(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| {
        checked(napi_unref_threadsafe_function(env, arguments.external(0)?), "releasing the doorbell function's reference")?;
        undefined(env)
    }) }
}

unsafe extern "C" fn doorbell_close(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| {
        checked(napi_release_threadsafe_function(arguments.external(0)?, NAPI_TSFN_RELEASE), "closing the doorbell function")?;
        undefined(env)
    }) }
}

/// `start(config, doorbell)`: `config` is the binary configuration record
/// (src/native.rs), `doorbell` the handle of `doorbell(...)`.
unsafe extern "C" fn start(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| {
        let (config, config_length) = arguments.bytes(0)?;
        let function = arguments.external(1)?;
        let mut message = vec![0u8; 4096];
        let mut length = 0;
        let server = library::sandom_gamestream_start(config, config_length, Some(ring), function, message.as_mut_ptr(),
            message.len(), &mut length);
        if server.is_null() {
            return Err(format!("The GameStream server did not start: {}", String::from_utf8_lossy(&message[..length.min(message.len())])));
        }
        external(env, server as *mut c_void)
    }) }
}

/// `takeEvent(server, buffer)`: the length of the record written into
/// `buffer`, 0 when none waits, or the record's length negated when it
/// does not fit.
unsafe extern "C" fn take_event(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| {
        let (buffer, capacity) = arguments.bytes(1)?;
        let length = library::sandom_gamestream_take_event(arguments.server()?, buffer, capacity);
        let mut result = null_mut();
        checked(napi_create_int64(env, length, &mut result), "creating a number")?;
        Ok(result)
    }) }
}

/// `sendAccessUnit(server, data, key, timestampMicroseconds)`: 0, or -1
/// with the reason in `lastError`.
unsafe extern "C" fn send_access_unit(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| {
        let (data, length) = arguments.bytes(1)?;
        int32(env, library::sandom_gamestream_send_access_unit(arguments.server()?, data, length, arguments.u32(2)?,
            arguments.i64(3)?))
    }) }
}

unsafe extern "C" fn end_session(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| int32(env, library::sandom_gamestream_end_session(arguments.server()?))) }
}

unsafe extern "C" fn stop(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| int32(env, library::sandom_gamestream_stop(arguments.server()?))) }
}

unsafe extern "C" fn free(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| int32(env, library::sandom_gamestream_free(arguments.server()?))) }
}

unsafe extern "C" fn last_error(env: napi_env, info: napi_callback_info) -> napi_value {
    unsafe { call(env, info, |arguments| {
        let server = arguments.server()?;
        let length = library::sandom_gamestream_last_error(server, null_mut(), 0);
        let mut message = vec![0u8; length];
        library::sandom_gamestream_last_error(server, message.as_mut_ptr(), length);
        string(env, &message)
    }) }
}

/// Node calls this when it loads the addon; `exports` receives each function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn napi_register_module_v1(env: napi_env, exports: napi_value) -> napi_value {
    let functions: [(&str, napi_callback); 11] = [
        ("doorbell\0", doorbell), ("doorbellRef\0", doorbell_ref), ("doorbellUnref\0", doorbell_unref),
        ("doorbellClose\0", doorbell_close), ("start\0", start), ("takeEvent\0", take_event),
        ("sendAccessUnit\0", send_access_unit), ("endSession\0", end_session), ("stop\0", stop), ("free\0", free),
        ("lastError\0", last_error),
    ];
    for (name, function) in functions {
        let mut value = null_mut();
        let created = unsafe { napi_create_function(env, name.as_ptr() as *const c_char, name.len() - 1, function, null_mut(),
            &mut value) };
        if created != NAPI_OK || unsafe { napi_set_named_property(env, exports, name.as_ptr() as *const c_char, value) } != NAPI_OK {
            unsafe { throw(env, &format!("the addon did not export {}", name.trim_end_matches('\0'))) };
            return null_mut();
        }
    }
    exports
}
