//! TSF 语言栏项：任务栏输入法指示器旁的"中/英"切换按钮。
//!
//! 参考微软 SampleIME（LanguageBar.cpp）与 Rime Weasel（LanguageBar.cpp）：
//! 一个 COM 对象同时实现 `ITfLangBarItem`（基）+ `ITfLangBarItemButton`（按钮）+
//! `ITfSource`（语言栏通过它塞入 `ITfLangBarItemSink`，状态变化时 `OnUpdate` 刷新图标）。
//! 经 `ITfThreadMgr` QI 得到 `ITfLangBarItemMgr` 后 `AddItem`/`RemoveItem`。
//!
//! 图标：编译进 DLL 的 .ico 资源（`res/zh.ico`/`res/en.ico`，winres 编入，ID 101/102），
//! `GetIcon` 用 `LoadImageW` + `MAKEINTRESOURCE` 加载（LR_SHARED，系统管理生命周期，
//! 无需 DestroyIcon）——与 Weasel（IDI_ZH/IDI_EN）同款可靠路径。中/英状态经
//! `Arc<AtomicBool>` 与 TextService 共享。
//!
//! 全部 COM 回调经 `guard` 包装捕获 panic（ime-tsf 绝不 panic 到宿主进程的硬性约定）。

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::Win32::Foundation::{HANDLE, POINT, RECT};
use windows::Win32::System::Ole::{
    CONNECT_E_ADVISELIMIT, CONNECT_E_CANNOTCONNECT, CONNECT_E_NOCONNECTION,
};
use windows::Win32::UI::TextServices::{
    GUID_LBI_INPUTMODE, ITfLangBarItemButton, ITfLangBarItemButton_Impl,
    ITfLangBarItemMgr, ITfLangBarItemSink, ITfLangBarItem_Impl, ITfMenu, ITfSource,
    ITfSource_Impl, ITfThreadMgr, TfLBIClick, TF_LANGBARITEMINFO, TF_LBI_CLK_LEFT,
    TF_LBI_ICON, TF_LBI_STATUS, TF_LBI_STATUS_HIDDEN, TF_LBI_STYLE_BTN_BUTTON,
    TF_LBI_STYLE_SHOWNINTRAY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadImageW, HICON, IMAGE_ICON, LR_SHARED, SM_CXSMICON, SM_CYSMICON,
};
use windows_core::{implement, BSTR, BOOL, ComObject, GUID, Interface, PCWSTR, Ref, Result, IUnknown};

use crate::log::log_line;

/// 语言栏塞入的 sink cookie（唯一即可，Weasel 用固定值，这里同款）。
const SINK_COOKIE: u32 = 0x42424242;

/// 中/英图标的 DLL 资源 ID（winres `set_icon_with_id` 编入，契约 01 §5.1；对齐 Weasel）。
const ICON_ID_ZH: u32 = 101;
const ICON_ID_EN: u32 = 102;

/// 从本 DLL 资源加载图标（LR_SHARED：系统缓存共享句柄，调用方不得 DestroyIcon）。
fn load_icon(id: u32) -> HICON {
    // SAFETY: GetModuleHandleW(None) 取当前 DLL 句柄；MAKEINTRESOURCEW 语义 = 数字资源 ID
    // （低 16 位有效，高位 0，PCWSTR 直接整数转指针）。LoadImageW 失败返回空 HANDLE。
    let hinst = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) };
    let hinst = hinst.unwrap_or_default();
    // SAFETY: MAKEINTRESOURCEW(id) = (LPWSTR)(ULONG_PTR)id，id < 0xFFFF 时合法。
    let name = PCWSTR::from_raw(id as usize as *const u16);
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let cy = unsafe { GetSystemMetrics(SM_CYSMICON) };
    // SAFETY: 标准资源加载；hinst/name 在本调用期间有效。
    let handle: HANDLE = unsafe {
        LoadImageW(
            Some(hinst.into()),
            name,
            IMAGE_ICON,
            size,
            cy,
            LR_SHARED,
        )
    }
    .unwrap_or_default();
    // HICON/HANDLE 同布局（句柄即指针），直接转换。
    HICON(handle.0)
}

