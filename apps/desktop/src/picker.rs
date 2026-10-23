//! The shell dialog runs on its own STA, never inside GPUI's foreground executor.
use crate::i18n::Language;
use anyhow::Result;
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
use windows::Win32::{Foundation::*, System::Com::*, UI::Shell::*};

pub(crate) struct Apartment;
impl Apartment {
    pub fn new() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()? };
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

pub fn choose(owner: usize, language: Language) -> Result<Option<Vec<PathBuf>>> {
    let _apartment = Apartment::new()?;
    let text = language.strings();
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            dialog.GetOptions()?
                | FOS_ALLOWMULTISELECT
                | FOS_FORCEFILESYSTEM
                | FOS_FILEMUSTEXIST
                | FOS_PATHMUSTEXIST
                | FOS_NOCHANGEDIR
                | FOS_DONTADDTORECENT,
        )?;
        dialog.SetTitle(&windows::core::HSTRING::from(text.picker_send_title))?;
        dialog.SetOkButtonLabel(&windows::core::HSTRING::from(text.choose_files))?;
        dialog.SetClientGuid(&windows::core::GUID::from_u128(
            0x8eae56d5_7919_466c_a218_433076dfad07,
        ))?;
        // Avoid starting at Home/Quick Access, which can enumerate disconnected
        // network locations. A previously used folder still takes precedence.
        if let Ok(folder) = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) {
            let item = SHCreateItemFromParsingName::<_, _, IShellItem>(folder, None);
            CoTaskMemFree(Some(folder.0.cast()));
            if let Ok(item) = item {
                let _ = dialog.SetDefaultFolder(&item);
            }
        }
        match dialog.Show(Some(HWND(owner as *mut _))) {
            Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) => {
                return Ok(None);
            }
            result => result?,
        }
        let items = dialog.GetResults()?;
        let mut paths = Vec::new();
        for index in 0..items.GetCount()? {
            let name = items.GetItemAt(index)?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = PathBuf::from(OsString::from_wide(name.as_wide()));
            CoTaskMemFree(Some(name.0.cast()));
            paths.push(path);
        }
        Ok(Some(paths))
    }
}

pub fn choose_directory(owner: usize, language: Language) -> Result<Option<PathBuf>> {
    let _apartment = Apartment::new()?;
    let text = language.strings();
    // SAFETY: this worker owns its STA and all COM interfaces stay on this thread,
    // dropping before Apartment uninitializes COM. Windows borrows the owner HWND
    // only for the modal dialog; no GPUI state crosses threads. Shell-allocated
    // strings remain valid until copied and are released with CoTaskMemFree.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            dialog.GetOptions()?
                | FOS_PICKFOLDERS
                | FOS_FORCEFILESYSTEM
                | FOS_PATHMUSTEXIST
                | FOS_NOCHANGEDIR
                | FOS_DONTADDTORECENT,
        )?;
        dialog.SetTitle(&windows::core::HSTRING::from(text.picker_directory_title))?;
        dialog.SetOkButtonLabel(&windows::core::HSTRING::from(text.picker_directory_ok))?;
        dialog.SetClientGuid(&windows::core::GUID::from_u128(
            0x9628bd75_7d80_42de_b44d_e0304c159d90,
        ))?;
        // Keep folder-picker history separate from the send-file dialog.
        if let Ok(folder) = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) {
            let item = SHCreateItemFromParsingName::<_, _, IShellItem>(folder, None);
            CoTaskMemFree(Some(folder.0.cast()));
            if let Ok(item) = item {
                let _ = dialog.SetDefaultFolder(&item);
            }
        }
        match dialog.Show(Some(HWND(owner as *mut _))) {
            Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) => {
                return Ok(None);
            }
            result => result?,
        }
        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0.cast()));
        Ok(Some(path))
    }
}
