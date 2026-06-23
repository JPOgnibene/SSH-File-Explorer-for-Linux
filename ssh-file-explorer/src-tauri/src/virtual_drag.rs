use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::AtomicUsize;
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
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};

use crate::{SshState, TransferProgress};

type SshSession = Arc<Mutex<SshState>>;

const FD_FILESIZE: u32 = 0x40;
const STGTY_STREAM: u32 = 2;

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

#[implement(IDropSource)]
struct SimpleDropSource;

#[allow(non_snake_case)]
impl IDropSource_Impl for SimpleDropSource {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if (grfkeystate & MK_LBUTTON) == MODIFIERKEYS_FLAGS(0) {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
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

#[implement(IStream)]
struct SftpStream {
    file_size: u64,
    buf: Arc<(StdMutex<StreamBuffer>, Condvar)>,
    position: StdMutex<u64>,
}

impl SftpStream {
    fn new(
        rt_handle: TokioHandle,
        ssh_state: SshSession,
        remote_path: String,
        file_size: u64,
        app_handle: AppHandle,
        transfer_id: String,
        file_name: String,
    ) -> Self {
        let buf = Arc::new((
            StdMutex::new(StreamBuffer {
                data: VecDeque::new(),
                done: false,
                error: None,
            }),
            Condvar::new(),
        ));

        let buf_clone = buf.clone();
        std::thread::spawn(move || {
            let result = rt_handle.block_on(async {
                let sftp = {
                    let s = ssh_state.lock().await;
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
                    .open(&remote_path)
                    .await
                    .map_err(|e| format!("Open error: {}", e))?;

                use tokio::io::AsyncReadExt;
                let mut chunk = vec![0u8; 65536];
                let mut bytes_transferred = 0u64;
                loop {
                    let n = file
                        .read(&mut chunk)
                        .await
                        .map_err(|e| format!("Read error: {}", e))?;
                    if n == 0 {
                        break;
                    }
                    bytes_transferred += n as u64;

                    let _ = app_handle.emit("transfer-progress", TransferProgress {
                        id: transfer_id.clone(),
                        transfer_type: "download".to_string(),
                        file_name: file_name.clone(),
                        bytes_transferred,
                        total_bytes: file_size,
                    });

                    let (lock, cvar) = &*buf_clone;
                    let mut state = lock.lock().unwrap();
                    state.data.extend(&chunk[..n]);
                    cvar.notify_all();

                    // Backpressure: wait if buffer exceeds 4MB
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
        });

        Self {
            file_size,
            buf,
            position: StdMutex::new(0),
        }
    }
}

#[allow(non_snake_case)]
impl ISequentialStream_Impl for SftpStream {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        let (lock, cvar) = &*self.buf;
        let mut state = lock.lock().unwrap();

        // Wait for data or completion
        while state.data.is_empty() && !state.done {
            state = cvar.wait(state).unwrap();
        }

        if let Some(ref e) = state.error {
            if state.data.is_empty() {
                let _ = e; // error consumed
                return E_FAIL;
            }
        }

        let available = state.data.len().min(cb as usize);
        if available > 0 {
            let dst = unsafe { std::slice::from_raw_parts_mut(pv as *mut u8, available) };
            for (i, byte) in state.data.drain(..available).enumerate() {
                dst[i] = byte;
            }
            // Signal backpressure relief
            cvar.notify_all();

            let mut pos = self.position.lock().unwrap();
            *pos += available as u64;
        }

        if !pcbread.is_null() {
            unsafe { *pcbread = available as u32 };
        }

        if available == 0 { S_FALSE } else { S_OK }
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

// --- IDataObject with virtual file descriptors ---

#[implement(IDataObject)]
struct VirtualFileDataObject {
    file_name: String,
    file_size: u64,
    remote_path: String,
    cf_descriptor: u16,
    cf_contents: u16,
    rt_handle: TokioHandle,
    ssh_state: SshSession,
    app_handle: AppHandle,
    transfer_id: String,
}

impl VirtualFileDataObject {
    fn build_file_descriptor(&self) -> windows::core::Result<STGMEDIUM> {
        let mut c_file_name = [0u16; 260];
        let name_wide: Vec<u16> = self.file_name.encode_utf16().collect();
        let len = name_wide.len().min(259);
        c_file_name[..len].copy_from_slice(&name_wide[..len]);

        let descriptor = FileDescriptorW {
            dw_flags: FD_FILESIZE,
            clsid: GUID::zeroed(),
            sizel: [0, 0],
            pointl: [0, 0],
            dw_file_attributes: 0,
            ft_creation_time: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            ft_last_access_time: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            ft_last_write_time: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            n_file_size_high: (self.file_size >> 32) as u32,
            n_file_size_low: self.file_size as u32,
            c_file_name,
        };

        let total_size = 4 + std::mem::size_of::<FileDescriptorW>();

        unsafe {
            let hglobal = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total_size)
                .map_err(|_| Error::new(E_OUTOFMEMORY, HSTRING::new()))?;
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                return Err(Error::new(E_OUTOFMEMORY, HSTRING::new()));
            }

            *(ptr as *mut u32) = 1;
            std::ptr::copy_nonoverlapping(
                &descriptor as *const FileDescriptorW as *const u8,
                (ptr as *mut u8).add(4),
                std::mem::size_of::<FileDescriptorW>(),
            );

            let _ = GlobalUnlock(hglobal);

            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: hglobal },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            })
        }
    }

