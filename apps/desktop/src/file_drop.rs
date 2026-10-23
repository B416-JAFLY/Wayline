//! Handle shell file drops directly, without turning drag events into clicks.
use crate::native::Action;
use anyhow::{Context, Result, ensure};
use std::{
    cell::Cell, ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf, sync::mpsc::Sender,
};
use windows::{
    Win32::{
        Foundation::{HWND, POINT, POINTL, S_OK},
        System::{Com::*, Ole::*, SystemServices::MODIFIERKEYS_FLAGS},
        UI::Shell::{CLSID_DragDropHelper, DragQueryFileW, HDROP, IDropTargetHelper},
    },
    core::{Ref, implement},
};

pub fn register(hwnd: HWND, tx: Sender<Action>) -> Result<()> {
    unsafe {
        let target: IDropTarget = FileDropTarget {
            hwnd,
            tx,
            helper: CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok(),
            accepting: Cell::new(false),
        }
        .into();
        // GPUI 0.2.2 dereferences hGlobal as an HDROP pointer instead of using
        // the handle itself. Own the native target so file paths stay intact.
        RevokeDragDrop(hwnd).context("撤销原窗口拖放目标失败")?;
        RegisterDragDrop(hwnd, &target).context("注册文件拖放目标失败")?;
    }
    Ok(())
}

pub fn unregister(hwnd: HWND) {
    let _ = unsafe { RevokeDragDrop(hwnd) };
}

fn file_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    }
}

struct Medium(STGMEDIUM);
impl Drop for Medium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

fn read_files(data: &IDataObject) -> Result<Vec<PathBuf>> {
    let text = crate::i18n::current().strings();
    let medium = Medium(unsafe { data.GetData(&file_format())? });
    ensure!(
        medium.0.tymed == TYMED_HGLOBAL.0 as u32,
        text.err_drag_invalid_data
    );
    let global = unsafe { medium.0.u.hGlobal };
    ensure!(!global.is_invalid(), text.err_drag_empty);
    // CF_HDROP is an HGLOBAL handle, not a pointer to another handle.
    read_hdrop(HDROP(global.0))
}

fn read_hdrop(drop: HDROP) -> Result<Vec<PathBuf>> {
    let text = crate::i18n::current().strings();
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    ensure!(count > 0 && count <= 16384, text.err_drag_count);
    let mut paths = Vec::with_capacity(count as usize);
    for index in 0..count {
        let len = unsafe { DragQueryFileW(drop, index, None) } as usize;
        ensure!(len > 0 && len <= 32767, text.err_drag_path);
        let mut name = vec![0u16; len + 1];
        let copied = unsafe { DragQueryFileW(drop, index, Some(&mut name)) } as usize;
        ensure!(copied == len, text.err_drag_incomplete);
        paths.push(PathBuf::from(OsString::from_wide(&name[..len])));
    }
    Ok(paths)
}

#[implement(IDropTarget)]
struct FileDropTarget {
    hwnd: HWND,
    tx: Sender<Action>,
    helper: Option<IDropTargetHelper>,
    accepting: Cell<bool>,
}

