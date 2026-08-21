//! composition 封装。契约 13 任务书 §3.5。
//! 【Agent D】W1 实现。
//!
//! 全部文档改动通过同步 edit session（TF_ES_SYNC | TF_ES_READWRITE）完成：
//! 预编辑文本 = StartComposition + ITfRange::SetText；上屏/取消 = SetText + EndComposition。

use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;
use std::rc::Rc;

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext,
    ITfContextComposition, ITfEditSession, ITfEditSession_Impl, TF_ANCHOR_END,
    TF_DEFAULT_SELECTION, TF_ES_READWRITE, TF_ES_SYNC, TF_SELECTION, TF_SELECTIONSTYLE,
};
use windows_core::{implement, BOOL, ComObject, Interface, Result};

use crate::log::log_line;
use crate::ui::CaretRect;

/// composition 封装：持有 ITfContext 与当前 composition 对象。
///
/// 所有方法取 `&self`：内部用 RefCell 保存可变状态，避免 edit session
/// 同步回调重入时的借用冲突。
pub struct Composition {
    context: ITfContext,
    client_id: u32,
    /// 共享 composition 槽：sink 在 OnCompositionTerminated（外部终止）时清空，
    /// 保证 set_text 不会复用一个已死的 composition（0x8000FFFF 问题）。
    comp: Rc<RefCell<Option<ITfComposition>>>,
    /// 是否曾被外部终止（sink 置位）：TextService 据此丢弃会话降级重建。
    terminated: Rc<Cell<bool>>,
}

impl Composition {
    pub fn new(context: ITfContext, client_id: u32) -> Self {
        Composition {
            context,
            client_id,
            comp: Rc::new(RefCell::new(None)),
            terminated: Rc::new(Cell::new(false)),
        }
    }

    /// 是否曾被外部终止（sink 置位）：调用方应丢弃会话降级（丢弃后重建 Composition 对象）。
    pub fn terminated(&self) -> bool {
        self.terminated.get()
    }

    /// 更新预编辑文本为 `text`（必要时先 StartComposition）。
    /// 返回新光标矩形（屏幕坐标）；失败返回 None 且保持原文本不变。
    pub fn set_text(&self, text: &str) -> Result<Option<CaretRect>> {
        let session = SetTextSession {
            context: self.context.clone(),
            existing: self.comp.borrow().clone(),
            comp_slot: self.comp.clone(),
            terminated: self.terminated.clone(),
            text: text.to_owned(),
            started: RefCell::new(None),
            caret: RefCell::new(None),
        };
        let com = ComObject::new(session);
        let sess: ITfEditSession = com.to_interface();
        self.request(&sess)?;
        if let Some(started) = com.started.borrow_mut().take() {
            *self.comp.borrow_mut() = Some(started);
        }
        let caret = *com.caret.borrow();
        Ok(caret)
    }

    /// 上屏 `text` 并结束 composition。
    pub fn commit(&self, text: &str) -> Result<()> {
        let session = EndSession {
            context: self.context.clone(),
            comp: self.comp.borrow().clone(),
            text: text.to_owned(),
        };
        let com = ComObject::new(session);
        let sess: ITfEditSession = com.to_interface();
        self.request(&sess)?;
        *self.comp.borrow_mut() = None;
        Ok(())
    }

    /// 取消：清空预编辑文本并结束 composition（文本不上屏）。
    pub fn cancel(&self) -> Result<()> {
        let session = EndSession {
            context: self.context.clone(),
            comp: self.comp.borrow().clone(),
            text: String::new(),
        };
        let com = ComObject::new(session);
        let sess: ITfEditSession = com.to_interface();
        self.request(&sess)?;
        *self.comp.borrow_mut() = None;
        Ok(())
    }

    /// 请求同步写 edit session；DoEditSession 的 HRESULT 一并检查。
    fn request(&self, sess: &ITfEditSession) -> Result<()> {
        // SAFETY: RequestEditSession 是标准 TSF 调用；sess 在本调用期间存活。
        let inner = unsafe {
            self.context
                .RequestEditSession(self.client_id, sess, TF_ES_SYNC | TF_ES_READWRITE)?
        };
        if inner.is_err() {
            log_line(&format!(
                "composition edit session 失败：code={:08X}",
                inner.0 as u32
            ));
        }
        // SAFETY: HRESULT 值类型，ok() 把失败码转为 Error。
        inner.ok()
    }
}

/// composition 销毁回调 sink：仿 Weasel/SampleIME 传真实实现（StartComposition 的 psink 不能为 null）。
/// 外部终止时清空共享 composition 槽，防 0x8000FFFF 复用死对象。
#[implement(ITfCompositionSink)]
struct CompositionSink {
    comp: Rc<RefCell<Option<ITfComposition>>>,
    terminated: Rc<Cell<bool>>,
}

