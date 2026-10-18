use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    static _NSConcreteGlobalBlock: c_void;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "UniformTypeIdentifiers", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
}

fn class(name: &str) -> Id {
    let name = CString::new(name).unwrap();
    unsafe { objc_getClass(name.as_ptr()) }
}

fn sel(name: &str) -> Sel {
    let name = CString::new(name).unwrap();
    unsafe { sel_registerName(name.as_ptr()) }
}

fn send(receiver: Id, selector: &str) -> Id {
    let send: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(receiver, sel(selector)) }
}

fn send_with(receiver: Id, selector: &str, arg: Id) -> Id {
    let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(receiver, sel(selector), arg) }
}

fn ns_string(value: &str) -> Id {
    let value = CString::new(value).unwrap();
    let send: unsafe extern "C" fn(Id, Sel, *const c_char) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        send(
            class("NSString"),
            sel("stringWithUTF8String:"),
            value.as_ptr(),
        )
    }
}

fn rust_string(value: Id) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let utf8 = send(value, "UTF8String") as *const c_char;
    (!utf8.is_null()).then(|| {
        unsafe { CStr::from_ptr(utf8) }
            .to_string_lossy()
            .into_owned()
    })
}

fn workspace() -> Id {
    send(class("NSWorkspace"), "sharedWorkspace")
}

fn content_type(identifier: &str) -> Id {
    send_with(
        class("UTType"),
        "typeWithIdentifier:",
        ns_string(identifier),
    )
}

pub fn type_for_extension(extension: &str) -> Option<String> {
    let found = send_with(
        class("UTType"),
        "typeWithFilenameExtension:",
        ns_string(extension),
    );
    if found.is_null() {
        return None;
    }
    rust_string(send(found, "identifier"))
}

pub fn handler(identifier: &str) -> Option<String> {
    let found = content_type(identifier);
    if found.is_null() {
        return None;
    }
    let url = send_with(workspace(), "URLForApplicationToOpenContentType:", found);
    if url.is_null() {
        return None;
    }
    let bundle = send_with(class("NSBundle"), "bundleWithURL:", url);
    if bundle.is_null() {
        return None;
    }
    rust_string(send(bundle, "bundleIdentifier"))
}

fn app_url(bundle_id: &str) -> Id {
    send_with(
        workspace(),
        "URLForApplicationWithBundleIdentifier:",
        ns_string(bundle_id),
    )
}

pub fn app_path(bundle_id: &str) -> Option<PathBuf> {
    let url = app_url(bundle_id);
    if url.is_null() {
        return None;
    }
    rust_string(send(url, "path")).map(PathBuf::from)
}

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

#[repr(C)]
struct Block {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*const Block, Id),
    descriptor: *const BlockDescriptor,
}

unsafe impl Sync for Block {}

const BLOCK_IS_GLOBAL: i32 = 1 << 28;

static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: size_of::<Block>(),
};

static FINISHED: AtomicBool = AtomicBool::new(false);
static FAILURE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

unsafe extern "C" fn finished(_block: *const Block, error: Id) {
    let error = if error.is_null() {
        error
    } else {
        send(error, "retain")
    };
    FAILURE.store(error, Ordering::SeqCst);
    FINISHED.store(true, Ordering::SeqCst);
}

pub fn set_handler(identifier: &str, bundle_id: &str) -> Result<()> {
    let app = app_url(bundle_id);
    if app.is_null() {
        bail!("{bundle_id} is not installed");
    }
    let found = content_type(identifier);
    if found.is_null() {
        bail!("macOS does not know {identifier}");
    }
    let block: &'static Block = Box::leak(Box::new(Block {
        isa: &raw const _NSConcreteGlobalBlock,
        flags: BLOCK_IS_GLOBAL,
        reserved: 0,
        invoke: finished,
        descriptor: &DESCRIPTOR,
    }));
    FINISHED.store(false, Ordering::SeqCst);
    FAILURE.store(ptr::null_mut(), Ordering::SeqCst);
    let set: unsafe extern "C" fn(Id, Sel, Id, Id, *const Block) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        set(
            workspace(),
            sel("setDefaultApplicationAtURL:toOpenContentType:completionHandler:"),
            app,
            found,
            block,
        )
    };
    let deadline = Instant::now() + Duration::from_secs(600);
    while !FINISHED.load(Ordering::SeqCst) {
        if Instant::now() > deadline {
            bail!("no answer to the dialog for {identifier}");
        }
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 1) };
    }
    let error = FAILURE.load(Ordering::SeqCst);
    if !error.is_null() {
        let reason = rust_string(send(error, "localizedDescription"));
        return Err(anyhow::anyhow!(reason.unwrap_or_default()))
            .context(format!("macOS kept the old handler for {identifier}"));
    }
    Ok(())
}
