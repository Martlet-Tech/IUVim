//! 注册常量与注册逻辑。W0 写入常量（冻结）；注册实现由 Agent D 完成。
//! 契约 01-contract.md §5.1 与 13 任务书 §3.1。

use windows::Win32::Foundation::{E_FAIL, HMODULE, S_OK};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, ITfCategoryMgr,
    ITfInputProcessorProfiles, GUID_TFCAT_CATEGORY_OF_TIP, GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
    GUID_TFCAT_TIPCAP_COMLESS, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED, GUID_TFCAT_TIP_KEYBOARD,
};
use windows_core::{Result, GUID, HRESULT};
use windows_registry::CLASSES_ROOT;

pub const CLSID_TEXT_SERVICE: &str = "{C69735F1-BAB1-458B-89FC-099ABA877ECB}";
const PROFILE_GUID: &str = "{799E00DD-64C2-4280-AC48-D379A9ABC5BE}";
pub const LANGID_ZH_CN: u16 = 0x0804;
pub const PROFILE_DESCRIPTION: &str = "IUV 输入法";
pub const DICT_FILENAME: &str = "iuv.imedic"; // 位于 %LOCALAPPDATA%\iuv\

/// 注册表根键名（HKCR 下）。
const CLSID_REG_KEY: &str = "CLSID\\{C69735F1-BAB1-458B-89FC-099ABA877ECB}";

/// 契约常量是带花括号的字符串（如 `{C69735F1-...}`）；windows-core 的 GUID::try_from
/// 不接受花括号，这里剥掉后解析。常量值冻结不动。
fn parse_guid(s: &str) -> GUID {
    let stripped = s.trim().trim_start_matches('{').trim_end_matches('}');
    GUID::try_from(stripped).expect("GUID 常量必须是合法 GUID")
}

pub(crate) fn clsid() -> GUID {
    parse_guid(CLSID_TEXT_SERVICE)
}

fn profile_guid() -> GUID {
    parse_guid(PROFILE_GUID)
}

/// 当前 DLL 的完整路径（regsvr32 场景即本文件）。
/// 从 DllRegisterServer 自身地址反查模块句柄，避免取到宿主进程 exe 的路径。
/// pub(crate)：daemon_client 自启用它解析同目录 iuv-daemon.exe。
pub(crate) fn dll_path() -> String {
    use std::os::raw::c_void;
    use windows_core::PCWSTR;
    // SAFETY: GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS 把 lpModuleName 解释为函数地址，
    // 指向本 DLL 内的导出函数；函数在本进程存活期间有效。
    let mut module = HMODULE::default();
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            PCWSTR(DllRegisterServer as *const c_void as *const u16),
            &mut module,
        )
    };
    if ok.is_err() || module.0.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 1024];
    // SAFETY: 查询 DLL 自身路径，缓冲 1024 宽足够；返回实际长度。
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) };
    if len == 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

/// HKCR\CLSID\{...}：InprocServer32 = 本 DLL，ThreadingModel = Apartment。
fn write_clsid_registry(dll: &str) -> windows_registry::Result<()> {
    let key = CLASSES_ROOT.create(CLSID_REG_KEY)?;
    key.set_string("", PROFILE_DESCRIPTION)?;
    let inproc = key.create("InprocServer32")?;
    inproc.set_string("", dll)?;
    inproc.set_string("ThreadingModel", "Apartment")?;
    Ok(())
}