impl ITfCompositionSink_Impl for CompositionSink_Impl {
    fn OnCompositionTerminated(
        &self,
        ecwrite: u32,
        _pcomposition: windows_core::Ref<ITfComposition>,
    ) -> Result<()> {
        log_line(&format!(
            "composition 终止通知（ec={ecwrite}）：清空槽+置终止标志，会话将降级重建"
        ));
        *self.comp.borrow_mut() = None;
        self.terminated.set(true);
        Ok(())
    }
}

/// 同步 edit session：更新预编辑文本（必要时新建 composition）并量取光标矩形。
#[implement(ITfEditSession)]
struct SetTextSession {
    context: ITfContext,
    /// 已有 composition（None = 首次输入，需要 StartComposition）。
    existing: Option<ITfComposition>,
    /// 共享 composition 槽（StartComposition 时写入，供 sink 终止时清空）。
    comp_slot: Rc<RefCell<Option<ITfComposition>>>,
    /// 终止标志（StartComposition 成功时复位）。
    terminated: Rc<Cell<bool>>,
    text: String,
    /// 输出：本次新建的 composition。
    started: RefCell<Option<ITfComposition>>,
    /// 输出：新光标矩形（屏幕坐标）。
    caret: RefCell<Option<CaretRect>>,
}

impl ITfEditSession_Impl for SetTextSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        let comp = match &self.existing {
            Some(c) => c.clone(),
            None => {
                // 取当前光标处 selection 的 range（TSF 标准做法，同 SampleIME）：
                // StartComposition 要求 range 由当前 context 在锁内 anchor 产生，
                // InsertTextAtSelection(QUERYONLY) 的模拟插入 range 在某些实现下无效（E_INVALIDARG）。
                let mut sel = [TF_SELECTION::default()];
                let mut fetched = 0u32;
                trace_step(
                    &format!("GetSelection(ec={ec}, TF_DEFAULT_SELECTION)"),
                    || unsafe {
                        self.context
                            .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)
                    },
                )?;
                if fetched == 0 || sel[0].range.is_none() {
                    log_line(&format!("GetSelection 无 selection（fetched={fetched}）"));
                    return Err(windows::Win32::Foundation::E_FAIL.into());
                }
                let range = sel[0]
                    .range
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| windows_core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
                // SAFETY: ITfContextComposition 为 ITfContext 的标准支持接口。
                let context_comp: ITfContextComposition = trace_step("cast ITfContextComposition", || {
                    self.context.cast()
                })?;
                // 仿 Weasel/SampleIME：psink 传真实 ITfCompositionSink（不能为 null）。
                let sink = ComObject::new(CompositionSink {
                    comp: self.comp_slot.clone(),
                    terminated: self.terminated.clone(),
                });
                let sink: ITfCompositionSink = sink.to_interface();
                let c = trace_step(&format!("StartComposition(ec={ec}, sink=Some)"), || unsafe {
                    context_comp.StartComposition(ec, &range, &sink)
                })?;
                *self.started.borrow_mut() = Some(c.clone());
                // 新建成功：清终止标志（旧 composition 的终止不影响新生命周期）。
                self.terminated.set(false);
                c
            }
        };
        // SAFETY: GetRange 返回本 composition 的有效范围，调用期间存活。
        let range = trace_step("comp.GetRange", || unsafe { comp.GetRange() })?;
        let wide: Vec<u16> = self.text.encode_utf16().collect();
        let wide_len = wide.len();
        let display: String = self.text.chars().take(32).collect();
        // SAFETY: SetText 替换整个 composition 文本（写入切片为 UTF-16 编码）。
        trace_step(&format!("range.SetText(ec={ec}, len={wide_len}, text={display:?})"), || {
            unsafe { range.SetText(ec, 0, &wide) }
        })?;
        // 仿 Weasel：把光标 range 折叠到组合文本末尾，并把该 range 设为当前 selection，
        // 否则光标仍停在原 selection 处（组合文本开头）。
        // SAFETY: 均为标准 TSF 调用，ec 为当前读写 cookie。
        trace_step("range.Collapse(TF_ANCHOR_END)", || unsafe {
            range.Collapse(ec, TF_ANCHOR_END)
        })?;
        let sel = [TF_SELECTION {
            range: ManuallyDrop::new(Some(range.clone())),
            style: TF_SELECTIONSTYLE::default(),
        }];
        trace_step(&format!("context.SetSelection(n={})", sel.len()), || unsafe {
            self.context.SetSelection(ec, &sel)
        })?;

        // 量取光标矩形（composition 文本的尾端，屏幕坐标）。
        // SAFETY: GetActiveView 由 TSF 保证在 edit session 内可调用。
        let view = match trace_step("context.GetActiveView", || unsafe { self.context.GetActiveView() }) {
            Ok(v) => v,
            Err(e) => {
                log_line(&format!("GetActiveView 失败：{e}，跳过光标量取"));
                return Ok(());
            }
        };
        let mut rc = RECT::default();
        let mut clipped = BOOL(0);
        // SAFETY: GetTextExt 由 TSF 保证在 edit session 内可调用；输出缓冲在调用前初始化。
        let ext = trace_step(
            &format!("view.GetTextExt(ec={ec})"),
            || unsafe { view.GetTextExt(ec, &range, &mut rc, &mut clipped) },
        );
        log_line(&format!(
            "[caret] GetTextExt：rc=({},{},{},{}) clipped={} err={:?}",
            rc.left, rc.top, rc.right, rc.bottom, clipped.0,
            ext.as_ref().err().map(|e| e.code())
        ));
        if ext.is_ok() && !clipped.as_bool() {
            // GetTextExt 返回屏幕坐标（MSDN：bounding box, in screen coordinates），
            // 不再做 ClientToScreen 转换（历史 bug：双重转换导致候选框偏移窗口原点）。
            if rc.left == 0 && rc.top == 0 && rc.right == 0 && rc.bottom == 0 {
                // MSDN：文档窗口最小化或文本不可见时返回 {0,0,0,0}。
                log_line("[caret] GetTextExt 返回全 0 矩形（文本不可见），跳过光标量取");
                return Ok(());
            }
            let rect = CaretRect {
                // y 用行顶（rc.top）：position_in_area 按"y=顶、h=行高"计算下方位置，
                // 若直接用 rc.bottom 会重复加一次行高，候选框被推下一行。
                x: rc.left,
                y: rc.top,
                w: rc.right - rc.left,
                h: rc.bottom - rc.top,
            };
            *self.caret.borrow_mut() = Some(rect);
        }
        Ok(())
    }
}

