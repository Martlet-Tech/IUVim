//! TSF 候选 UI 元素：把候选数据暴露给系统（TSF→IMM 桥 / 应用 ITfUIElementSink）。
//! WoW 实验（wow-ime 分支）：桥消费候选元素 → 游戏发 IMN_OPENCANDIDATE →
//! ImmGetCandidateList 有数据 → 游戏内候选框（QQ 机制逆向）。
//!
//! ITfUIElementMgr 由 TSF manager 实现，text service 经
//! ITfThreadMgr::QueryInterface(IID_ITfUIElementMgr) 获取（微软文档原文）。
//! BeginUIElement 的 pbshow 返回系统判定：false = 系统/桥接管候选显示
//! （TIP 不应自绘——游戏内框场景）；true = TIP 自绘（notepad 场景维持现状）。
//! 实测（2026-08-16）：IMM 应用（WoW）pbshow=true（系统认为 TIP 自绘），但桥
//! **同时**把候选转给游戏（游戏也画）——所以自绘窗隐藏不能靠 pbshow，靠
//! ImmDetect（text_service.rs：GetTextExt 退化矩形 w/h<=2 连续 3 次 = IMM 客户端
//! → CandwinCandidateWindow::set_suppressed）。
//!
//! 桥对元素的数据消费（日志实证）：候选变化（Update）时拉 GetCount/GetString
//! （**全量**，全局索引）/GetPageIndex/GetCurrentPage/GetSelection；翻页时只拉
//! 元数据（字符串缓存不变）。
//!
//! 候选数据语义（CANDIDATELIST 文档对齐，2026-08-16 翻页消失修复的根因）：
//! - GetCount = 候选**总数**（全量）；GetString(uindex) = **全局索引**；
//! - GetSelection = **全局索引**（page*page_size+selected）——游戏翻页时校验
//!   dwSelection 是否落在 [dwPageStart, dwPageStart+dwPageSize) 内，页内索引
//!   导致页≠0 时候选栏被游戏关闭（页 0 正常、翻页消失、翻回页 0 恢复）。
//! - GetPageIndex = 每页起始索引数组（步进 page_size）；GetCurrentPage = 当前页。
//!
//! 游戏内候选栏的显示/翻页/高亮全部由游戏自己渲染（IMM 客户端模式）——本模块
//! 只提供数据，不画任何 UI；自绘窗（GDI）供 TSF 应用（notepad 等）使用。

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::E_INVALIDARG;
use windows::Win32::UI::TextServices::{
    ITfCandidateListUIElement, ITfCandidateListUIElementBehavior,
    ITfCandidateListUIElementBehavior_Impl, ITfCandidateListUIElement_Impl, ITfDocumentMgr,
    ITfThreadMgr, ITfUIElement, ITfUIElementMgr, ITfUIElement_Impl, TF_CLUIE_COUNT,
    TF_CLUIE_CURRENTPAGE, TF_CLUIE_PAGEINDEX, TF_CLUIE_SELECTION, TF_CLUIE_STRING,
};
use windows_core::{implement, ComObject, Interface, Result, BOOL, BSTR, GUID};

use crate::log::log_line;
use crate::ui::UiSnapshot;

/// 候选元素宿主（TextService 持有，同线程 Rc<RefCell>）：元素生命周期 + 关键点位日志。
pub(crate) struct CandidateElementHost {
    mgr: Option<ITfUIElementMgr>,
    /// 关联线程管理器（GetDocumentMgr 现场取焦点文档用；attach 时缓存）。
    thread_mgr: Option<ITfThreadMgr>,
    elem: Option<ComObject<CandidateElement>>,
    elem_id: u32,
    /// BeginUIElement 返回 pbshow：系统判定 TIP 是否应自绘（false = 系统/桥接管显示）。
    bshow: bool,
    /// mgr 不可用日志只记一次（防刷屏）。
    mgr_fail_logged: bool,
}

impl CandidateElementHost {
    pub(crate) fn new() -> Self {
        Self {
            mgr: None,
            thread_mgr: None,
            elem: None,
            elem_id: 0,
            bshow: true,
            mgr_fail_logged: false,
        }
    }