/// 通过 TSF 管理器注册文本服务（契约 13 任务书 §3.1 第 2 步）。
fn register_with_tsf(dll: &str) -> Result<()> {
    // SAFETY: 标准 COM 初始化；随后在同一 STA 上使用。
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        crate::log::log_line(&format!("CoInitializeEx 失败：{hr:?}"));
    }
    let _guard = CoInitGuard(hr.is_ok());

    let profiles: ITfInputProcessorProfiles =
        // SAFETY: CoCreateInstance 由系统解析注册表创建 TSF 对象；泛型返回强类型接口。
        match unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) } {
            Ok(p) => p,
            Err(e) => {
                crate::log::log_line(&format!("CoCreateInstance(InputProcessorProfiles) 失败：{e:?}"));
                return Err(e);
            }
        };
    crate::log::log_line("TSF profiles 对象创建 OK");
    // SAFETY: TSF 注册 API，参数均为本地栈上对象地址，调用期间存活。
    crate::log::log_line(&format!("before Register：clsid={CLSID_TEXT_SERVICE}"));
    unsafe { profiles.Register(&clsid()) }.map_err(|e| {
        crate::log::log_line(&format!("profiles.Register 失败：{e:?}"));
        e
    })?;
    crate::log::log_line("profiles.Register OK");
    // TSF 按 C 字符串语义读取 desc/icon（需 null 结尾），长度由 windows-rs 从切片推导。
    let mut desc: Vec<u16> = PROFILE_DESCRIPTION.encode_utf16().collect();
    desc.push(0);
    let mut icon: Vec<u16> = dll.encode_utf16().collect();
    icon.push(0);
    // SAFETY: 同上；langid 与 GUID 常量由契约冻结。
    // ulIconIndex 必须用非负索引（0 = DLL 组图标第一个 = icon.ico/main logo）：
    // 传 u32::MAX(-1) 时系统输入指示器按索引提取图标失败，回退显示语言名"简体"。
    unsafe {
        profiles.AddLanguageProfile(&clsid(), LANGID_ZH_CN, &profile_guid(), &desc, &icon, 0)
    }
    .map_err(|e| {
        crate::log::log_line(&format!("AddLanguageProfile 失败：{e:?}"));
        e
    })?;
    crate::log::log_line("AddLanguageProfile OK");

    let category_mgr: ITfCategoryMgr =
        // SAFETY: 同上，CoCreateInstance 创建 CategoryMgr。
        match unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) } {
            Ok(c) => c,
            Err(e) => {
                crate::log::log_line(&format!("CoCreateInstance(CategoryMgr) 失败：{e:?}"));
                return Err(e);
            }
        };
    // SAFETY: 注册键盘 TIP 类别。
    unsafe { category_mgr.RegisterCategory(&clsid(), &GUID_TFCAT_TIP_KEYBOARD, &clsid()) }
        .map_err(|e| {
            crate::log::log_line(&format!("RegisterCategory 失败：{e:?}"));
            e
        })?;
    crate::log::log_line("RegisterCategory OK");
    // 沉浸式支持（TSF 3.0 客户端如 Windows Terminal / UWP 只加载声明了该类别的 TIP）
    unsafe {
        category_mgr.RegisterCategory(&clsid(), &GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, &clsid())
    }
    .map_err(|e| {
        crate::log::log_line(&format!("RegisterCategory(IMMERSIVESUPPORT) 失败：{e:?}"));
        e
    })?;
    crate::log::log_line("RegisterCategory(IMMERSIVESUPPORT) OK");
    // 输入模式 compartment 支持（老式/IMM 场景兼容——同微软拼音/老 Rime 的注册，
    // 缺此类别时老游戏（WoW 1.12 等）不激活本 TIP）
    unsafe {
        category_mgr.RegisterCategory(&clsid(), &GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT, &clsid())
    }
    .map_err(|e| {
        crate::log::log_line(&format!(
            "RegisterCategory(INPUTMODECOMPARTMENT) 失败：{e:?}"
        ));
        e
    })?;
    crate::log::log_line("RegisterCategory(INPUTMODECOMPARTMENT) OK");
    // 系统托盘/语言栏兼容
    unsafe { category_mgr.RegisterCategory(&clsid(), &GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT, &clsid()) }
        .map_err(|e| {
            crate::log::log_line(&format!("RegisterCategory(SYSTRAYSUPPORT) 失败：{e:?}"));
            e
        })?;
    crate::log::log_line("RegisterCategory(SYSTRAYSUPPORT) OK");
    // ---- 剩余 4 类别（2026-08-16 补齐：原为手动注册表，WoW 激活/候选 UI 元素依赖，见 63c6833）----
    // 显示属性提供者（系统/应用经 ITfDisplayAttributeProvider 查询预编辑属性）。
    unsafe {
        category_mgr.RegisterCategory(&clsid(), &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, &clsid())
    }
    .map_err(|e| {
        crate::log::log_line(&format!(
            "RegisterCategory(DISPLAYATTRIBUTEPROVIDER) 失败：{e:?}"
        ));
        e
    })?;
    crate::log::log_line("RegisterCategory(DISPLAYATTRIBUTEPROVIDER) OK");
    // COMLESS：老式/IMM 场景激活的关键类别（对齐 QQ——8 类别中主嫌疑，
    // 缺此类别时 WoW 1.12 等老进程不激活本 TIP）。
    unsafe { category_mgr.RegisterCategory(&clsid(), &GUID_TFCAT_TIPCAP_COMLESS, &clsid()) }
        .map_err(|e| {
            crate::log::log_line(&format!("RegisterCategory(COMLESS) 失败：{e:?}"));
            e
        })?;
    crate::log::log_line("RegisterCategory(COMLESS) OK");
    // UI 元素启用：TSF 3.0 候选 UI 元素（ITfCandidateListUIElement）被系统消费的前提。
    unsafe {
        category_mgr.RegisterCategory(&clsid(), &GUID_TFCAT_TIPCAP_UIELEMENTENABLED, &clsid())
    }
    .map_err(|e| {
        crate::log::log_line(&format!("RegisterCategory(UIELEMENTENABLED) 失败：{e:?}"));
        e
    })?;
    crate::log::log_line("RegisterCategory(UIELEMENTENABLED) OK");
    // 输入法类别（语言栏/输入指示器展示为中文输入法）。
    unsafe { category_mgr.RegisterCategory(&clsid(), &GUID_TFCAT_CATEGORY_OF_TIP, &clsid()) }
        .map_err(|e| {
            crate::log::log_line(&format!("RegisterCategory(CATEGORY_OF_TIP) 失败：{e:?}"));
            e
        })?;
    crate::log::log_line("RegisterCategory(CATEGORY_OF_TIP) OK");
    Ok(())
}

