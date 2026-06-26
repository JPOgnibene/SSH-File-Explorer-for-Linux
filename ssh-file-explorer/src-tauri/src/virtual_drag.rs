use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use tauri::{AppHandle, Emitter};
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::Mutex;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{IDataObjectAsyncCapability, IDataObjectAsyncCapability_Impl};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};

use serde::Serialize;

use crate::{SshState, TransferProgress};

type SshSession = Arc<Mutex<SshState>>;

#[derive(Clone, Serialize)]
struct TransferComplete {
    id: String,
}

pub static DRAG_PROGRESS_BYTES: AtomicU64 = AtomicU64::new(0);
pub static DRAG_PROGRESS_TOTAL: AtomicU64 = AtomicU64::new(0);
static PROGRESS_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

pub fn start_progress_server() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind progress server");
    let port = listener.local_addr().unwrap().port();
    PROGRESS_PORT.store(port, Ordering::Relaxed);

    std::thread::spawn(move || {
        // Keep the MTA alive for the process lifetime so drag-out IStream
        // stubs dispatch Read calls to COM thread pool threads instead of
        // the main STA thread.
        unsafe { CoInitializeEx(Some(std::ptr::null()), COINIT_MULTITHREADED).ok(); }

        let mut buf = [0u8; 1024];
        let cors = "\
            Access-Control-Allow-Origin: *\r\n\
            Access-Control-Allow-Methods: GET, OPTIONS\r\n\
            Access-Control-Allow-Headers: *\r\n\
            Access-Control-Allow-Private-Network: true\r\n\
            Connection: close\r\n";

        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
            let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(200)));
            let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);

            let is_options = n >= 7 && &buf[..7] == b"OPTIONS";
            let resp = if is_options {
                format!("HTTP/1.1 204 No Content\r\n{}\r\n", cors)
            } else {
                let bytes = DRAG_PROGRESS_BYTES.load(Ordering::Relaxed);
                let total = DRAG_PROGRESS_TOTAL.load(Ordering::Relaxed);
                let body = format!(r#"{{"bytes":{},"total":{}}}"#, bytes, total);
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n{}",
                    cors, body.len(), body
                )
            };
            let _ = std::io::Write::write_all(&mut stream, resp.as_bytes());
        }
    });
}

pub fn progress_port() -> u16 {
    PROGRESS_PORT.load(Ordering::Relaxed)
}

const FD_FILESIZE: u32 = 0x40;
const FD_ATTRIBUTES: u32 = 0x04;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const STGTY_STREAM: u32 = 2;

extern "system" {
    fn CoMarshalInterThreadInterfaceInStream(
        riid: *const GUID,
        punk: *mut c_void,
        ppstm: *mut *mut c_void,
    ) -> HRESULT;
    fn CoGetInterfaceAndReleaseStream(
        pstm: *mut c_void,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT;
}

pub struct VirtualFileEntry {
    pub relative_path: String,
    pub remote_path: String,
    pub is_dir: bool,
    pub file_size: u64,
}

#[repr(C)]
struct FileDescriptorW {
    dw_flags: u32,
    clsid: GUID,
    sizel: [i32; 2],
    pointl: [i32; 2],
    dw_file_attributes: u32,
    ft_creation_time: FILETIME,
    ft_last_access_time: FILETIME,
    ft_last_write_time: FILETIME,
    n_file_size_high: u32,
    n_file_size_low: u32,
    c_file_name: [u16; 260],
}

// --- IDropSource ---
// Runs on the main thread via run_on_main_thread, so grfkeystate is reliable.
// We still use GetAsyncKeyState as a safety measure since it works on any thread.

#[implement(IDropSource)]
struct SimpleDropSource {
    hwnd: HWND,
    was_outside: Arc<AtomicBool>,
}

#[allow(non_snake_case)]
impl IDropSource_Impl for SimpleDropSource {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, _grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }

        extern "system" {
            fn GetAsyncKeyState(vKey: i32) -> i16;
        }

        const VK_LBUTTON: i32 = 0x01;
        let lbutton_down = unsafe { GetAsyncKeyState(VK_LBUTTON) } < 0;

        if !lbutton_down {
            return DRAGDROP_S_DROP;
        }

        let mut cursor_pos = POINT::default();
        let mut window_rect = RECT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor_pos);
            let _ = GetWindowRect(self.hwnd, &mut window_rect);
        }
        let inside = cursor_pos.x >= window_rect.left && cursor_pos.x <= window_rect.right
            && cursor_pos.y >= window_rect.top && cursor_pos.y <= window_rect.bottom;

        if !inside {
            self.was_outside.store(true, Ordering::Relaxed);
        } else if self.was_outside.load(Ordering::Relaxed) {
            return DRAGDROP_S_CANCEL;
        }

        S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