/// 分步调试日志：每个 TSF 调用包装一层，成功打 OK，失败打 HRESULT 并短路。
fn trace_step<T>(name: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match f() {
        Ok(v) => Ok(v),
        Err(e) => {
            log_line(&format!("do_edit_session: {name} 失败：{e:?}"));
            Err(e)
        }
    }
}

/// 同步 edit session：替换文本 + EndComposition（commit 上屏 / cancel 清空）。
#[implement(ITfEditSession)]
struct EndSession {
    context: ITfContext,
    comp: Option<ITfComposition>,
    text: String,
}

impl ITfEditSession_Impl for EndSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        let Some(comp) = &self.comp else {
            // 无 composition（理论上不会发生）：无操作，仍返回成功。
            log_line("end session：composition 缺失，跳过");
            return Ok(());
        };
        // SAFETY: GetRange 返回本 composition 的有效范围，调用期间存活。
        let range = trace_step("end: comp.GetRange", || unsafe { comp.GetRange() })?;
        let wide: Vec<u16> = self.text.encode_utf16().collect();
        // SAFETY: SetText 替换 composition 范围文本；空串 = 删除（cancel 语义）。
        trace_step(&format!("end: range.SetText(ec={ec}, len={})", wide.len()), || {
            unsafe { range.SetText(ec, 0, &wide) }
        })?;
        // 显式把选区折叠到文本尾端并设为当前 selection——「composition 结束后光标
        // 放哪」TSF 未定义、由应用自定：Word 会恢复自己记录的选区锚点（composition
        // 起点）→ 光标落回新上屏文字前面。weasel `_InsertText` 与微软官方血统的
        // Metasequoia `_AddCharAndFinalize` 同款收尾（"insertion point just past the
        // inserted text"），且与预编辑路径 SetTextSession 一致；cancel 空串删除后
        // 折叠回原点，语义同样正确。
        // SAFETY: Collapse/SetSelection 均为标准 TSF 调用，ec 为当前读写 cookie。
        trace_step("end: range.Collapse(TF_ANCHOR_END)", || unsafe {
            range.Collapse(ec, TF_ANCHOR_END)
        })?;
        let sel = [TF_SELECTION {
            range: ManuallyDrop::new(Some(range)),
            style: TF_SELECTIONSTYLE::default(),
        }];
        trace_step(&format!("end: context.SetSelection(n={})", sel.len()), || unsafe {
            self.context.SetSelection(ec, &sel)
        })?;
        // SAFETY: EndComposition 需要写 cookie，当前 edit session 为读写。
        trace_step("end: comp.EndComposition", || unsafe {
            comp.EndComposition(ec)
        })?;
        Ok(())
    }
}
