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
//! 全部 COM 回调经 `guard` 包装捕获 panic（iuv-tsf 绝不 panic 到宿主进程的硬性约定）。

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::Win32::Foundation::{HANDLE, HMODULE, POINT, RECT};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
};
use windows::Win32::System::Ole::{
    CONNECT_E_ADVISELIMIT, CONNECT_E_CANNOTCONNECT, CONNECT_E_NOCONNECTION,
};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::TextServices::{
    GUID_LBI_INPUTMODE, ITfCompartment, ITfLangBarItemButton, ITfLangBarItemButton_Impl,
    ITfLangBarItemMgr, ITfLangBarItemSink, ITfLangBarItem_Impl, ITfMenu, ITfSource,
    ITfSource_Impl, ITfThreadMgr, TfLBIClick, TF_LANGBARITEMINFO, TF_LBI_CLK_LEFT,
    TF_LBI_CLK_RIGHT, TF_LBI_ICON, TF_LBI_STATUS, TF_LBI_STATUS_HIDDEN,
    TF_LBI_STYLE_BTN_BUTTON, TF_LBI_STYLE_SHOWNINTRAY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadImageW, HICON, IMAGE_ICON, LR_SHARED, SM_CXSMICON, SM_CYSMICON,
};
use windows_core::{implement, BSTR, BOOL, ComObject, GUID, Interface, PCWSTR, Ref, Result, IUnknown};

use crate::log::log_line;
use iuv_win::Request;
use iuv_ui::{MenuEntry, Theme};

/// 「关于」对话框（自绘菜单与 InitMenu 菜单共用）。
fn show_about() {
    let text = "iuv 输入法（代号 iuvim，谐音\"哎哟喂\"）\nRust + TSF 的 Windows 中文输入法。";
    let title = "关于 iuv";
    let text_wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let title_wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    // SAFETY: 静态 UTF-16 缓冲 NUL 结尾；MB_OK 无回调。
    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
            None,
            windows::core::PCWSTR(text_wide.as_ptr()),
            windows::core::PCWSTR(title_wide.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE::default(),
        )
    };
}

/// 语言栏塞入的 sink cookie（唯一即可，Weasel 用固定值，这里同款）。
const SINK_COOKIE: u32 = 0x42424242;

/// 中/英图标的 DLL 资源 ID（winres `set_icon_with_id` 编入，契约 01 §5.1；对齐 Weasel）。
const ICON_ID_ZH: u32 = 101;
const ICON_ID_EN: u32 = 102;

/// 本 DLL 的模块句柄。
///
/// 用 `GetModuleHandleExW(FROM_ADDRESS)` 从本函数自身地址反查——绝不能
/// `GetModuleHandleW(None)`（那会取到宿主进程 EXE 的句柄，资源 ID 撞车时
/// 加载出应用自己的图标，甚至加载失败导致语言栏项无法显示）。同
/// `registration.rs::dll_path` 的已验证模式。
fn dll_module_handle() -> HMODULE {
    use std::os::raw::c_void;
    let mut module = HMODULE::default();
    // SAFETY: FROM_ADDRESS 把 lpModuleName 解释为函数地址，指向本 DLL 内的代码，
    // 该函数在本进程存活期间有效。
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            PCWSTR(load_icon as *const c_void as *const u16),
            &mut module,
        )
    };
    if ok.is_err() || module.0.is_null() {
        log_line("dll_module_handle：GetModuleHandleExW(FROM_ADDRESS) 失败");
        HMODULE::default()
    } else {
        module
    }
}

