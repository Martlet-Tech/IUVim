//! ULW（UpdateLayeredWindow）呈现共享模块（32-status-toolbar.md §6.4 从 iuv-tsf 抽取）。
//!
//! daemon 工具栏窗口与 TSF 候选窗/自绘菜单窗口复用。
//!
//! 机制（M4 定稿路线，见 `19-m4-cross-render.md`）：
//! 1. iuv-ui 软件光栅 premultiplied BGRA `Surface`
//! 2. 尺寸变化 → 重建 32bpp 自顶向下 DIB section（内存序 BGRA 与 Surface 一致，直拷）
//! 3. `UpdateLayeredWindow(hwnd, 桌面 DC, &pt{窗口位置}, &size{窗口尺寸}, hdc_src(DIB),
//!    &pt{0,0}, 0, &blend{AC_SRC_OVER, 255, AC_SRC_ALPHA}, ULW_ALPHA)`
//!    ——一次调用同时定位 + 定尺寸 + per-pixel alpha 合成（DWM）。
//!
//! 窗口必须带 `WS_EX_LAYERED` 样式（CreateWindowExW 时指定）且由调用线程创建/使用；
//! 全部方法不返回错误传播：失败记日志（调用方传入 `log` 前缀）并静默降级，绝不 panic。

use std::mem::size_of;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, BI_RGB, AC_SRC_ALPHA, AC_SRC_OVER,
    BLENDFUNCTION, HBITMAP, HDC, RGBQUAD,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};

/// ULW 呈现缓存：内存 DC + 32bpp 自顶向下 DIB section（bits 由 GDI 分配）。
/// 尺寸变化时自动重建；Drop 释放（DC 先删，DIB 对象后删）。
pub struct UlwSurface {
    hdc_src: HDC,
    dib: HBITMAP,
    bits: *mut u8,
    w: u32,
    h: u32,
}

// SAFETY: UlwSurface 仅在创建线程使用（TSF 回调线程 / daemon 工具条线程），
// bits 指针不跨线程传递。
unsafe impl Send for UlwSurface {}

impl UlwSurface {
    /// 空缓存（首次 upload 时按 Surface 尺寸懒建）。
    pub fn new() -> Self {
        UlwSurface {
            hdc_src: HDC::default(),
            dib: HBITMAP::default(),
            bits: std::ptr::null_mut(),
            w: 0,
            h: 0,
        }
    }