    fn build_file_contents_stream(&self) -> windows::core::Result<STGMEDIUM> {
        let stream = SftpStream::new(
            self.rt_handle.clone(),
            self.ssh_state.clone(),
            self.remote_path.clone(),
            self.file_size,
            self.app_handle.clone(),
            self.transfer_id.clone(),
            self.file_name.clone(),
        );
        let istream: IStream = stream.into();

        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: std::mem::ManuallyDrop::new(Some(istream)),
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
        } else if fmt.cfFormat == self.cf_contents && fmt.lindex == 0 {
            self.build_file_contents_stream()
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
        if fmt.cfFormat == self.cf_descriptor
            || (fmt.cfFormat == self.cf_contents && fmt.lindex == 0)
        {
            S_OK
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
                lindex: 0,
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
        let current = self.index.load(std::sync::atomic::Ordering::SeqCst);
        let mut fetched = 0u32;

        for i in 0..celt as usize {
            let idx = current + i;
            if idx >= self.formats.len() {
                break;
            }
            unsafe { *rgelt.add(i) = self.formats[idx] };
            fetched += 1;
        }

        self.index
            .store(current + fetched as usize, std::sync::atomic::Ordering::SeqCst);

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
        let current = self.index.load(std::sync::atomic::Ordering::SeqCst);
        self.index.store(
            (current + celt as usize).min(self.formats.len()),
            std::sync::atomic::Ordering::SeqCst,
        );
        Ok(())
    }

    fn Reset(&self) -> windows::core::Result<()> {
        self.index.store(0, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IEnumFORMATETC> {
        let cloned = FormatEnumerator {
            formats: self.formats.clone(),
            index: AtomicUsize::new(self.index.load(std::sync::atomic::Ordering::SeqCst)),
        };
        Ok(cloned.into())
    }
}

// --- Entry point ---

pub fn start_drag(
    rt_handle: TokioHandle,
    ssh_state: SshSession,
    remote_path: String,
    file_name: String,
    file_size: u64,
    main_thread_id: u32,
    app_handle: AppHandle,
    transfer_id: String,
) -> std::result::Result<(), String> {
    extern "system" {
        fn GetCurrentThreadId() -> u32;
        fn AttachThreadInput(id_attach: u32, id_attach_to: u32, f_attach: i32) -> i32;
    }

    unsafe {
        let hr = OleInitialize(Some(std::ptr::null_mut()));
        if hr.is_err() {
            return Err(format!("OleInitialize failed: {:?}", hr));
        }

        let drag_thread_id = GetCurrentThreadId();
        AttachThreadInput(drag_thread_id, main_thread_id, 1);
    }

    let cf_descriptor =
        unsafe { RegisterClipboardFormatW(w!("FileGroupDescriptorW")) as u16 };
    let cf_contents =
        unsafe { RegisterClipboardFormatW(w!("FileContents")) as u16 };

    let data_object: IDataObject = VirtualFileDataObject {
        file_name,
        file_size,
        remote_path,
        cf_descriptor,
        cf_contents,
        rt_handle,
        ssh_state,
        app_handle,
        transfer_id,
    }
    .into();

    let drop_source: IDropSource = SimpleDropSource.into();

    unsafe {
        let mut effect = DROPEFFECT::default();
        let _ = DoDragDrop(&data_object, &drop_source, DROPEFFECT_COPY, &mut effect);

        let drag_thread_id = GetCurrentThreadId();
        AttachThreadInput(drag_thread_id, main_thread_id, 0);
    }

    Ok(())
}