impl FileDropTarget {
    fn hover(&self, active: bool) {
        if self.accepting.replace(active) != active {
            let _ = self.tx.send(Action::DropHover(active));
        }
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for FileDropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let data = data.ok()?;
        unsafe {
            let accepted = data.QueryGetData(&file_format()) == S_OK
                && (*effect & DROPEFFECT_COPY) != DROPEFFECT_NONE;
            self.hover(accepted);
            *effect = if accepted {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            };
            if let Some(helper) = &self.helper {
                let _ = helper.DragEnter(self.hwnd, data, &POINT { x: pt.x, y: pt.y }, *effect);
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            *effect = if self.accepting.get() && (*effect & DROPEFFECT_COPY) != DROPEFFECT_NONE {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            };
            if let Some(helper) = &self.helper {
                let _ = helper.DragOver(&POINT { x: pt.x, y: pt.y }, *effect);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.hover(false);
        if let Some(helper) = &self.helper {
            let _ = unsafe { helper.DragLeave() };
        }
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let accepted = self.accepting.get();
        self.hover(false);
        let data = data.ok()?;
        unsafe {
            *effect = DROPEFFECT_NONE;
            if accepted {
                match read_files(data) {
                    Ok(paths) => {
                        if self.tx.send(Action::FilesDropped(paths)).is_ok() {
                            *effect = DROPEFFECT_COPY;
                        }
                    }
                    Err(error) => {
                        let _ = self.tx.send(Action::ShellError(format!(
                            "{}{error}",
                            crate::i18n::current().strings().err_drag_read
                        )));
                    }
                }
            }
            if let Some(helper) = &self.helper {
                let _ = helper.Drop(data, &POINT { x: pt.x, y: pt.y }, *effect);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use windows::Win32::{
        Foundation::{DV_E_FORMATETC, E_NOTIMPL},
        System::Memory::*,
        UI::Shell::DROPFILES,
    };

    #[implement(IDataObject)]
    struct TestDataObject(Option<Vec<String>>);

    #[allow(non_snake_case)]
    impl IDataObject_Impl for TestDataObject_Impl {
        fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            self.QueryGetData(format).ok()?;
            Ok(medium_with_files(self.0.as_ref().unwrap()))
        }
        fn QueryGetData(&self, format: *const FORMATETC) -> windows::core::HRESULT {
            if self.0.is_some() && !format.is_null() && unsafe { (*format).cfFormat == CF_HDROP.0 }
            {
                S_OK
            } else {
                DV_E_FORMATETC
            }
        }
        fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn GetCanonicalFormatEtc(
            &self,
            _: *const FORMATETC,
            _: *mut FORMATETC,
        ) -> windows::core::HRESULT {
            E_NOTIMPL
        }
        fn SetData(
            &self,
            _: *const FORMATETC,
            _: *const STGMEDIUM,
            _: windows_core::BOOL,
        ) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn EnumFormatEtc(&self, _: u32) -> windows::core::Result<IEnumFORMATETC> {
            Err(E_NOTIMPL.into())
        }
        fn DAdvise(
            &self,
            _: *const FORMATETC,
            _: u32,
            _: Ref<IAdviseSink>,
        ) -> windows::core::Result<u32> {
            Err(E_NOTIMPL.into())
        }
        fn DUnadvise(&self, _: u32) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
            Err(E_NOTIMPL.into())
        }
    }

    fn data_with_files(names: &[&str]) -> IDataObject {
        TestDataObject(Some(names.iter().map(|name| name.to_string()).collect())).into()
    }

    fn medium_with_files(names: &[String]) -> STGMEDIUM {
        let units: Vec<u16> = names
            .iter()
            .flat_map(|name| name.encode_utf16().chain(Some(0)))
            .chain([0, 0])
            .collect();
        unsafe {
            let global = GlobalAlloc(
                GMEM_MOVEABLE | GMEM_ZEROINIT,
                size_of::<DROPFILES>() + units.len() * 2,
            )
            .unwrap();
            let memory = GlobalLock(global).cast::<u8>();
            assert!(!memory.is_null());
            memory.cast::<DROPFILES>().write(DROPFILES {
                pFiles: size_of::<DROPFILES>() as u32,
                fWide: true.into(),
                ..Default::default()
            });
            std::ptr::copy_nonoverlapping(
                units.as_ptr(),
                memory.add(size_of::<DROPFILES>()).cast::<u16>(),
                units.len(),
            );
            let _ = GlobalUnlock(global);
            STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: global },
                ..Default::default()
            }
        }
    }

    fn target() -> (IDropTarget, mpsc::Receiver<Action>) {
        let (tx, rx) = mpsc::channel();
        let target = FileDropTarget {
            hwnd: HWND::default(),
            tx,
            helper: None,
            accepting: Cell::new(false),
        }
        .into();
        (target, rx)
    }

    #[test]
    fn native_file_data_preserves_unicode_spaces_and_multiple_paths() {
        let _apartment = crate::picker::Apartment::new().unwrap();
        let names = [
            r"C:\测试 文件\图片 一.png",
            r"E:\reports\report & notes.txt",
        ];
        let data = data_with_files(&names);
        assert_eq!(read_files(&data).unwrap(), names.map(PathBuf::from));
        assert!(read_files(&data_with_files(&[])).is_err());
    }

    #[test]
    fn entering_and_leaving_never_submits_files_and_drop_submits_once() {
        let _apartment = crate::picker::Apartment::new().unwrap();
        let data = data_with_files(&[r"C:\test.txt"]);
        let (target, rx) = target();
        let point = POINTL { x: 10, y: 10 };
        let mut effect = DROPEFFECT_COPY;
        unsafe {
            target
                .DragEnter(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
            assert!(matches!(rx.try_recv(), Ok(Action::DropHover(true))));
            target
                .DragOver(MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
            assert!(rx.try_recv().is_err());
            target.DragLeave().unwrap();
            assert!(matches!(rx.try_recv(), Ok(Action::DropHover(false))));
            assert!(rx.try_recv().is_err());
            target
                .DragEnter(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
            assert!(matches!(rx.try_recv(), Ok(Action::DropHover(true))));
            target
                .Drop(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
            assert_eq!(effect, DROPEFFECT_COPY);
            assert!(matches!(rx.try_recv(), Ok(Action::DropHover(false))));
            assert!(
                matches!(rx.try_recv(), Ok(Action::FilesDropped(files)) if files == vec![PathBuf::from(r"C:\test.txt")])
            );
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn empty_drop_and_non_file_drag_never_submit_files() {
        let _apartment = crate::picker::Apartment::new().unwrap();
        let (target, rx) = target();
        let point = POINTL::default();
        unsafe {
            let data: IDataObject = TestDataObject(None).into();
            let mut effect = DROPEFFECT_COPY;
            target
                .DragEnter(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
            assert_eq!(effect, DROPEFFECT_NONE);
            target
                .Drop(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
            assert!(rx.try_recv().is_err());

            let data = data_with_files(&[]);
            effect = DROPEFFECT_COPY;
            target
                .DragEnter(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
            target
                .Drop(&data, MODIFIERKEYS_FLAGS(0), point, &mut effect)
                .unwrap();
            assert_eq!(effect, DROPEFFECT_NONE);
            assert!(
                rx.try_iter()
                    .all(|event| !matches!(event, Action::FilesDropped(_)))
            );
        }
    }
}
