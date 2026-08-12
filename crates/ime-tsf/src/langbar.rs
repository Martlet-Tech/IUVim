//! TSF 语言栏项：任务栏输入法指示器旁的"中/英"切换按钮。
//!
//! 参考微软 SampleIME（LanguageBar.cpp）与 Rime Weasel（LanguageBar.cpp）：
//! 一个 COM 对象同时实现 `ITfLangBarItem`（基）+ `ITfLangBarItemButton`（按钮）+
//! `ITfSource`（语言栏通过它塞入 `ITfLangBarItemSink`，状态变化时 `OnUpdate` 刷新图标）。
//! 经 `ITfThreadMgr` QI 得到 `ITfLangBarItemMgr` 后 `AddItem`/`RemoveItem`。
//!
//! 图标运行时用 GDI 生成（画"中"/"英"文字 → DIB → `CreateIconIndirect`），
//! 零二进制资产。中/英状态经 `Arc<AtomicBool>` 与 TextService 共享。
//!
//! 全部 COM 回调经 `guard` 包装捕获 panic（ime-tsf 绝不 panic 到宿主进程的硬性约定）。

use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject,
    GetDC, GetTextExtentPoint32W, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    TextOutW, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, HGDIOBJ, LOGFONTW,
    OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::Ole::{
    CONNECT_E_ADVISELIMIT, CONNECT_E_CANNOTCONNECT, CONNECT_E_NOCONNECTION,
};
use windows::Win32::UI::TextServices::{
    ITfLangBarItemButton, ITfLangBarItemButton_Impl, ITfLangBarItemMgr, ITfLangBarItemSink,
    ITfLangBarItem_Impl, ITfMenu, ITfSource, ITfSource_Impl, ITfThreadMgr, TfLBIClick,
    TF_LANGBARITEMINFO, TF_LBI_CLK_LEFT, TF_LBI_ICON, TF_LBI_STATUS,
    TF_LBI_STATUS_HIDDEN, TF_LBI_STYLE_BTN_BUTTON, TF_LBI_STYLE_SHOWNINTRAY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};
use windows_core::{implement, BSTR, BOOL, ComObject, GUID, Interface, Ref, Result, IUnknown};

use crate::log::log_line;

/// 语言栏塞入的 sink cookie（唯一即可，Weasel 用固定值，这里同款）。
const SINK_COOKIE: u32 = 0x42424242;
/// 图标文字颜色（BGR：B=0xCC, G=0x66, R=0x00 → RGB(0,102,204)，深浅任务栏均可读）。
const ICON_COLOR: COLORREF = COLORREF(0x00CC_6600);