    /// 上屏：确保 DIB 匹配 surf 尺寸 → 拷贝 premultiplied BGRA → UpdateLayeredWindow
    /// （一次调用同时定位 ptDst + 定尺寸 psize + per-pixel alpha 合成）。
    /// `log_prefix`：日志前缀（如 "[candwin]" / "[menuwin]" / "[toolbar]"），失败记日志不 panic。
    pub fn upload(
        &mut self,
        hwnd: HWND,
        surf: &iuv_ui::Surface,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        log_prefix: &str,
    ) -> bool {
        if hwnd.is_invalid() || w <= 0 || h <= 0 {
            return false;
        }
        if !self.ensure_dib(surf, log_prefix) {
            return false;
        }
        // 像素直拷（DIB 内存序 BGRA 与 iuv-ui Surface 一致，无 stride 填充）
        // SAFETY: bits 由 ensure_dib 以 surf 尺寸创建，拷贝长度 = surf 全量，不越界。
        unsafe {
            std::ptr::copy_nonoverlapping(
                surf.pixels.as_ptr(),
                self.bits,
                (surf.w as usize) * (surf.h as usize) * 4,
            );
        }
        // SAFETY: hdcDst = 桌面 DC（DWM 合成目标）；ULW_ALPHA + AC_SRC_ALPHA =
        // per-pixel premultiplied 合成；ptDst 屏幕坐标定位；psize 窗口尺寸（ULW 同步调整）。
        let dst = POINT { x, y };
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let hdc_dst = unsafe { GetDC(None) };
        if hdc_dst.is_invalid() {
            crate::logger::log_line(&format!("{log_prefix} GetDC(桌面) 失败"));
            return false;
        }
        let r = unsafe {
            UpdateLayeredWindow(
                hwnd,
                Some(hdc_dst),
                Some(&dst),
                Some(&size),
                Some(self.hdc_src),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        };
        // SAFETY: GetDC/ReleaseDC 配对
        let _ = unsafe { ReleaseDC(None, hdc_dst) };
        if r.is_err() {
            crate::logger::log_line(&format!(
                "{log_prefix} UpdateLayeredWindow 失败：{:?}",
                r.unwrap_err()
            ));
            return false;
        }
        true
    }

    /// 确保 DIB section 匹配 surf 尺寸（变化重建：内存 DC + 32bpp 自顶向下 DIB，
    /// bits 直拷 premultiplied BGRA）。失败返回 false（记日志，不 panic）。
    fn ensure_dib(&mut self, surf: &iuv_ui::Surface, log_prefix: &str) -> bool {
        if self.w == surf.w && self.h == surf.h && !self.dib.is_invalid() {
            return true;
        }
        // 释放旧 DIB/DC（先删 DC——选中对象不受影响；再删 DIB 对象）。
        if !self.dib.is_invalid() {
            // SAFETY: DeleteDC 释放内存 DC；DIB 对象随后独立 DeleteObject（DC 已不引用）。
            unsafe {
                let _ = DeleteDC(self.hdc_src);
                let _ = DeleteObject(self.dib.into());
            }
            self.dib = HBITMAP::default();
            self.hdc_src = HDC::default();
            self.bits = std::ptr::null_mut();
        }
        // 新建内存 DC。
        // SAFETY: CreateCompatibleDC(None) 以桌面 DC 为模板创建内存 DC。
        let hdc = unsafe { CreateCompatibleDC(None) };
        if hdc.is_invalid() {
            crate::logger::log_line(&format!("{log_prefix} CreateCompatibleDC 失败"));
            return false;
        }
        // 32bpp 自顶向下（biHeight 负数）；内存序 = BGRA little-endian，与 iuv-ui 一致。
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: surf.w as i32,
                biHeight: -(surf.h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default(); 1],
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: bmi 与 bits 输出在调用期间有效；DIB_RGB_COLORS 平台调色板；无文件映射。
        let dib = match unsafe {
            CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
        } {
            Ok(d) => d,
            Err(e) => {
                crate::logger::log_line(&format!("{log_prefix} CreateDIBSection 失败：{e:?}"));
                // SAFETY: DC 未选中任何对象，可立即删除。
                unsafe {
                    let _ = DeleteDC(hdc);
                }
                return false;
            }
        };
        if bits.is_null() {
            crate::logger::log_line(&format!("{log_prefix} CreateDIBSection 返回空位图指针"));
            // SAFETY: DIB 成功但无像素指针（理论不发生）：释放后降级。
            unsafe {
                let _ = DeleteObject(dib.into());
                let _ = DeleteDC(hdc);
            }
            return false;
        }
        // SAFETY: 选中 DIB 到内存 DC（ULW 的 hdcSrc 数据源）。
        unsafe {
            let _ = SelectObject(hdc, dib.into());
        }
        self.hdc_src = hdc;
        self.dib = dib;
        self.bits = bits as *mut u8;
        self.w = surf.w;
        self.h = surf.h;
        true
    }
}

impl Drop for UlwSurface {
    fn drop(&mut self) {
        if !self.dib.is_invalid() {
            // SAFETY: 同 ensure_dib 释放顺序（DC 已不引用 DIB）。
            unsafe {
                let _ = DeleteDC(self.hdc_src);
                let _ = DeleteObject(self.dib.into());
            }
            self.dib = HBITMAP::default();
        }
    }
}