/// 注销：TSF 管理器反注册 + 删除注册表键。
fn unregister_with_tsf() -> Result<()> {
    // SAFETY: 标准 COM 初始化；随后在同一 STA 上使用。
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let _guard = CoInitGuard(hr.is_ok());

    let profiles: ITfInputProcessorProfiles =
        // SAFETY: CoCreateInstance 创建 TSF 对象。
        unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) }?;
    // SAFETY: 反向注销；参数存活于调用期间。
    unsafe {
        let _ = profiles.RemoveLanguageProfile(&clsid(), LANGID_ZH_CN, &profile_guid());
        let _ = profiles.Unregister(&clsid());
    }

    let category_mgr: ITfCategoryMgr =
        // SAFETY: CoCreateInstance 创建 CategoryMgr。
        unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) }?;
    // SAFETY: 反向注销类别。
    unsafe {
        let _ = category_mgr.UnregisterCategory(&clsid(), &GUID_TFCAT_TIP_KEYBOARD, &clsid());
        let _ = category_mgr.UnregisterCategory(
            &clsid(),
            &GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
            &clsid(),
        );
        let _ = category_mgr.UnregisterCategory(
            &clsid(),
            &GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
            &clsid(),
        );
        let _ =
            category_mgr.UnregisterCategory(&clsid(), &GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT, &clsid());
        let _ = category_mgr.UnregisterCategory(
            &clsid(),
            &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
            &clsid(),
        );
        let _ = category_mgr.UnregisterCategory(&clsid(), &GUID_TFCAT_TIPCAP_COMLESS, &clsid());
        let _ = category_mgr.UnregisterCategory(
            &clsid(),
            &GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
            &clsid(),
        );
        let _ = category_mgr.UnregisterCategory(&clsid(), &GUID_TFCAT_CATEGORY_OF_TIP, &clsid());
    }
    Ok(())
}

/// regsvr32 入口：注册。
#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    let dll = dll_path();
    if dll.is_empty() {
        crate::log::log_line("DllRegisterServer：无法取得 DLL 路径");
        return E_FAIL;
    }
    crate::log::log_line(&format!("DllRegisterServer 开始：{dll}"));
    let r = write_clsid_registry(&dll).map_err(|e| {
        crate::log::log_line(&format!("write_clsid_registry 失败：{e}"));
        e
    });
    let r = r.and_then(|_| {
        crate::log::log_line("write_clsid_registry OK，进入 TSF 注册");
        register_with_tsf(&dll).map_err(|e| {
            crate::log::log_line(&format!("register_with_tsf 失败：{e:?}"));
            e
        })
    });
    match r {
        Ok(()) => {
            crate::log::log_line(&format!("DllRegisterServer OK：{dll}"));
            S_OK
        }
        Err(e) => {
            crate::log::log_line(&format!("DllRegisterServer 失败：{e}"));
            e.into()
        }
    }
}

/// regsvr32 入口：注销。
#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    match unregister_with_tsf() {
        Ok(()) => {
            let _ = CLASSES_ROOT.remove_tree(CLSID_REG_KEY);
            crate::log::log_line("DllUnregisterServer OK");
            S_OK
        }
        Err(e) => {
            crate::log::log_line(&format!("DllUnregisterServer 失败：{e}"));
            e.into()
        }
    }
}

/// RAII：保证本线程 COM 单元与初始化配对释放（线程退出时也安全）。
struct CoInitGuard(bool);

impl Drop for CoInitGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: 与 CoInitializeEx 配对；本线程 STA 由 guard 生命周期内独占。
            unsafe { CoUninitialize() }
        }
    }
}