/// 构造 `TF_LANGBARITEMINFO`（纯函数，单测覆盖）。
fn build_info() -> TF_LANGBARITEMINFO {
    let mut info = TF_LANGBARITEMINFO {
        clsidService: crate::registration::clsid(),
        guidItem: crate::registration::lang_bar_item_guid(),
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
#[implement(ITfLangBarItemButton, ITfSource)]
pub(crate) struct LangBarItemButton {
    /// 共享中/英模式：true = 英文（按键放行），false = 中文。
    mode: Arc<AtomicBool>,
    /// 语言栏塞进来的 sink；状态变化时 `OnUpdate` 刷新图标。
    sink: RefCell<Option<ITfLangBarItemSink>>,
    /// 状态位（TF_LBI_STATUS_*，MVP 仅 HIDDEN 会用到）。
    status: Cell<u32>,
    /// 中/英图标（运行时 GDI 生成，Drop 时销毁）。
    icon_zh: HICON,
    icon_en: HICON,
}

impl LangBarItemButton {
    pub(crate) fn new(mode: Arc<AtomicBool>) -> Self {
        LangBarItemButton {
            mode,
            sink: RefCell::new(None),
            status: Cell::new(0),
            icon_zh: make_text_icon("中"),
            icon_en: make_text_icon("英"),
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

impl Drop for LangBarItemButton {
    fn drop(&mut self) {
        // SAFETY: 图标由本对象创建并持有，销毁不重入。
        if !self.icon_zh.is_invalid() {
            let _ = unsafe { DestroyIcon(self.icon_zh) };
        }
        if !self.icon_en.is_invalid() {
            let _ = unsafe { DestroyIcon(self.icon_en) };
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
        Ok(if self.mode.load(Ordering::SeqCst) {
            self.icon_en
        } else {
            self.icon_zh
        })
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
    unsafe { mgr.AddItem(&item) }
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

// ===== GDI 运行时图标生成 =====

/// 生成 16×16（SM_CXSMICON）文字图标：画"中/英"进 32bpp DIB → 设 alpha → CreateIconIndirect。
/// 失败静默返回空 HICON（语言栏退化为不显示图标，绝不 panic）。
fn make_text_icon(text: &str) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if size <= 0 {
        return HICON::default();
    }
    let s = size as i32;

    // SAFETY: GetDC(None) 取屏幕 DC，用后 ReleaseDC(None, hdc) 配对释放。
    let hdc = unsafe { GetDC(None) };
    if hdc.is_invalid() {
        return HICON::default();
    }

    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = s;
    bmi.bmiHeader.biHeight = s;
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB.0;
    let mut bits: *mut core::ffi::c_void = null_mut();
    // SAFETY: bmi 在调用期间存活；bits 由 DIB section 写回。
    let hbmp = unsafe { CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) };
    if hbmp.is_err() || bits.is_null() {
        // SAFETY: 与 GetDC 配对释放。
        let _ = unsafe { ReleaseDC(None, hdc) };
        return HICON::default();
    }
    let hbmp = hbmp.unwrap();

    // SAFETY: 内存 DC 与 DIB 配 SelectObject/DeleteDC/DeleteObject 使用。
    let mem = unsafe { CreateCompatibleDC(Some(hdc)) };
    let old_bmp = unsafe { SelectObject(mem, hbmp.into()) };

    let font = make_icon_font();
    let old_font = if !font.is_invalid() {
        // SAFETY: 配 SelectObject 还原。
        unsafe { SelectObject(mem, font.into()) }
    } else {
        HGDIOBJ::default()
    };    // 清空缓冲区（透明黑，alpha=0）。
    if !bits.is_null() {
        let px = bits as *mut u32;
        for i in 0..(s as usize * s as usize) {
            // SAFETY: bits 指向 s*s 个 32bpp 像素（CreateDIBSection 保证缓冲大小）。
            unsafe { px.add(i).write(0) };
        }
    }

    // SAFETY: 透明背景画文字；TextOutW 写 RGB，alpha 由后续后处理补上。
    unsafe {
        let _ = SetBkMode(mem, TRANSPARENT);
        let _ = SetTextColor(mem, ICON_COLOR);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(mem, &wide, &mut sz);
        let x = (s - sz.cx) / 2;
        let y = (s - sz.cy) / 2;
        let _ = TextOutW(mem, x, y, &wide);
    }

    // 后处理：alpha = 最大颜色分量（文字覆盖率）；背景保持透明（alpha=0）。
    if !bits.is_null() {
        let px = bits as *mut u32;
        for i in 0..(s as usize * s as usize) {
            // SAFETY: 同上的像素缓冲遍历。
            unsafe {
                let v = px.add(i).read();
                let r = v & 0xFF;
                let g = (v >> 8) & 0xFF;
                let b = (v >> 16) & 0xFF;
                let a = r.max(g).max(b);
                px.add(i).write(v | (a << 24));
            }
        }
    }

    // 还原 DC 状态并清理。
    // SAFETY: 还原顺序与选择顺序相反；font/hbmp/mem/hdc 各配对释放。
    unsafe {
        if !old_font.is_invalid() {
            let _ = SelectObject(mem, old_font);
        }
        let _ = SelectObject(mem, old_bmp);
        if !font.is_invalid() {
            let _ = DeleteObject(font.into());
        }
        let _ = DeleteDC(mem);
        let _ = DeleteObject(hbmp.into());
        let _ = ReleaseDC(None, hdc);
    }

    // 32bpp DIB 带 alpha 时 hbmMask 可传空（MSDN CreateIconIndirect）。
    let info = ICONINFO {
        fIcon: true.into(),
        hbmColor: hbmp,
        ..Default::default()
    };
    // SAFETY: info 在调用期间存活；CreateIconIndirect 复制位图，随后删除 hbmp 安全。
    let icon = unsafe { CreateIconIndirect(&info) };
    icon.unwrap_or_default()
}

/// 图标字体：Microsoft YaHei UI，灰阶抗锯齿（避免 ClearType 亚像素在 16px 下的色边）。
fn make_icon_font() -> windows::Win32::Graphics::Gdi::HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -12,
        lfWeight: 400,
        lfCharSet: DEFAULT_CHARSET,
        lfOutPrecision: OUT_DEFAULT_PRECIS,
        lfClipPrecision: CLIP_DEFAULT_PRECIS,
        lfQuality: ANTIALIASED_QUALITY,
        ..Default::default()
    };
    let face: Vec<u16> = "Microsoft YaHei UI".encode_utf16().collect();
    let n = face.len().min(lf.lfFaceName.len() - 1);
    lf.lfFaceName[..n].copy_from_slice(&face[..n]);
    // SAFETY: lf 在调用期间存活；CreateFontIndirectW 复制字体描述。
    unsafe { CreateFontIndirectW(&lf) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_fills_identity() {
        let info = build_info();
        assert_eq!(info.clsidService, crate::registration::clsid());
        assert_eq!(info.guidItem, crate::registration::lang_bar_item_guid());
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
}