// --- IStream backed by a background SFTP download ---

struct StreamBuffer {
    data: VecDeque<u8>,
    done: bool,
    error: Option<String>,
}

struct DownloadParams {
    rt_handle: TokioHandle,
    ssh_state: SshSession,
    remote_path: String,
    app_handle: AppHandle,
    transfer_id: String,
    display_name: String,
    total_transfer_size: u64,
    shared_bytes: Arc<AtomicU64>,
}

#[implement(IStream)]
struct SftpStream {
    file_size: u64,
    buf: Arc<(StdMutex<StreamBuffer>, Condvar)>,
    position: StdMutex<u64>,
    download_started: std::sync::atomic::AtomicBool,
    params: StdMutex<Option<DownloadParams>>,
}

impl SftpStream {
    fn new(
        rt_handle: TokioHandle,
        ssh_state: SshSession,
        remote_path: String,
        file_size: u64,
        app_handle: AppHandle,
        transfer_id: String,
        display_name: String,
        total_transfer_size: u64,
        shared_bytes: Arc<AtomicU64>,
    ) -> Self {
        Self {
            file_size,
            buf: Arc::new((
                StdMutex::new(StreamBuffer {
                    data: VecDeque::new(),
                    done: false,
                    error: None,
                }),
                Condvar::new(),
            )),
            position: StdMutex::new(0),
            download_started: std::sync::atomic::AtomicBool::new(false),
            params: StdMutex::new(Some(DownloadParams {
                rt_handle, ssh_state, remote_path, app_handle,
                transfer_id, display_name, total_transfer_size, shared_bytes,
            })),
        }
    }

    fn ensure_download_started(&self) {
        if self.download_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(params) = self.params.lock().unwrap().take() else { return };
        let buf_clone = self.buf.clone();

        std::thread::spawn(move || {
            let app_for_complete = params.app_handle.clone();
            let id_for_complete = params.transfer_id.clone();
            let result = params.rt_handle.block_on(async {
                let sftp = {
                    let s = params.ssh_state.lock().await;
                    let session = s.session.as_ref().ok_or("Not connected")?;
                    let channel = session
                        .channel_open_session()
                        .await
                        .map_err(|e| format!("Channel error: {}", e))?;
                    channel
                        .request_subsystem(true, "sftp")
                        .await
                        .map_err(|e| format!("SFTP error: {}", e))?;
                    russh_sftp::client::SftpSession::new(channel.into_stream())
                        .await
                        .map_err(|e| format!("SFTP init error: {}", e))?
                };

                let mut file = sftp
                    .open(&params.remote_path)
                    .await
                    .map_err(|e| format!("Open error: {}", e))?;

                use tokio::io::AsyncReadExt;
                let mut chunk = vec![0u8; 65536];
                loop {
                    if crate::is_transfer_cancelled(&params.transfer_id) {
                        crate::CANCELLED_TRANSFERS.lock().unwrap().remove(&params.transfer_id);
                        return Err("Transfer cancelled".to_string());
                    }

                    let n = file
                        .read(&mut chunk)
                        .await
                        .map_err(|e| format!("Read error: {}", e))?;
                    if n == 0 {
                        break;
                    }

                    let total = params.shared_bytes.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
                    DRAG_PROGRESS_BYTES.store(total, Ordering::Relaxed);
                    let _ = params.app_handle.emit("transfer-progress", TransferProgress {
                        id: params.transfer_id.clone(),
                        transfer_type: "download".to_string(),
                        file_name: params.display_name.clone(),
                        bytes_transferred: total,
                        total_bytes: params.total_transfer_size,
                    });

                    let (lock, cvar) = &*buf_clone;
                    let mut state = lock.lock().unwrap();
                    state.data.extend(&chunk[..n]);
                    cvar.notify_all();

                    while state.data.len() > 4 * 1024 * 1024 && !state.done {
                        state = cvar.wait(state).unwrap();
                    }
                }
                Ok::<_, String>(())
            });

            let (lock, cvar) = &*buf_clone;
            let mut state = lock.lock().unwrap();
            state.done = true;
            if let Err(e) = result {
                state.error = Some(e);
            }
            cvar.notify_all();

            // Signal the frontend that the drag transfer is complete
            DRAG_PROGRESS_BYTES.store(0, Ordering::Relaxed);
            DRAG_PROGRESS_TOTAL.store(u64::MAX, Ordering::Relaxed);

            let _ = app_for_complete.emit("transfer-complete", TransferComplete {
                id: id_for_complete,
            });
        });
    }
}

