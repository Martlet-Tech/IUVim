//! iuv-ime：老式 IMM 输入法组件（.ime），M1 探路——验证 WoW(1.12, 32位) 的 IMM 激活链路。
//! 架构参考 Weasel 0.9.30（GPL，仅参考结构不复制代码）：15 个 IMM 导出函数，
//! 核心为 ImeInquire/ImeProcessKey/ImeSelect/ImeSetActiveContext；M1 全部打日志验证激活，
//! 后续里程碑接 iuv-core 引擎 + GDI 候选窗（进程内，无需独立 Server 进程）。
//!
//! IMM 是 32 位 ABI：全部导出函数 extern "system"（x86 = stdcall）。

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, RegisterClassExW, WNDCLASSEXW,
};

/// HIMC（输入上下文句柄）。跨线程存储用 usize。
pub type HIMC = *mut c_void;

// ---- imm32 强制导入（老式 IME 惯例：IME DLL 依赖 imm32，同 weasel.ime）----

#[link(name = "imm32")]
unsafe extern "system" {
    fn ImmGetOpenStatus(himc: HIMC) -> i32;
}

// ---- IMM 常量（MSDN IMM32 头文件）----
const IME_SYSINFO_WINLOGON: u32 = 0x0002;
const IME_PROP_UNICODE: u32 = 0x0000_0080;
const IME_PROP_SPECIAL_UI: u32 = 0x0002_0000;
const IME_CMODE_NATIVE: u32 = 0x0002;
const IME_CMODE_FULLSHAPE: u32 = 0x0100;
const IME_SMODE_NONE: u32 = 0x0000;
const UI_CAP_2700: u32 = 0x0000_0010;
const SELECT_CAP_CONVERSION: u32 = 0x0000_0001;
const CS_IME: u32 = 0x0001;

/// IMEINFO：ImeInquire 输出（8 个 DWORD，MSDN 布局）。
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct IMEINFO {
    pub dw_private_data_size: u32,
    pub fdw_property: u32,
    pub fdw_conversion_caps: u32,
    pub fdw_sentence_caps: u32,
    pub fdw_ui_caps: u32,
    pub fdw_scs_caps: u32,
    pub fdw_select_caps: u32,
    pub fdw_ignore: u32,
}

// ---- 日志（%TEMP%\iuv-ime.log，与 TSF 日志分离）----

fn log_line(msg: &str) {
    use std::io::Write;
    let path = std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .ok()
        .map(|d| std::path::PathBuf::from(d).join("iuv-ime.log"));
    if let Some(p) = path {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{msg}");
        }
    }
}

// ---- UI 类（CS_IME，供 IMM 系统发 WM_IME_* 到输入法窗口）----

const UI_CLASS: &[u16] = &[
    'I' as u16, 'u' as u16, 'v' as u16, 'U' as u16, 'I' as u16, 'C' as u16, 'l' as u16,
    'a' as u16, 's' as u16, 's' as u16, 0,
];

static UI_CLASS_REGISTERED: Once = Once::new();
static MODULE_INSTANCE: OnceLock<usize> = OnceLock::new();

/// 注册 UI 窗口类（等价 DllMain 的 RegisterUIClass；Rust 惰性注册，首次导出调用时触发）。
fn ensure_ui_class() {
    UI_CLASS_REGISTERED.call_once(|| unsafe {
        let hinst = *MODULE_INSTANCE.get_or_init(|| {
            let mut h = windows::Win32::Foundation::HMODULE::default();
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                windows::core::PCWSTR(ImeInquire as *const c_void as *const u16),
                &mut h,
            )
            .ok();
            h.0 as usize
        }) as isize;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(CS_IME),
            lpfnWndProc: Some(ui_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 2 * std::mem::size_of::<isize>() as i32,
            hInstance: HINSTANCE(hinst as *mut c_void),
            hIcon: Default::default(),
            hCursor: Default::default(),
            hbrBackground: Default::default(),
            lpszMenuName: Default::default(),
            lpszClassName: windows::core::PCWSTR(UI_CLASS.as_ptr()),
            hIconSm: Default::default(),
        };
        let ret = RegisterClassExW(&wc);
        if ret == 0 {
            log_line("UI 类注册失败");
        } else {
            log_line("UI 类注册 OK");
        }
    });
}

unsafe extern "system" fn ui_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    log_line(&format!("UIWndProc: msg=0x{msg:04X}"));
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

// ---- 实例管理（HIMC → 输入法实例；M1 仅记录存在性）----

static INSTANCES: OnceLock<Mutex<HashMap<usize, u64>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn instances() -> &'static Mutex<HashMap<usize, u64>> {
    INSTANCES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn instance_enter(himc: HIMC) -> u64 {
    ensure_ui_class();
    let mut map = instances().lock().unwrap();
    *map.entry(himc as usize).or_insert_with(|| {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        log_line(&format!("新建实例 #{id}: HIMC={himc:p}"));
        id
    })
}

fn instance_leave(himc: HIMC) {
    let mut map = instances().lock().unwrap();
    if let Some(id) = map.remove(&(himc as usize)) {
        log_line(&format!("销毁实例 #{id}: HIMC={himc:p}"));
    }
}

