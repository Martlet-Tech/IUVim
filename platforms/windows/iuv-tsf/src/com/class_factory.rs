//! IClassFactory 实现。契约 13 任务书 §3.2。
//! 【Agent D】W1 实现。

use windows::Win32::Foundation::CLASS_E_NOAGGREGATION;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows_core::{implement, BOOL, Interface, Result, GUID, IUnknown};

use super::text_service::TextService;

/// 全局活动对象计数（DllCanUnloadNow 用）：>0 时 DLL 不可卸载。
pub(crate) fn active_count() -> u32 {
    super::text_service::instance_count()
}

/// 构造 TextService 实例，返回 IUnknown 指针（CreateInstance 用）。
pub(crate) fn create_text_service() -> Result<IUnknown> {
    let service = TextService::new();
    // implement 宏生成 From<TextService> for ITfTextInputProcessorEx；
    // 接口继承链提供到 IUnknown 的转换。
    let iface = windows::Win32::UI::TextServices::ITfTextInputProcessorEx::from(service);
    Ok(iface.into())
}

/// DllGetClassObject 返回的 class factory 对象。
#[implement(IClassFactory)]
pub(crate) struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    /// 创建文本服务实例；TSF 用 IID_ITfTextInputProcessor 或 IID_ITfTextInputProcessorEx 查询。
    fn CreateInstance(
        &self,
        punkouter: windows_core::Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        // 聚合不支持：TSF 从不聚合文本服务。
        if !punkouter.is_null() {
            return Err(windows_core::Error::from_hresult(CLASS_E_NOAGGREGATION));
        }
        let service = create_text_service()?;
        // SAFETY: riid/ppvobject 由 COM 运行时保证有效；query 内部做标准 QueryInterface。
        let hr = unsafe { service.query(riid, ppvobject) };
        if hr.is_err() {
            return Err(windows_core::Error::from_hresult(hr));
        }
        Ok(())
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}
