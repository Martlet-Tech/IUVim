//! iuv-tsf：cdylib COM/TSF 管线 + GDI 候选窗。
//! W0 冻结件：ui/mod.rs、registration.rs 常量；其余 Agent D/E W1 实现。
//!
//! COM 导出：DllGetClassObject / DllCanUnloadNow / DllRegisterServer / DllUnregisterServer。

pub mod composition;
pub mod ctl;
pub mod daemon_client;
pub mod langbar;
pub mod log;
pub mod registration;
pub mod session_bridge;
pub mod ui;
pub mod ui_element;

pub(crate) mod com;

pub use ui::{effect_to_snapshot, CaretRect, CandidateUi, UiSnapshot};

use std::ffi::c_void;

use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, S_FALSE, S_OK};
use windows_core::{GUID, HRESULT, Interface};

/// DllGetClassObject：文本服务的 class factory（契约 13 任务书 §3.2）。
///
/// # Safety
///
/// 标准 COM 导出约定：rclsid/riid 非空且指向有效 GUID；ppv 非空且指向 COM 运行时管理的
/// 输出指针槽。调用方（COM 运行时）保证这些约束。
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return windows_core::imp::E_INVALIDARG;
        }
        // 只提供文本服务这一个类对象。
        let clsid_text_service = registration::clsid();
        if *rclsid != clsid_text_service {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory = com::class_factory::ClassFactory;
        let unknown: windows_core::IUnknown = factory.into();
        unknown.query(riid, ppv)
    }
}

/// DllCanUnloadNow：无活动对象（实例/工厂引用）且引擎后台加载线程已结束时才允许卸载
/// （加载线程运行中访问 DLL 代码，卸载会导致宿主进程崩溃）。
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if com::class_factory::active_count() == 0 && !com::text_service::engine_loading() {
        S_OK
    } else {
        S_FALSE
    }
}