#[allow(non_snake_case)]
impl ISequentialStream_Impl for SftpStream {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        self.ensure_download_started();

        let (lock, cvar) = &*self.buf;
        let mut state = lock.lock().unwrap();

        while state.data.is_empty() && !state.done {
            state = cvar.wait(state).unwrap();
        }

        if !state.data.is_empty() {
            let available = state.data.len().min(cb as usize);
            let dst = unsafe { std::slice::from_raw_parts_mut(pv as *mut u8, available) };
            for (i, byte) in state.data.drain(..available).enumerate() {
                dst[i] = byte;
            }
            cvar.notify_all();

            let mut pos = self.position.lock().unwrap();
            *pos += available as u64;

            if !pcbread.is_null() {
                unsafe { *pcbread = available as u32 };
            }
            return S_OK;
        }

        if !pcbread.is_null() {
            unsafe { *pcbread = 0 };
        }

        if state.error.is_some() {
            E_FAIL
        } else {
            S_FALSE
        }
    }

    fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        E_NOTIMPL
    }
}

#[allow(non_snake_case)]
impl IStream_Impl for SftpStream {
    fn Seek(
        &self,
        _dlibmove: i64,
        _dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> windows::core::Result<()> {
        let pos = self.position.lock().unwrap();
        if !plibnewposition.is_null() {
            unsafe { *plibnewposition = *pos };
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> windows::core::Result<()> {
        Err(Error::new(E_NOTIMPL, HSTRING::new()))
    }

    fn CopyTo(
        &self,
        _pstm: Option<&IStream>,
        _cb: u64,
        _pcbread: *mut u64,
        _pcbwritten: *mut u64,
    ) -> windows::core::Result<()> {
        Err(Error::new(E_NOTIMPL, HSTRING::new()))
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Err(Error::new(E_NOTIMPL, HSTRING::new()))
    }

    fn LockRegion(
        &self,
        _liboffset: u64,
        _cb: u64,
        _dwlocktype: &LOCKTYPE,
    ) -> windows::core::Result<()> {
        Err(Error::new(E_NOTIMPL, HSTRING::new()))
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> windows::core::Result<()> {
        Err(Error::new(E_NOTIMPL, HSTRING::new()))
    }

    fn Stat(
        &self,
        pstatstg: *mut STATSTG,
        _grfstatflag: &STATFLAG,
    ) -> windows::core::Result<()> {
        if pstatstg.is_null() {
            return Err(Error::new(E_INVALIDARG, HSTRING::new()));
        }
        unsafe {
            let stat = &mut *pstatstg;
            *stat = std::mem::zeroed();
            stat.r#type = STGTY_STREAM;
            stat.cbSize = self.file_size;
        }
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Err(Error::new(E_NOTIMPL, HSTRING::new()))
    }
}

// --- IDataObject with multi-file virtual descriptors ---

#[implement(IDataObject, IDataObjectAsyncCapability)]
struct VirtualFileDataObject {
    entries: Vec<VirtualFileEntry>,
    total_size: u64,
    display_name: String,
    cf_descriptor: u16,
    cf_contents: u16,
    rt_handle: TokioHandle,
    ssh_state: SshSession,
    app_handle: AppHandle,
    transfer_id: String,
    shared_bytes: Arc<AtomicU64>,
    async_mode: AtomicBool,
    in_operation: AtomicBool,
}

impl VirtualFileDataObject {
    fn build_file_descriptor(&self) -> windows::core::Result<STGMEDIUM> {
        let count = self.entries.len();
        let desc_size = std::mem::size_of::<FileDescriptorW>();
        let total_size = 4 + desc_size * count;

        unsafe {
            let hglobal = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total_size)
                .map_err(|_| Error::new(E_OUTOFMEMORY, HSTRING::new()))?;
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                return Err(Error::new(E_OUTOFMEMORY, HSTRING::new()));
            }

            *(ptr as *mut u32) = count as u32;

            for (i, entry) in self.entries.iter().enumerate() {
                let win_path = entry.relative_path.replace('/', "\\");
                let mut c_file_name = [0u16; 260];
                let name_wide: Vec<u16> = win_path.encode_utf16().collect();
                let len = name_wide.len().min(259);
                c_file_name[..len].copy_from_slice(&name_wide[..len]);

                let descriptor = FileDescriptorW {
                    dw_flags: FD_FILESIZE | FD_ATTRIBUTES,
                    clsid: GUID::zeroed(),
                    sizel: [0, 0],
                    pointl: [0, 0],
                    dw_file_attributes: if entry.is_dir {
                        FILE_ATTRIBUTE_DIRECTORY
                    } else {
                        FILE_ATTRIBUTE_NORMAL
                    },
                    ft_creation_time: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
                    ft_last_access_time: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
                    ft_last_write_time: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
                    n_file_size_high: (entry.file_size >> 32) as u32,
                    n_file_size_low: entry.file_size as u32,
                    c_file_name,
                };

                std::ptr::copy_nonoverlapping(
                    &descriptor as *const FileDescriptorW as *const u8,
                    (ptr as *mut u8).add(4 + desc_size * i),
                    desc_size,
                );
            }

            let _ = GlobalUnlock(hglobal);

            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: hglobal },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            })
        }
    }

    fn build_file_contents_stream(&self, index: usize) -> windows::core::Result<STGMEDIUM> {
        let entry = &self.entries[index];

        let rt_handle = self.rt_handle.clone();
        let ssh_state = self.ssh_state.clone();
        let remote_path = entry.remote_path.clone();
        let file_size = entry.file_size;
        let app_handle = self.app_handle.clone();
        let transfer_id = self.transfer_id.clone();
        let display_name = self.display_name.clone();
        let total_size = self.total_size;
        let shared_bytes = self.shared_bytes.clone();

        let (tx, rx) = std::sync::mpsc::channel::<std::result::Result<usize, String>>();

        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(Some(std::ptr::null()), COINIT_MULTITHREADED);
            }

            let stream = SftpStream::new(
                rt_handle, ssh_state, remote_path, file_size,
                app_handle, transfer_id, display_name,
                total_size, shared_bytes,
            );
            let istream: IStream = stream.into();

            let mut marshal_stm: *mut c_void = std::ptr::null_mut();
            let hr = unsafe {
                CoMarshalInterThreadInterfaceInStream(
                    &IStream::IID,
                    istream.as_raw(),
                    &mut marshal_stm,
                )
            };

            if hr.is_err() {
                tx.send(Err(format!("Marshal failed: 0x{:08x}", hr.0))).ok();
                drop(istream);
                unsafe { CoUninitialize(); }
                return;
            }

            tx.send(Ok(marshal_stm as usize)).ok();

            // MTA: no message pump needed. COM dispatches Read calls to
            // thread pool threads directly. Drop our reference and exit;
            // the MTA stays alive via the progress server thread.
            drop(istream);
            unsafe { CoUninitialize(); }
        });

        // Wait for the marshaled stream pointer from the worker
        let marshal_raw = rx.recv()
            .map_err(|_| Error::new(E_FAIL, HSTRING::new()))?
            .map_err(|e| Error::new(E_FAIL, HSTRING::from(e.as_str())))?;

        // Unmarshal to get the proxy IStream on the main thread.
        // CoGetInterfaceAndReleaseStream releases the marshal stream for us.
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            CoGetInterfaceAndReleaseStream(
                marshal_raw as *mut c_void,
                &IStream::IID,
                &mut ppv,
            )
        };
        if hr.is_err() {
            return Err(Error::new(hr, HSTRING::from("Unmarshal failed")));
        }

        let proxy = unsafe { IStream::from_raw(ppv) };

        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: std::mem::ManuallyDrop::new(Some(proxy)),
            },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for VirtualFileDataObject {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let fmt = unsafe { &*pformatetcin };
        if fmt.cfFormat == self.cf_descriptor {
            self.build_file_descriptor()
        } else if fmt.cfFormat == self.cf_contents {
            let idx = fmt.lindex;
            if idx < 0 || idx as usize >= self.entries.len() {
                return Err(Error::new(DV_E_FORMATETC, HSTRING::new()));
            }
            let entry = &self.entries[idx as usize];
            if entry.is_dir {
                return Err(Error::new(DV_E_FORMATETC, HSTRING::new()));
            }
            self.build_file_contents_stream(idx as usize)
        } else {
            Err(Error::new(DV_E_FORMATETC, HSTRING::new()))
        }
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(Error::new(E_NOTIMPL, HSTRING::new()))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        let fmt = unsafe { &*pformatetc };
        if fmt.cfFormat == self.cf_descriptor {
            S_OK
        } else if fmt.cfFormat == self.cf_contents {
            if fmt.lindex == -1 {
                return S_OK;
            }
            let idx = fmt.lindex as usize;
            if idx < self.entries.len() && !self.entries[idx].is_dir {
                S_OK
            } else {
                DV_E_FORMATETC
            }
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        unsafe { (*pformatetcout).ptd = std::ptr::null_mut() };
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: BOOL,
    ) -> windows::core::Result<()> {
        Err(Error::new(E_NOTIMPL, HSTRING::new()))
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(Error::new(E_NOTIMPL, HSTRING::new()));
        }

        let formats = vec![
            FORMATETC {
                cfFormat: self.cf_descriptor,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0 as u32,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            },
            FORMATETC {
                cfFormat: self.cf_contents,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0 as u32,
                lindex: -1,
                tymed: TYMED_ISTREAM.0 as u32,
            },
        ];

        Ok(FormatEnumerator::new(formats).into())
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Option<&IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(Error::new(OLE_E_ADVISENOTSUPPORTED, HSTRING::new()))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(Error::new(OLE_E_ADVISENOTSUPPORTED, HSTRING::new()))
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(Error::new(OLE_E_ADVISENOTSUPPORTED, HSTRING::new()))
    }
}

