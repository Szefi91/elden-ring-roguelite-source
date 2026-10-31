//! Release-only save isolation and native crash evidence.
use std::{ffi::c_void, path::PathBuf, sync::{OnceLock, atomic::{AtomicBool, Ordering}}};
use std::os::windows::ffi::OsStrExt;
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();
static DUMP_PATH: OnceLock<Vec<u16>> = OnceLock::new();
static DUMPING: AtomicBool = AtomicBool::new(false);
#[repr(C)] struct ExceptionPointers { record: *mut c_void, context: *mut c_void }
#[repr(C)] struct DumpException { thread_id: u32, pointers: *mut ExceptionPointers, client_pointers: i32 }
#[link(name="kernel32")] extern "system" {
 fn GetCurrentProcess() -> *mut c_void;
 fn GetCurrentProcessId() -> u32;
 fn GetCurrentThreadId() -> u32;
 fn GetModuleHandleW(name: *const u16) -> *mut u8;
 fn VirtualProtect(address: *mut c_void, size: usize, protect: u32, old: *mut u32) -> i32;
 fn CreateFileW(name: *const u16, access: u32, share: u32, security: *mut c_void, creation: u32, flags: u32, template: *mut c_void) -> *mut c_void;
 fn CloseHandle(handle: *mut c_void) -> i32;
 fn AddVectoredExceptionHandler(first: u32, handler: unsafe extern "system" fn(*mut ExceptionPointers)->i32) -> *mut c_void;
}
#[link(name="dbghelp")] extern "system" {
 fn MiniDumpWriteDump(process: *mut c_void, pid: u32, file: *mut c_void, kind: u32, exception: *mut DumpException, streams: *mut c_void, callback: *mut c_void) -> i32;
}
pub fn directory() -> &'static PathBuf { LOG_DIR.get_or_init(|| {
 let dir=std::env::var_os("ER_ROGUELITE_LOG_DIR").map(PathBuf::from).unwrap_or_else(||std::env::temp_dir().join("EldenRingRoguelite"));
 let _=std::fs::create_dir_all(&dir);dir
}) }
pub fn log(message: &str) {
 use std::io::Write;
 if let Ok(mut file)=std::fs::OpenOptions::new().create(true).append(true).open(directory().join("roguelite.log")) {
  let timestamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_secs()).unwrap_or(0);
  let _=writeln!(file,"[{timestamp}] {message}");let _=file.flush();
 }
}
unsafe extern "system" fn exception_handler(pointers: *mut ExceptionPointers)->i32 {
 if pointers.is_null()||(*pointers).record.is_null(){return 0;}
 let code=std::ptr::read_unaligned((*pointers).record.cast::<u32>());
 if !matches!(code,0xc0000005|0xc000001d|0xc00000fd) || DUMPING.swap(true,Ordering::SeqCst) {return 0;}
 if let Some(path)=DUMP_PATH.get() {
  let file=CreateFileW(path.as_ptr(),0x40000000,1,std::ptr::null_mut(),2,0x80,std::ptr::null_mut());
  if file as isize != -1 {
   let mut info=DumpException{thread_id:GetCurrentThreadId(),pointers,client_pointers:0};
   let result=MiniDumpWriteDump(GetCurrentProcess(),GetCurrentProcessId(),file,0x1000,&mut info,std::ptr::null_mut(),std::ptr::null_mut());
   CloseHandle(file);
   let address=std::ptr::read_unaligned((*pointers).record.cast::<u8>().add(16).cast::<usize>());
   let _=std::fs::write(directory().join("crash-summary.txt"),format!("exception=0x{code:08x}\naddress=0x{address:x}\nminidump_written={}\n",result!=0));
  }
 }
 0 // Observe only; never swallow an access violation or continue corrupted gameplay.
}
/// Replace the game's sole UTF-16 save extension in its read-only data section.
/// Fail closed unless the supported executable has exactly one expected literal.
unsafe fn isolate_saves()->bool {
 let base=GetModuleHandleW(std::ptr::null());if base.is_null(){return false;}
 let pe=std::ptr::read_unaligned(base.add(0x3c).cast::<u32>()) as usize;
 if pe>0x100000||std::ptr::read_unaligned(base.add(pe).cast::<u32>())!=0x4550{return false;}
 let count=std::ptr::read_unaligned(base.add(pe+6).cast::<u16>()) as usize;
 let optional_size=std::ptr::read_unaligned(base.add(pe+20).cast::<u16>()) as usize;
 let image_size=std::ptr::read_unaligned(base.add(pe+24+56).cast::<u32>()) as usize;
 let first=pe+24+optional_size;let pattern=[46u8,0,115,0,108,0,50,0,0,0];let replacement=[46u8,0,114,0,103,0,108,0,0,0];
 let mut hits=Vec::new();
 for index in 0..count.min(96) {
  let section=base.add(first+index*40);
  if std::slice::from_raw_parts(section,8).starts_with(b".rdata") {
   let size=std::ptr::read_unaligned(section.add(8).cast::<u32>()) as usize;
   let rva=std::ptr::read_unaligned(section.add(12).cast::<u32>()) as usize;
   if rva.checked_add(size).map_or(true,|end|end>image_size){return false;}
   let data=std::slice::from_raw_parts(base.add(rva),size);
   for (offset,bytes) in data.windows(pattern.len()).enumerate(){if bytes==pattern{hits.push(rva+offset);}}
  }
 }
 if hits.len()!=1 {log(&format!("Save isolation refused: expected one .sl2 literal, found {}",hits.len()));return false;}
 let target=base.add(hits[0]);let mut old=0;
 if VirtualProtect(target.cast(),replacement.len(),0x04,&mut old)==0{return false;}
 std::ptr::copy_nonoverlapping(replacement.as_ptr(),target,replacement.len());
 let mut ignored=0;VirtualProtect(target.cast(),replacement.len(),old,&mut ignored);
 log(&format!("Save isolation active: .sl2 -> .rgl at game RVA 0x{:x}; original saves untouched",hits[0]));true
}
pub fn initialize()->bool {
 let path=directory().join("roguelite-crash.dmp");let wide=path.as_os_str().encode_wide().chain(Some(0)).collect();let _=DUMP_PATH.set(wide);
 unsafe{AddVectoredExceptionHandler(1,exception_handler);}
 std::panic::set_hook(Box::new(|info|log(&format!("Rust panic: {info}"))));
 log("Release 0.1.10-responsive-card-text: starting; native crash handler installed");
 unsafe{isolate_saves()}
}
