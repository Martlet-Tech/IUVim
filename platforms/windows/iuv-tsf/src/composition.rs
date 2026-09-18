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
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfContextComposition,
    ITfEditSession, ITfEditSession_Impl, TF_ANCHOR_END, TF_ANCHOR_START, TF_DEFAULT_SELECTION,
    TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC, TF_SELECTION, TF_SELECTIONSTYLE,
};
use windows_core::{implement, ComObject, Interface, Result, BOOL};

use crate::log::log_line;
use crate::ui::CaretRect;

/// 光标量取（`GetTextExt`）连续失败多少次后判定宿主不支持并停止尝试。
///
/// 取 3 而非 1：个别应用在文档尚未就绪时会短暂失败后恢复正常，首次失败即永久
/// 禁用会让这类宿主再也拿不到候选窗位置。代价是最初几次仍会尝试，但之后每键
/// 都省下一次跨进程调用与一条失败日志。
const CARET_PROBE_FAIL_LIMIT: u8 = 3;

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
    /// 光标量取（`GetTextExt`）连续失败计数：达 [`CARET_PROBE_FAIL_LIMIT`] 判定宿主
    /// 不支持并停止尝试。量取成功即复位（宿主可能只是文档未就绪时短暂失败）。
    /// 本对象每会话新建，标记随会话结束自然失效，无需清理。
    caret_probe_fails: Rc<Cell<u8>>,
}

impl Composition {
    pub fn new(context: ITfContext, client_id: u32) -> Self {
        Composition {
            context,
            client_id,
            comp: Rc::new(RefCell::new(None)),
            terminated: Rc::new(Cell::new(false)),
            caret_probe_fails: Rc::new(Cell::new(0)),
        }
    }

    /// 是否曾被外部终止（sink 置位）：调用方应丢弃会话降级（丢弃后重建 Composition 对象）。
    pub fn terminated(&self) -> bool {
        self.terminated.get()
    }

    /// 组合所在 context（布局跟随 sink 比对事件来源用）。
    pub(crate) fn context(&self) -> &ITfContext {
        &self.context
    }