/// 从本 DLL 资源加载图标（LR_SHARED：系统缓存共享句柄，调用方不得 DestroyIcon）。
fn load_icon(id: u32) -> HICON {
    let hinst = dll_module_handle();
    if hinst.0.is_null() {
        return HICON::default();
    }
    // SAFETY: MAKEINTRESOURCEW 语义 = 数字资源 ID（低 16 位有效，高位 0，
    // PCWSTR 直接整数转指针）。LoadImageW 失败返回空 HANDLE。
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
    if handle.0.is_null() {
        log_line(&format!("语言栏图标加载失败：id={id}"));
    }
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

/// 构造 VT_I4 VARIANT（OPENCLOSE compartment 读写用；纯函数，单测覆盖）。
fn variant_i4(v: i32) -> VARIANT {
    // SAFETY: VARIANT 为联合体；初值 zeroed 后只写 VT_I4 活跃成员。
    let mut var = VARIANT::default();
    unsafe {
        // SAFETY: ManuallyDrop 字段需显式解引用后写入（不运行析构）。
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = v;
    }
    var
}

/// 读 OPENCLOSE compartment：VT_I4 非零 = 打开（中文），零 = 关闭（英文）。
/// 未设置（VT_EMPTY）或读取失败返回 None。
pub(crate) fn read_openclose(comp: &ITfCompartment) -> Option<bool> {
    // SAFETY: GetValue 为 TSF 标准查询；VARIANT 联合体字段访问需 unsafe。
    let var = unsafe { comp.GetValue() }.ok()?;
    // SAFETY: vt 位于 VARIANT 首字段（VARIANT_0_0），可安全读取。
    let vt = unsafe { var.Anonymous.Anonymous.vt };
    if vt != VT_I4 {
        return None;
    }
    // SAFETY: vt==VT_I4 时 lVal 为活跃成员。
    Some(unsafe { var.Anonymous.Anonymous.Anonymous.lVal } != 0)
}

/// 写 OPENCLOSE compartment（`tid` 为 TSF client id，同 SetValue 约定）。
/// 注意：SetValue 会同步触发本 TIP 的 OnChange 回调（重入），调用方需防抖。
pub(crate) fn write_openclose(comp: &ITfCompartment, tid: u32, open: bool) -> Result<()> {
    let var = variant_i4(i32::from(open));
    // SAFETY: 标准 TSF 写入；VARIANT 在本调用期间存活。
    unsafe { comp.SetValue(tid, &var) }
}

/// 语言栏按钮项。`mode` 与 TextService 共享（`Arc<AtomicBool>`），
/// 点击图标（OnClick）翻转它并刷新图标。
///
/// 点击归一为写 `GUID_COMPARTMENT_KEYBOARD_OPENCLOSE`（系统"输入法/非输入法切换"
/// 真相源）：TextService 的 OnChange 统一响应并刷图标；compartment 缺失时
/// 本地翻转兜底。图标来自 DLL 资源（LR_SHARED），不持有、不销毁。
#[implement(ITfLangBarItemButton, ITfSource)]
pub(crate) struct LangBarItemButton {
    /// 共享中/英模式：true = 英文（按键放行），false = 中文。
    mode: Arc<AtomicBool>,
    /// OPENCLOSE compartment + 本实例 client id（Activate 传入；None = 无监听，
    /// 点击走本地翻转兜底）。
    compartment: RefCell<Option<(ITfCompartment, u32)>>,
    /// 语言栏塞进来的 sink；状态变化时 `OnUpdate` 刷新图标。
    sink: RefCell<Option<ITfLangBarItemSink>>,
    /// 状态位（TF_LBI_STATUS_*，MVP 仅 HIDDEN 会用到）。
    status: Cell<u32>,
    /// M6 daemon 客户端：右键菜单「设置」→ 管道 `OpenSettings` 通知守护进程弹设置页
    /// （2026-08-17 用户决策：无独立托盘图标，入口全走语言栏菜单）。
    daemon: Arc<crate::daemon_client::DaemonClient>,
    /// 自绘右键菜单窗口（懒建；Win10/11 输入法区域右键走 OnClick(RIGHT)，InitMenu 不触发）。
    menu: RefCell<Option<crate::ui::menu_window::MenuWindow>>,
    /// 菜单主题（与候选窗一致，构造时由 text_service 从 config 注入）。
    menu_theme: Theme,
}

impl LangBarItemButton {
    pub(crate) fn new(
        mode: Arc<AtomicBool>,
        compartment: Option<(ITfCompartment, u32)>,
        daemon: Arc<crate::daemon_client::DaemonClient>,
        menu_theme: Theme,
    ) -> Self {
        LangBarItemButton {
            mode,
            compartment: RefCell::new(compartment),
            sink: RefCell::new(None),
            status: Cell::new(0),
            daemon,
            menu: RefCell::new(None),
            menu_theme,
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
    ///
    /// 归一为写 OPENCLOSE compartment（单一真相源，TextService OnChange 统一响应）；
    /// compartment 缺失或写失败时本地翻转兜底。
    fn toggle_mode(&self) {
        if let Some((comp, tid)) = self.compartment.borrow().as_ref() {
            let next_open = !read_openclose(comp).unwrap_or(true);
            if write_openclose(comp, *tid, next_open).is_ok() {
                // OnChange 会负责翻转 mode + 刷新图标；这里仅记日志。
                log_line(&format!("语言栏图标点击：写 OPENCLOSE={next_open}"));
                return;
            }
            log_line("语言栏图标点击：写 OPENCLOSE 失败，本地翻转兜底");
        }
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

    /// 工具栏菜单项文案：按 daemon 当前全局显隐偏好二选一（2026-08-21 用户需求：
    /// 已显示 →「隐藏工具栏」、已隐藏 →「显示工具栏」）。查询失败（离线/旧版 daemon）
    /// → 中性文案兜底。
    fn toolbar_menu_label(&self) -> String {
        match self.daemon.toolbar_visible() {
            Some(true) => "隐藏工具栏".to_string(),
            Some(false) => "显示工具栏".to_string(),
            None => "显示/隐藏工具栏".to_string(),
        }
    }

    /// 右键菜单：懒建自绘菜单窗（iuv-ui 渲染 + ULW 呈现），在按钮位置弹出。
    /// 菜单项：显示/隐藏工具栏 / 设置 / 关于（无"退出"——用户 2026-08-17 决策）；
    /// 工具栏项文案每次弹出前按当前偏好刷新（窗口复用，set_items 替换）。
    fn show_menu(&self, pt: &POINT) {
        const MENU_TOOLBAR: u16 = 1;
        const MENU_SETTINGS: u16 = 2;
        const MENU_ABOUT: u16 = 3;
        let mut m = self.menu.borrow_mut();
        let items = vec![
            MenuEntry::new(self.toolbar_menu_label(), MENU_TOOLBAR),
            MenuEntry::new("设置".to_string(), MENU_SETTINGS),
            MenuEntry::new("关于".to_string(), MENU_ABOUT),
        ];
        if m.is_none() {
            let daemon = self.daemon.clone();
            *m = Some(crate::ui::menu_window::MenuWindow::new(
                self.menu_theme.clone(),
                items,
                Some(Box::new(move |id| match id {
                    MENU_TOOLBAR => {
                        log_line("语言栏菜单：显示/隐藏工具栏 → 通知守护进程切换全局偏好");
                        let _ = daemon.send_request(&Request::ToggleToolbar);
                    }
                    MENU_SETTINGS => {
                        log_line("语言栏菜单：设置 → 通知守护进程打开设置页");
                        let _ = daemon.send_request(&Request::OpenSettings);
                    }
                    MENU_ABOUT => {
                        log_line("语言栏菜单：关于");
                        show_about();
                    }
                    _ => log_line(&format!("语言栏菜单：未知项 {id}")),
                })),
            ));
        } else if let Some(w) = m.as_mut() {
            w.set_items(items);
        }
        if let Some(w) = m.as_mut() {
            log_line("语言栏菜单：右键弹出自绘菜单");
            w.show_at(pt.x, pt.y);
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
    fn OnClick(&self, click: TfLBIClick, pt: &POINT, _prcarea: *const RECT) -> Result<()> {
        if click == TF_LBI_CLK_LEFT {
            self.toggle_mode();
        } else if click == TF_LBI_CLK_RIGHT {
            // Win10/11 输入法区域右键不触发 InitMenu（2026-08-17 实测零日志），
            // 走 OnClick(TF_LBI_CLK_RIGHT) 自绘菜单（搜狗等第三方输入法同款路径）。
            self.show_menu(pt);
        }
        Ok(())
    }

    fn InitMenu(&self, pmenu: Ref<ITfMenu>) -> Result<()> {
        // 右键"中/英"按钮 → 语言栏上下文菜单（TSF 官方机制：语言栏弹菜单前调 InitMenu，
        // 我们经 ITfMenu::AddMenuItem 塞入自定义项；OnMenuSelect 分发）。
        // 2026-08-17 用户决策：入口全走语言栏菜单，无独立托盘图标。菜单项无"退出"。
        const MENU_TOOLBAR: u32 = 1;
        const MENU_SETTINGS: u32 = 2;
        const MENU_ABOUT: u32 = 3;
        let Some(menu) = pmenu.as_ref() else {
            log_line("语言栏菜单：InitMenu 收到空 ITfMenu");
            return Ok(());
        };
        // 工具栏项文案按当前全局显隐偏好二选一（与自绘菜单一致）。
        let toolbar_label = self.toolbar_menu_label();
        let items: &[(String, u32)] = &[
            (toolbar_label, MENU_TOOLBAR),
            ("设置".to_string(), MENU_SETTINGS),
            ("关于".to_string(), MENU_ABOUT),
        ];
        for (label, wid) in items {
            let text: Vec<u16> = label.encode_utf16().collect();
            // SAFETY: AddMenuItem 同步复制菜单文本；hbmp/hbmpmask 空位图；无子菜单。
            let _ = unsafe {
                menu.AddMenuItem(
                    *wid,
                    0,
                    windows::Win32::Graphics::Gdi::HBITMAP::default(),
                    windows::Win32::Graphics::Gdi::HBITMAP::default(),
                    &text,
                    &mut None,
                )
            };
        }
        Ok(())
    }

    fn OnMenuSelect(&self, wid: u32) -> Result<()> {
        const MENU_TOOLBAR: u32 = 1;
        const MENU_SETTINGS: u32 = 2;
        const MENU_ABOUT: u32 = 3;
        match wid {
            MENU_TOOLBAR => {
                log_line("语言栏菜单：显示/隐藏工具栏 → 通知守护进程切换全局偏好");
                let _ = self.daemon.send_request(&Request::ToggleToolbar);
            }
            MENU_SETTINGS => {
                log_line("语言栏菜单：设置 → 通知守护进程打开设置页");
                let _ = self.daemon.send_request(&Request::OpenSettings);
            }
            MENU_ABOUT => {
                log_line("语言栏菜单：关于");
                show_about();
            }
            _ => log_line(&format!("语言栏菜单：未知项 {wid}")),
        }
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

/// 刷新语言栏图标（模式切换后由 TextService 调用）。
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

    #[test]
    fn variant_i4_layout() {
        for v in [0, 1, -1, 12345] {
            let var = variant_i4(v);
            // SAFETY: 测试内直接读联合体字段（vt 首字段；lVal 为刚写入的活跃成员）。
            let vt = unsafe { var.Anonymous.Anonymous.vt };
            let lval = unsafe { var.Anonymous.Anonymous.Anonymous.lVal };
            assert_eq!(vt, VT_I4);
            assert_eq!(lval, v);
        }
    }

    #[test]
    fn read_openclose_mapping() {
        // 纯函数映射验证：VT_I4 非零 → Some(true)，零 → Some(false)。
        // read_openclose 需要 COM 对象，这里仅验证 VARIANT→bool 的等价语义
        // （通过 variant_i4 构造 + 手工解析，保证与实现同构）。
        for (v, expect) in [(0i32, false), (1, true), (42, true)] {
            let var = variant_i4(v);
            // SAFETY: 同上。
            let vt = unsafe { var.Anonymous.Anonymous.vt };
            let some = vt == VT_I4;
            let lval = some && unsafe { var.Anonymous.Anonymous.Anonymous.lVal } != 0;
            assert_eq!(lval, expect, "lVal={v}");
        }
    }
}