// --- IDataObjectAsyncCapability ---
// Tells Explorer to extract file data asynchronously so DoDragDrop returns
// immediately after the drop, freeing the main thread.

#[allow(non_snake_case)]
impl IDataObjectAsyncCapability_Impl for VirtualFileDataObject {
    fn SetAsyncMode(&self, fdoopasync: BOOL) -> windows::core::Result<()> {
        self.async_mode.store(fdoopasync.as_bool(), Ordering::SeqCst);
        Ok(())
    }

    fn GetAsyncMode(&self) -> windows::core::Result<BOOL> {
        Ok(BOOL::from(self.async_mode.load(Ordering::SeqCst)))
    }

    fn StartOperation(&self, _pbcreserved: Option<&IBindCtx>) -> windows::core::Result<()> {
        self.in_operation.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn InOperation(&self) -> windows::core::Result<BOOL> {
        Ok(BOOL::from(self.in_operation.load(Ordering::SeqCst)))
    }

    fn EndOperation(
        &self,
        _hresult: HRESULT,
        _pbcreserved: Option<&IBindCtx>,
        _dweffects: u32,
    ) -> windows::core::Result<()> {
        self.in_operation.store(false, Ordering::SeqCst);
        Ok(())
    }
}

// --- IEnumFORMATETC ---

#[implement(IEnumFORMATETC)]
struct FormatEnumerator {
    formats: Vec<FORMATETC>,
    index: AtomicUsize,
}

impl FormatEnumerator {
    fn new(formats: Vec<FORMATETC>) -> Self {
        Self {
            formats,
            index: AtomicUsize::new(0),
        }
    }
}

#[allow(non_snake_case)]
impl IEnumFORMATETC_Impl for FormatEnumerator {
    fn Next(
        &self,
        celt: u32,
        rgelt: *mut FORMATETC,
        pceltfetched: *mut u32,
    ) -> windows::core::Result<()> {
        let current = self.index.load(Ordering::SeqCst);
        let mut fetched = 0u32;

        for i in 0..celt as usize {
            let idx = current + i;
            if idx >= self.formats.len() {
                break;
            }
            unsafe { *rgelt.add(i) = self.formats[idx] };
            fetched += 1;
        }

        self.index.store(current + fetched as usize, Ordering::SeqCst);

        if !pceltfetched.is_null() {
            unsafe { *pceltfetched = fetched };
        }

        if fetched == celt {
            Ok(())
        } else {
            Err(Error::new(S_FALSE, HSTRING::new()))
        }
    }