    /// 更新预编辑文本为 `text`（必要时先 StartComposition），并返回新**锚点矩形**
    /// （composition 起点，屏幕坐标）；量取失败返回 None（调用方沿用旧锚点，文本照常更新）。
    ///
    /// 47 号治本：锚点量取**只有一条路径**——写会话结束后调用 [`Self::query_caret`]
    /// （只读会话，与布局跟随同一份代码）。写会话内直接 `GetTextExt` 拿到的是"刚 SetText
    /// 完、宿主布局尚未稳定"的**字形盒**（真机实测 h=21），而布局跟随稍后量到的是稳定后的
    /// **行盒**（h=23）；`position_in_area` 的下方偏移取 `caret.y + caret.h`，于是首键
    /// `show` 之后必然被 `move_to` 往下挪 2px——"候选栏出现瞬间下沉一下"。统一到只读路径后
    /// 两路矩形逐字段相同，下沉与去重失效一并消失（也顺带消掉了上一版"写会话内量出尾端"
    /// 导致的闪烁：那次是两路 x 差 316px 触发跳变藏窗）。
    pub fn set_text(&self, text: &str) -> Result<Option<CaretRect>> {
        let session = SetTextSession {
            context: self.context.clone(),
            existing: self.comp.borrow().clone(),
            comp_slot: self.comp.clone(),
            terminated: self.terminated.clone(),
            text: text.to_owned(),
            started: RefCell::new(None),
        };
        {
            let com = ComObject::new(session);
            let sess: ITfEditSession = com.to_interface();
            self.request(&sess)?;
            // 先落成语句级绑定：`borrow_mut()` 的临时量若跨 `if let` 存活，
            // `com` 会被判定为"still borrowed"而不能在本作用域内析构（E0597）。
            let started = com.started.borrow_mut().take();
            if let Some(started) = started {
                *self.comp.borrow_mut() = Some(started);
            }
            // com/sess 随手释放：锚点量取要发起**新的**只读 edit session，
            // 不带着上一个会话的对象继续（同一个 context 上不并存两个未结束的会话）。
        }
        let caret = self.query_caret();
        if let Some(r) = caret {
            log_line(&format!(
                "[caret] 锚点（composition 起点，写会话后只读量取）：x={} y={} w={} h={}",
                r.x, r.y, r.w, r.h
            ));
        }
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

    /// 只读量取当前锚点矩形（屏幕坐标；composition **起点**）。
    ///
    /// **唯一锚点量取路径**（47 号治本）：既供打字路径 [`Self::set_text`] 取首键定位锚点，
    /// 也供布局跟随（宿主窗口拖拽/缩放/滚动后重定位候选窗，锚点随文档平移）——两处同源
    /// 同口径，矩形逐字段一致，不会再出现"两路各量一个盒"造成的抖动/下沉。
    ///
    /// 失败（文档锁定/无 view/clipped/全零矩形）一律 None，调用方保持原位/沿用旧锚点。
    /// SYNC|READ：回调不在编辑锁内时同步完成；被锁即失败——正好跳过打字路径
    /// 自身触发布局变化时的重复量取。
    pub(crate) fn query_caret(&self) -> Option<CaretRect> {
        // 宿主不支持光标量取（连续失败达上限）：整体早退，不再为每次按键/布局事件
        // 发起一个注定失败的只读 edit session。返回 None → 调用方保持原位/沿用旧锚点。
        if self.caret_probe_fails.get() >= CARET_PROBE_FAIL_LIMIT {
            return None;
        }
        let comp = self.comp.borrow().clone()?;
        let session = RepositionSession {
            context: self.context.clone(),
            comp,
            caret_probe_fails: self.caret_probe_fails.clone(),
            caret: RefCell::new(None),
        };
        let com = ComObject::new(session);
        let sess: ITfEditSession = com.to_interface();
        // SAFETY: RequestEditSession 是标准 TSF 调用；sess 在本调用期间存活。
        if unsafe {
            self.context
                .RequestEditSession(self.client_id, &sess, TF_ES_SYNC | TF_ES_READ)
        }
        .is_err()
        {
            return None;
        }
        let caret = *com.caret.borrow();
        caret
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

/// 同步 edit session：更新预编辑文本（必要时新建 composition）。
///
/// 47 号起**不再在此量取锚点**：写会话内 `GetTextExt` 量到的是宿主布局尚未稳定的字形盒
/// （真机实测 h=21），与布局跟随量到的行盒（h=23）差一个高度，会让候选窗在出现瞬间下沉。
/// 量取统一移到写会话之后由 [`Composition::query_caret`] 完成（唯一路径）。
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
                trace_step("GetSelection(TF_DEFAULT_SELECTION)", || unsafe {
                    self.context
                        .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)
                })?;
                if fetched == 0 || sel[0].range.is_none() {
                    log_line(&format!("GetSelection 无 selection（fetched={fetched}）"));
                    return Err(windows::Win32::Foundation::E_FAIL.into());
                }
                let range = sel[0].range.as_ref().cloned().ok_or_else(|| {
                    windows_core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
                })?;
                // SAFETY: ITfContextComposition 为 ITfContext 的标准支持接口。
                let context_comp: ITfContextComposition =
                    trace_step("cast ITfContextComposition", || self.context.cast())?;
                // 仿 Weasel/SampleIME：psink 传真实 ITfCompositionSink（不能为 null）。
                let sink = ComObject::new(CompositionSink {
                    comp: self.comp_slot.clone(),
                    terminated: self.terminated.clone(),
                });
                let sink: ITfCompositionSink = sink.to_interface();
                let c = trace_step("StartComposition", || unsafe {
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
        // SAFETY: SetText 替换整个 composition 文本（写入切片为 UTF-16 编码）。
        // 注：原先会把预编辑文本前 32 字符拼进描述（每键一次 String 分配），
        // 描述改静态后已移除——失败时仍有 HRESULT 可定位，不缺排查手段。
        trace_step("range.SetText", || unsafe { range.SetText(ec, 0, &wide) })?;
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
        trace_step("context.SetSelection", || unsafe {
            self.context.SetSelection(ec, &sel)
        })?;

        // 选区收尾保持尾端不变（光标仍落在预编辑末尾，输入法正常行为）；锚点**不在这里量取**
        // ——写会话内 GetTextExt 拿到的是宿主尚未稳定的字形盒，与布局跟随的行盒不一致。
        // 锚点统一由写会话之后的 `Composition::query_caret` 量取（唯一路径），详见 `set_text`。

        Ok(())
    }
}

/// 分步调试日志：每个 TSF 调用包装一层，失败打 HRESULT 并短路。
///
/// `name` 必须是**静态**描述（不拼接动态内容）：本函数只在失败分支才格式化，
/// 调用点若预先 `format!` 出来，热路径就会为一条看不到（或本就不写）的日志
/// 白做字符串分配。
///
/// 消息带 `[edit]` 前缀：原先无前缀，而 `log.rs` 对无 tag 消息**恒放行**，
/// 导致 Electron 类宿主上每键一条失败日志无法通过配置关闭。
fn trace_step<T>(name: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match f() {
        Ok(v) => Ok(v),
        Err(e) => {
            log_line(&format!("[edit] do_edit_session: {name} 失败：{e:?}"));
            Err(e)
        }
    }
}

/// 同步只读 edit session：量取 composition **起点**（候选窗锚点）矩形（屏幕坐标）。
/// 只读不写：`query_caret` 专用（打字首键 show 与布局跟随**共用**这一条路径），
/// 绝不扰动应用文档。
#[implement(ITfEditSession)]
struct RepositionSession {
    context: ITfContext,
    comp: ITfComposition,
    /// 光标量取连续失败计数（与 Composition 共享）。
    caret_probe_fails: Rc<Cell<u8>>,
    /// 输出：锚点矩形（composition 起点；None = 量取失败/clipped/文本不可见）。
    caret: RefCell<Option<CaretRect>>,
}

impl ITfEditSession_Impl for RepositionSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        // SAFETY: GetRange 返回本 composition 的有效范围，调用期间存活。
        let range = trace_step("reposition: comp.GetRange", || unsafe {
            self.comp.GetRange()
        })?;
        // 与 SetTextSession 打字路径同锚点：**起点**折叠（候选窗锚定会话起点；47 号）。
        // 拖拽/滚动/缩放改变的是整个文档的布局，起点随之平移 → 窗口跟着平移；
        // 打字只移动尾端、起点不动 → 窗口原地不动（不抖动）。
        // SAFETY: 标准 TSF 调用，ec 为当前只读 cookie。
        trace_step("reposition: range.Collapse(TF_ANCHOR_START)", || unsafe {
            range.Collapse(ec, TF_ANCHOR_START)
        })?;
        // SAFETY: GetActiveView 由 TSF 保证在 edit session 内可调用。
        let view = trace_step("reposition: context.GetActiveView", || unsafe {
            self.context.GetActiveView()
        })?;
        let mut rc = RECT::default();
        let mut clipped = BOOL(0);
        // SAFETY: GetTextExt 由 TSF 保证在 edit session 内可调用；输出缓冲先初始化。
        // 失败计入共享计数（与打字路径同一份）：宿主不支持时 query_caret 整体早退。
        if let Err(e) = trace_step("reposition: view.GetTextExt(ec)", || unsafe {
            view.GetTextExt(ec, &range, &mut rc, &mut clipped)
        }) {
            let fails = self.caret_probe_fails.get().saturating_add(1);
            self.caret_probe_fails.set(fails);
            return Err(e);
        }
        self.caret_probe_fails.set(0);
        if clipped.as_bool() || (rc.left == 0 && rc.top == 0 && rc.right == 0 && rc.bottom == 0) {
            // MSDN：clipped 或全零 = 文本不可见（如最小化）：保持原位。
            return Ok(());
        }
        *self.caret.borrow_mut() = Some(CaretRect {
            x: rc.left,
            y: rc.top,
            w: rc.right - rc.left,
            h: rc.bottom - rc.top,
        });
        Ok(())
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
        trace_step("end: range.SetText", || unsafe {
            range.SetText(ec, 0, &wide)
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
        trace_step("end: context.SetSelection", || unsafe {
            self.context.SetSelection(ec, &sel)
        })?;
        // SAFETY: EndComposition 需要写 cookie，当前 edit session 为读写。
        trace_step("end: comp.EndComposition", || unsafe {
            comp.EndComposition(ec)
        })?;
        Ok(())
    }
}