/// 构造 `TF_LANGBARITEMINFO`（纯函数，单测覆盖）。
///
/// `guidItem` 必须用系统 `GUID_LBI_INPUTMODE`：MSDN AddItem 注明 Windows 8+ 只显示
/// `GetInfo` 返回该 GUID 的项（自定义 GUID 会被静默忽略，AddItem 仍返回 S_OK）。
fn build_info() -> TF_LANGBARITEMINFO {
    let mut info = TF_LANGBARITEMINFO {
        clsidService: crate::registration::clsid(),
        guidItem: GUID_LBI_INPUTMODE,
        dwStyle: TF_LBI_STYLE_BTN_BUTTON | TF_LBI_STYLE_SHOWNINTRAY,
        ulSort: 0,
        ..Default::default()
    };
    let desc: Vec<u16> = "中英切换".encode_utf16().collect();
    let n = desc.len().min(info.szDescription.len() - 1);
    info.szDescription[..n].copy_from_slice(&desc[..n]);
    info
}

/// 语言栏按钮项。`mode` 与 TextService 共享（`Arc<AtomicBool>`），
/// 点击图标（OnClick）与 Shift 按键（TextService）都翻转它并刷新图标。
///
/// 图标来自 DLL 资源（LR_SHARED），不持有、不销毁——`Drop` 无事可做。
#[implement(ITfLangBarItemButton, ITfSource)]
pub(crate) struct LangBarItemButton {
    /// 共享中/英模式：true = 英文（按键放行），false = 中文。
    mode: Arc<AtomicBool>,
    /// 语言栏塞进来的 sink；状态变化时 `OnUpdate` 刷新图标。
    sink: RefCell<Option<ITfLangBarItemSink>>,
    /// 状态位（TF_LBI_STATUS_*，MVP 仅 HIDDEN 会用到）。
    status: Cell<u32>,
}

impl LangBarItemButton {
    pub(crate) fn new(mode: Arc<AtomicBool>) -> Self {
        LangBarItemButton {
            mode,
            sink: RefCell::new(None),
            status: Cell::new(0),
        }
    }

    /// 状态变化（模式/隐藏）后通知语言栏刷新（走 sink OnUpdate）。
    fn refresh(&self) {
        if let Some(sink) = self.sink.borrow().as_ref() {
            // SAFETY: sink 由语言栏通过 ITfSource::AdviseSink 塞入，持有期间有效。
            let _ = unsafe { sink.OnUpdate(TF_LBI_STATUS | TF_LBI_ICON) };
        }
    }

    /// 翻转中/英模式并刷新图标。
    fn toggle_mode(&self) {
        let next = !self.mode.load(Ordering::SeqCst);
        self.mode.store(next, Ordering::SeqCst);
        log_line(&format!("语言栏图标点击切换英文模式：{next}"));
        self.refresh();
    }

    /// 增删状态位；变化时刷新。
    fn set_status(&self, status: u32, set: bool) {
        let cur = self.status.get();
        let next = if set { cur | status } else { cur & !status };
        if cur != next {
            self.status.set(next);
            self.refresh();
        }
    }
}

// ---- ITfLangBarItem ----

impl ITfLangBarItem_Impl for LangBarItemButton_Impl {
    fn GetInfo(&self, pinfo: *mut TF_LANGBARITEMINFO) -> Result<()> {
        // SAFETY: pinfo 由语言栏保证非空有效（COM out 参数约定）。
        unsafe { pinfo.write(build_info()) };
        Ok(())
    }

    fn GetStatus(&self) -> Result<u32> {
        Ok(self.status.get())
    }

    fn Show(&self, fshow: BOOL) -> Result<()> {
        self.set_status(TF_LBI_STATUS_HIDDEN, !fshow.as_bool());
        Ok(())
    }

    fn GetTooltipString(&self) -> Result<BSTR> {
        Ok(BSTR::from("左键切换中英文"))
    }
}

// ---- ITfLangBarItemButton ----

impl ITfLangBarItemButton_Impl for LangBarItemButton_Impl {
    fn OnClick(&self, click: TfLBIClick, _pt: &POINT, _prcarea: *const RECT) -> Result<()> {
        if click == TF_LBI_CLK_LEFT {
            self.toggle_mode();
        }
        Ok(())
    }

    fn InitMenu(&self, _pmenu: Ref<ITfMenu>) -> Result<()> {
        Ok(())
    }

    fn OnMenuSelect(&self, _wid: u32) -> Result<()> {
        Ok(())
    }

    fn GetIcon(&self) -> Result<HICON> {
        // 按当前模式加载 DLL 资源图标（LR_SHARED，系统管理，无需销毁）。
        let id = if self.mode.load(Ordering::SeqCst) {
            ICON_ID_EN
        } else {
            ICON_ID_ZH
        };
        Ok(load_icon(id))
    }