    fn Skip(&self, celt: u32) -> windows::core::Result<()> {
        let current = self.index.load(Ordering::SeqCst);
        self.index.store(
            (current + celt as usize).min(self.formats.len()),
            Ordering::SeqCst,
        );
        Ok(())
    }

    fn Reset(&self) -> windows::core::Result<()> {
        self.index.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IEnumFORMATETC> {
        let cloned = FormatEnumerator {
            formats: self.formats.clone(),
            index: AtomicUsize::new(self.index.load(Ordering::SeqCst)),
        };
        Ok(cloned.into())
    }
}

// --- Entry point ---

pub fn init_ole_main_thread() {
    unsafe {
        let _ = OleInitialize(Some(std::ptr::null_mut()));
    }
}

pub fn start_drag(
    rt_handle: TokioHandle,
    ssh_state: SshSession,
    entries: Vec<VirtualFileEntry>,
    total_size: u64,
    display_name: String,
    app_handle: AppHandle,
    transfer_id: String,
    hwnd: HWND,
) -> std::result::Result<String, String> {
    let cf_descriptor =
        unsafe { RegisterClipboardFormatW(w!("FileGroupDescriptorW")) as u16 };
    let cf_contents =
        unsafe { RegisterClipboardFormatW(w!("FileContents")) as u16 };

    let shared_bytes = Arc::new(AtomicU64::new(0));

    DRAG_PROGRESS_BYTES.store(0, Ordering::Relaxed);
    DRAG_PROGRESS_TOTAL.store(total_size, Ordering::Relaxed);

    let data_object: IDataObject = VirtualFileDataObject {
        entries,
        total_size,
        display_name,
        cf_descriptor,
        cf_contents,
        rt_handle,
        ssh_state,
        app_handle,
        transfer_id,
        shared_bytes: shared_bytes.clone(),
        async_mode: AtomicBool::new(true),
        in_operation: AtomicBool::new(false),
    }
    .into();

    let was_outside = Arc::new(AtomicBool::new(false));
    let drop_source: IDropSource = SimpleDropSource {
        hwnd,
        was_outside: was_outside.clone(),
    }.into();

    let (effect, hr);
    unsafe {
        let mut eff = DROPEFFECT::default();
        hr = DoDragDrop(&data_object, &drop_source, DROPEFFECT_COPY, &mut eff);
        effect = eff;
    }

    if hr == DRAGDROP_S_CANCEL && was_outside.load(Ordering::Relaxed) {
        DRAG_PROGRESS_BYTES.store(0, Ordering::Relaxed);
        DRAG_PROGRESS_TOTAL.store(u64::MAX, Ordering::Relaxed);
        return Ok("reentry".to_string());
    }

    if effect == DROPEFFECT(0) {
        DRAG_PROGRESS_BYTES.store(0, Ordering::Relaxed);
        DRAG_PROGRESS_TOTAL.store(u64::MAX, Ordering::Relaxed);
    }

    Ok("done".to_string())
}