// ---- IMM 导出函数（weasel.def 同款 15 个）----

/// ImeInquire：报告输入法能力 + UI 类名。
#[no_mangle]
pub unsafe extern "system" fn ImeInquire(
    lp_ime_info: *mut IMEINFO,
    lpsz_ui_class: *mut u16,
    dw_system_info_flags: u32,
) -> i32 {
    log_line(&format!(
        "ImeInquire: flags=0x{dw_system_info_flags:08X} winlogon={}",
        dw_system_info_flags & IME_SYSINFO_WINLOGON != 0
    ));
    ensure_ui_class();
    if lp_ime_info.is_null() || lpsz_ui_class.is_null() {
        return 0;
    }
    (*lp_ime_info).dw_private_data_size = 0;
    (*lp_ime_info).fdw_property = IME_PROP_UNICODE | IME_PROP_SPECIAL_UI;
    (*lp_ime_info).fdw_conversion_caps = IME_CMODE_FULLSHAPE | IME_CMODE_NATIVE;
    (*lp_ime_info).fdw_sentence_caps = IME_SMODE_NONE;
    (*lp_ime_info).fdw_ui_caps = UI_CAP_2700;
    (*lp_ime_info).fdw_scs_caps = 0;
    (*lp_ime_info).fdw_select_caps = SELECT_CAP_CONVERSION;
    // 拷贝 UI 类名（含 null 结尾）
    let mut i = 0;
    while i < UI_CLASS.len() {
        *lpsz_ui_class.add(i) = UI_CLASS[i];
        i += 1;
    }
    1
}

/// ImeProcessKey：是否消费该键。M1 不消费（返回 FALSE），仅记录。
#[no_mangle]
pub unsafe extern "system" fn ImeProcessKey(
    himc: HIMC,
    v_key: u32,
    _l_key_data: LPARAM,
    _lpb_key_state: *const u8,
) -> i32 {
    instance_enter(himc);
    log_line(&format!("ImeProcessKey: vKey=0x{v_key:02X} HIMC={himc:p}（M1 不消费）"));
    0
}

/// ImeSelect：输入法被选中/取消。
#[no_mangle]
pub unsafe extern "system" fn ImeSelect(himc: HIMC, f_select: i32) -> i32 {
    // 真实调用 imm32 函数：强制保留 imm32.dll 导入（同 weasel.ime 依赖），
    // 返回值无业务意义（M1 stub 不查打开状态）。
    let _ = unsafe { ImmGetOpenStatus(himc) };
    log_line(&format!("ImeSelect: fSelect={f_select} HIMC={himc:p}"));
    if f_select != 0 {
        instance_enter(himc);
    } else {
        instance_leave(himc);
    }
    1
}

/// ImeSetActiveContext：焦点进入/离开。
#[no_mangle]
pub unsafe extern "system" fn ImeSetActiveContext(himc: HIMC, f_focus: i32) -> i32 {
    instance_enter(himc);
    log_line(&format!("ImeSetActiveContext: fFocus={f_focus} HIMC={himc:p}"));
    1
}

// ---- 其余 11 个 stub（M1 不实现）----

#[no_mangle]
pub unsafe extern "system" fn ImeConversionList(
    _himc: HIMC,
    _lp_source: *const u16,
    _lp_cand_list: *mut c_void,
    _dw_buf_len: u32,
    _u_flag: u32,
) -> u32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn ImeConfigure(
    _hkl: *mut c_void,
    _hwnd: HWND,
    _dw_mode: u32,
    _lp_data: *mut c_void,
) -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn ImeDestroy(_u_force: u32) -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn ImeEscape(
    _himc: HIMC,
    _u_sub_func: u32,
    _lp_data: *mut c_void,
) -> isize {
    1
}

#[no_mangle]
pub unsafe extern "system" fn ImeToAsciiEx(
    _u_vkey: u32,
    _u_scan_code: u32,
    _lpb_key_state: *const u8,
    _lpdw_trans_key: *mut u32,
    _fu_state: u32,
    _himc: HIMC,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn NotifyIME(
    _himc: HIMC,
    _dw_action: u32,
    _dw_index: u32,
    _dw_value: u32,
) -> i32 {
    1
}

#[no_mangle]
pub unsafe extern "system" fn ImeRegisterWord(
    _lp_read: *const u16,
    _dw: u32,
    _lp_str: *const u16,
) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn ImeUnregisterWord(
    _lp_read: *const u16,
    _dw: u32,
    _lp_str: *const u16,
) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn ImeGetRegisterWordStyle(
    _n_item: u32,
    _lp: *mut c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn ImeEnumRegisterWord(
    _lpfn: *mut c_void,
    _lp_read: *const u16,
    _dw: u32,
    _lp_str: *const u16,
    _lp_data: *mut c_void,
) -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "system" fn ImeSetCompositionString(
    _himc: HIMC,
    _dw_index: u32,
    _lp_comp: *const c_void,
    _dw_comp: u32,
    _lp_read: *const c_void,
    _dw_read: u32,
) -> i32 {
    0
}