    /// Activate 时调用：QI ITfUIElementMgr（TSF manager 实现）。
    /// 失败仅记日志，不影响输入法主体（元素机制不可用 = 现状行为）。
    pub(crate) fn attach(&mut self, ptim: &ITfThreadMgr) {
        self.thread_mgr = Some(ptim.clone());
        match ptim.cast::<ITfUIElementMgr>() {
            Ok(mgr) => {
                self.mgr = Some(mgr);
                log_line("[uielem] ITfUIElementMgr QI 成功（候选 UI 元素机制激活）");
            }
            Err(e) => {
                self.mgr = None;
                log_line(&format!(
                    "[uielem] ITfUIElementMgr QI 失败：{e:?}（候选元素机制不可用，维持现状）"
                ));
            }
        }
    }

    /// 候选同步（dispatch 主路径）：候选非空 → Begin/Update；空 → End。
    /// 与自绘窗平行：跳变隐藏只影响自绘窗，元素跟随候选数据本身（游戏内框无跳变概念）。
    pub(crate) fn sync(&mut self, snap: &UiSnapshot) {
        let Some(mgr) = self.mgr.clone() else {
            if !self.mgr_fail_logged {
                self.mgr_fail_logged = true;
                log_line("[uielem] sync 跳过：ITfUIElementMgr 不可用");
            }
            return;
        };
        if snap.candidates.is_empty() {
            self.end();
            return;
        }
        let first = snap.candidates.first().map(String::as_str).unwrap_or("");
        match self.elem.as_ref() {
            // 首次出现候选：创建元素并 Begin。
            None => {
                let elem = ComObject::new(CandidateElement::new(self.thread_mgr.clone()));
                elem.as_ref().update_data(snap);
                let element: ITfUIElement = elem.to_interface();
                let mut bshow = BOOL(1);
                let mut id = 0u32;
                // SAFETY: 标准 TSF 调用；bshow/id 由 TSF manager 写入。
                match unsafe { mgr.BeginUIElement(&element, &mut bshow, &mut id) } {
                    Ok(()) => {
                        self.elem = Some(elem);
                        self.elem_id = id;
                        self.bshow = bshow.as_bool();
                        log_line(&format!(
                            "[uielem] BeginUIElement：id={} pbshow={} 候选{}个（{}…）",
                            id,
                            self.bshow,
                            snap.candidates.len(),
                            first
                        ));
                        if !self.bshow {
                            log_line(
                                "[uielem] pbshow=false：系统接管候选显示（游戏内框预期）；自绘窗暂保留（实验观察）",
                            );
                        }
                    }
                    Err(e) => log_line(&format!("[uielem] BeginUIElement 失败：{e:?}")),
                }
            }
            // 候选更新：刷新数据并 Update。
            Some(elem) => {
                elem.as_ref().update_data(snap);
                // SAFETY: 标准 TSF 调用；id 为 BeginUIElement 返回值。
                match unsafe { mgr.UpdateUIElement(self.elem_id) } {
                    Ok(()) => log_line(&format!(
                        "[uielem] UpdateUIElement：id={} 候选{}个（{}…）",
                        self.elem_id,
                        snap.candidates.len(),
                        first
                    )),
                    Err(e) => log_line(&format!("[uielem] UpdateUIElement 失败：{e:?}")),
                }
            }
        }
    }

    /// 结束元素（候选消失 / 会话结束 / Deactivate 兜底）。幂等。
    pub(crate) fn end(&mut self) {
        if self.elem.is_none() {
            return;
        }
        let Some(mgr) = self.mgr.clone() else {
            self.elem = None;
            return;
        };
        // SAFETY: 标准 TSF 调用；id 为 BeginUIElement 返回值。
        match unsafe { mgr.EndUIElement(self.elem_id) } {
            Ok(()) => log_line(&format!("[uielem] EndUIElement：id={}", self.elem_id)),
            Err(e) => log_line(&format!("[uielem] EndUIElement 失败：{e:?}")),
        }
        self.elem = None;
    }

    /// Deactivate 兜底清理（元素结束 + mgr 置空）。
    pub(crate) fn clear(&mut self) {
        self.end();
        self.mgr = None;
        self.thread_mgr = None;
    }
}