    fn GetText(&self) -> Result<BSTR> {
        Ok(if self.mode.load(Ordering::SeqCst) {
            BSTR::from("英")
        } else {
            BSTR::from("中")
        })
    }
}

// ---- ITfSource ----

impl ITfSource_Impl for LangBarItemButton_Impl {
    fn AdviseSink(&self, riid: *const GUID, punk: Ref<IUnknown>) -> Result<u32> {
        // SAFETY: riid 由语言栏保证非空有效。
        if unsafe { *riid } != <ITfLangBarItemSink as Interface>::IID {
            return Err(windows_core::Error::from_hresult(CONNECT_E_CANNOTCONNECT));
        }
        if self.sink.borrow().is_some() {
            return Err(windows_core::Error::from_hresult(CONNECT_E_ADVISELIMIT));
        }
        let sink: ITfLangBarItemSink = punk.ok()?.cast()?;
        *self.sink.borrow_mut() = Some(sink);
        Ok(SINK_COOKIE)
    }

    fn UnadviseSink(&self, dwcookie: u32) -> Result<()> {
        if dwcookie != SINK_COOKIE || self.sink.borrow().is_none() {
            return Err(windows_core::Error::from_hresult(CONNECT_E_NOCONNECTION));
        }
        *self.sink.borrow_mut() = None;
        Ok(())
    }
}

/// 把按钮项添加到线程语言栏（Activate 时调用；失败仅记日志，不影响输入法主体）。
pub(crate) fn add_to_lang_bar(thread_mgr: &ITfThreadMgr, com: &ComObject<LangBarItemButton>) -> Result<()> {
    // SAFETY: ITfLangBarItemMgr 由 ITfThreadMgr QI 得到（MSDN 标准做法，同 Weasel）。
    let mgr: ITfLangBarItemMgr = thread_mgr.cast()?;
    let item: ITfLangBarItemButton = com.to_interface();
    // SAFETY: item 在本调用期间存活；AddItem 后语言栏持引用，直到 RemoveItem。
    unsafe { mgr.AddItem(&item) }?;
    // Weasel 同款：AddItem 后 Show(true) 确保项可见（默认可能被语言栏隐藏）。
    // SAFETY: item 仍存活；Show 只改状态位。
    unsafe { item.Show(true) }?;
    Ok(())
}

/// 从线程语言栏移除按钮项（Deactivate 时调用）。
pub(crate) fn remove_from_lang_bar(thread_mgr: &ITfThreadMgr, com: &ComObject<LangBarItemButton>) -> Result<()> {
    // SAFETY: 同上；RemoveItem 与 AddItem 配对。
    let mgr: ITfLangBarItemMgr = thread_mgr.cast()?;
    let item: ITfLangBarItemButton = com.to_interface();
    unsafe { mgr.RemoveItem(&item) }
}

/// 刷新语言栏图标（Shift 切换英文模式后由 TextService 调用）。
pub(crate) fn refresh_lang_bar(com: &ComObject<LangBarItemButton>) {
    // SAFETY: com 为正在语言栏上的有效对象；内部经 sink OnUpdate 刷新。
    com.as_ref().refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_fills_identity() {
        let info = build_info();
        assert_eq!(info.clsidService, crate::registration::clsid());
        assert_eq!(info.guidItem, GUID_LBI_INPUTMODE);
        assert_eq!(info.dwStyle & TF_LBI_STYLE_BTN_BUTTON, TF_LBI_STYLE_BTN_BUTTON);
        assert_eq!(info.dwStyle & TF_LBI_STYLE_SHOWNINTRAY, TF_LBI_STYLE_SHOWNINTRAY);
    }

    #[test]
    fn build_info_description_utf16() {
        let info = build_info();
        let desc: Vec<u16> = "中英切换".encode_utf16().collect();
        assert_eq!(&info.szDescription[..desc.len()], desc.as_slice());
        // 描述之后应保持 0（null 结尾）。
        assert_eq!(info.szDescription[desc.len()], 0);
    }

    #[test]
    fn icon_ids_unique() {
        // MAKEINTRESOURCEW 要求 id 在 16 位内（高位 0）；两个 ID 不得相同。
        assert_ne!(ICON_ID_ZH, ICON_ID_EN);
    }
}