/// 候选列表 UI 元素（COM 对象）：桥/应用经 ITfUIElementMgr::GetUIElement 拉取
/// 候选数据（GetCount/GetString/GetSelection/分页），经 Behavior 接口控制。
#[implement(
    ITfUIElement,
    ITfCandidateListUIElement,
    ITfCandidateListUIElementBehavior
)]
struct CandidateElement {
    /// 关联线程管理器：GetDocumentMgr 现场取焦点文档（桥可能校验文档上下文）。
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    /// 全量候选文本（所有页，全局索引）。桥构造 IMM CANDIDATELIST 的完整数据源
    /// ——游戏翻页从全量切片（2026-08-16 实测：只给当前页 → 翻页后游戏内候选栏
    /// 消失，回第 0 页恢复；QQ 提供全量 → 翻页正常）。
    candidates: RefCell<Vec<String>>,
    selected: Cell<u32>,
    page: Cell<u32>,
    page_count: Cell<u32>,
    page_size: Cell<u32>,
    /// Show/IsShown 状态（系统/桥控制元素显隐语义，记录观察）。
    shown: Cell<bool>,
}

impl CandidateElement {
    fn new(thread_mgr: Option<ITfThreadMgr>) -> Self {
        Self {
            thread_mgr: RefCell::new(thread_mgr),
            candidates: RefCell::new(Vec::new()),
            selected: Cell::new(0),
            page: Cell::new(0),
            page_count: Cell::new(1),
            page_size: Cell::new(5),
            shown: Cell::new(true),
        }
    }

    /// 刷新候选数据（TextService 在 Begin/Update 前调用）：
    /// 全量候选（桥/游戏翻页数据源，dwCount）+ 全局选中（dwSelection 语义）+
    /// 当前页/页大小（dwPageStart/dwPageSize 数据源）。
    fn update_data(&self, snap: &UiSnapshot) {
        self.candidates.replace(snap.all_candidates.clone());
        let ps = snap.page.page_size.max(1);
        // dwSelection 为全局索引（CANDIDATELIST 文档语义："Index of the selected
        // candidate string"）——翻页后游戏校验 dwSelection 是否落在 [dwPageStart,
        // dwPageStart+dwPageSize) 内，页内索引导致页≠0 时候选栏被游戏关闭
        // （2026-08-16 实测：页 0 正常、翻页消失、翻回恢复；QQ 全局索引翻页正常）。
        self.selected
            .set((snap.page.page * ps + snap.selected.min(ps - 1)) as u32);
        self.page.set(snap.page.page as u32);
        let total = self.candidates.borrow().len();
        self.page_count.set((total.div_ceil(ps)).max(1) as u32);
        self.page_size.set(ps as u32);
    }
}

impl ITfUIElement_Impl for CandidateElement_Impl {
    fn GetDescription(&self) -> Result<BSTR> {
        log_line("[uielem] GetDescription 被调");
        Ok(BSTR::from("iuv 拼音候选"))
    }

    fn GetGUID(&self) -> Result<GUID> {
        Ok(crate::registration::clsid())
    }

    fn Show(&self, bshow: BOOL) -> Result<()> {
        self.shown.set(bshow.as_bool());
        log_line(&format!("[uielem] Show({})", bshow.as_bool()));
        Ok(())
    }

    fn IsShown(&self) -> Result<BOOL> {
        Ok(BOOL::from(self.shown.get()))
    }
}

impl ITfCandidateListUIElement_Impl for CandidateElement_Impl {
    fn GetUpdatedFlags(&self) -> Result<u32> {
        log_line("[uielem] GetUpdatedFlags 被调");
        Ok(TF_CLUIE_COUNT
            | TF_CLUIE_SELECTION
            | TF_CLUIE_STRING
            | TF_CLUIE_PAGEINDEX
            | TF_CLUIE_CURRENTPAGE)
    }

    fn GetDocumentMgr(&self) -> Result<ITfDocumentMgr> {
        log_line("[uielem] GetDocumentMgr 被调");
        match self.thread_mgr.borrow().as_ref() {
            Some(tm) => {
                // SAFETY: 标准 TSF 查询当前焦点文档。
                match unsafe { tm.GetFocus() } {
                    Ok(doc) => {
                        log_line("[uielem] GetDocumentMgr：返回焦点文档");
                        Ok(doc)
                    }
                    Err(e) => {
                        log_line(&format!("[uielem] GetDocumentMgr：GetFocus 失败：{e:?}"));
                        Err(e)
                    }
                }
            }
            None => {
                log_line("[uielem] GetDocumentMgr：无线程管理器，E_INVALIDARG");
                Err(windows_core::Error::from_hresult(E_INVALIDARG))
            }
        }
    }

    fn GetCount(&self) -> Result<u32> {
        let n = self.candidates.borrow().len() as u32;
        log_line(&format!("[uielem] GetCount 被调：{n}"));
        Ok(n)
    }

    fn GetSelection(&self) -> Result<u32> {
        let s = self.selected.get();
        log_line(&format!("[uielem] GetSelection 被调：{s}"));
        Ok(s)
    }

    fn GetString(&self, uindex: u32) -> Result<BSTR> {
        // 全局索引（全量候选）——桥构造完整 CANDIDATELIST、游戏翻页切片的数据源。
        let text = self
            .candidates
            .borrow()
            .get(uindex as usize)
            .cloned()
            .unwrap_or_default();
        log_line(&format!(
            "[uielem] GetString({uindex}) 被调：{}",
            if text.is_empty() {
                "<空/越界>"
            } else {
                &text
            }
        ));
        if uindex as usize >= self.candidates.borrow().len() {
            return Err(windows_core::Error::from_hresult(E_INVALIDARG));
        }
        Ok(BSTR::from(text.as_str()))
    }

    fn GetPageIndex(&self, pindex: *mut u32, buf_len: u32, pupagecnt: *mut u32) -> Result<()> {
        log_line("[uielem] GetPageIndex 被调");
        let page_count = self.page_count.get();
        let page_size = self.page_size.get();
        // SAFETY: 输出指针由调用方提供（TSF 标准约定）；按容量写入。
        if !pupagecnt.is_null() {
            unsafe { *pupagecnt = page_count };
        }
        if !pindex.is_null() {
            let n = page_count.min(buf_len);
            for i in 0..n {
                unsafe { *pindex.add(i as usize) = i * page_size };
            }
        }
        Ok(())
    }

    fn SetPageIndex(&self, pindex: *const u32, upagecnt: u32) -> Result<()> {
        log_line(&format!(
            "[uielem] SetPageIndex 被调：upagecnt={upagecnt}（外部改页暂忽略，页由引擎按键驱动）"
        ));
        // pindex 读取：记录首个值辅助判断桥行为（不改变引擎页状态）。
        if !pindex.is_null() {
            log_line(&format!("[uielem] SetPageIndex 首索引={}", unsafe {
                *pindex
            }));
        }
        Ok(())
    }

    fn GetCurrentPage(&self) -> Result<u32> {
        let p = self.page.get();
        log_line(&format!("[uielem] GetCurrentPage 被调：{p}"));
        Ok(p)
    }
}

// ITfCandidateListUIElementBehavior：候选列表的"控制侧"接口（TSF 3.0）。
// 当前仅观察位：WoW/notepad 实测桥从未调用（选词由 TSF 按键路径驱动、游戏内框
// 只是显示，游戏不实现 TSF 3.0 控制接口）——保留保险（若未来桥/应用使用，
// 可联动引擎：SetSelection→session.set_selected、Finalize→commit、Abort→cancel）。
impl ITfCandidateListUIElementBehavior_Impl for CandidateElement_Impl {
    fn SetSelection(&self, nindex: u32) -> Result<()> {
        log_line(&format!(
            "[uielem] Behavior::SetSelection({nindex}) 被调（桥选候选——暂不联动引擎，观察）"
        ));
        Ok(())
    }

    fn Finalize(&self) -> Result<()> {
        log_line("[uielem] Behavior::Finalize 被调（桥要求上屏——暂不联动，观察）");
        Ok(())
    }

    fn Abort(&self) -> Result<()> {
        log_line("[uielem] Behavior::Abort 被调（桥要求取消——暂不联动，观察）");
        Ok(())
    }
}
