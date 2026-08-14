//! 会话状态机。契约 01-contract.md §4 session.rs / §4.1 按键行为。

use crate::{Candidate, Effect, Engine, Key, PageInfo, SessionEnd};
use std::sync::Arc;

/// 一次输入会话。TSF/REPL 创建后逐键喂入。
pub struct Session {
    engine: Arc<Engine>,
    raw: String,
    seg: Vec<String>,
    /// 已确认选词栈：(文本, 词条 code)——选中间级词入栈（悬空，未上屏），退格回退栈顶
    picked: Vec<(String, String)>,
    /// 全表候选
    all: Vec<Candidate>,
    page: usize,
    selected: usize,
    end: Option<SessionEnd>,
}

impl Session {
    pub(crate) fn new(engine: Arc<Engine>) -> Session {
        Session {
            engine,
            raw: String::new(),
            seg: Vec::new(),
            picked: Vec::new(),
            all: Vec::new(),
            page: 0,
            selected: 0,
            end: None,
        }
    }

    pub fn on_key(&mut self, key: Key) -> Effect {
        // 会话结束后兜底：不再处理，返回 Cancel
        if self.end.is_some() {
            return Effect {
                end: Some(SessionEnd::Cancel),
                ..Effect::default()
            };
        }
        match key {
            Key::Char(c) if c.is_ascii_lowercase() || c == '\'' => {
                // 连续 `'`：已处于分隔尾态（raw 以 `'` 结尾）时忽略，不允许 `''`——
                // 空段只允许来自尾撇号（xi'），连续撇号产生的多余空段会让切分/续接
                // 出现空段怪态（实测 2026-08-13：xi''an 尾巴续接保留 'an 表现不佳）。
                if c == '\'' && self.raw.ends_with('\'') {
                    return self.effect();
                }
                self.raw.push(c);
                self.recompute();
            }
            Key::ShiftChar(c) if c.is_ascii_uppercase() => {
                // 大写保形进序列：原样追加大写（不转小写）。匹配只认小写——大写字符
                // 切分时不被音节表命中（按不可匹配字符单字母段兜底），其余自然流；
                // commit 时 raw 原样上屏（如 Hello）。大写也是开会话键（is_session_start_key）。
                self.raw.push(c);
                self.recompute();
            }
            Key::Backspace => {
                if let Some((_, code)) = self.picked.pop() {
                    // 有已选词：回退栈顶，词 code 拼回 raw 头部（续接的逆操作）
                    self.raw = if self.raw.is_empty() {
                        code
                    } else {
                        format!("{code}'{}", self.raw)
                    };
                    self.recompute();
                } else if !self.raw.is_empty() {
                    // 真退格：删 raw 尾字符
                    self.raw.pop();
                    if self.raw.is_empty() {
                        self.end = Some(SessionEnd::Cancel);
                        self.all.clear();
                        self.seg.clear();
                    } else {
                        self.recompute();
                    }
                }
            }
            Key::Space => {
                if self.all.is_empty() {
                    self.end = Some(SessionEnd::Commit(self.all_text()));
                } else {
                    let idx = self.page * self.page_size() + self.selected;
                    self.commit_index(idx);
                }
            }
            Key::Digit(n) if (1..=9).contains(&n) => {
                let idx = self.page * self.page_size() + (n - 1) as usize;
                if idx < self.all.len() {
                    self.commit_index(idx);
                }
                // 越界：消费但无操作
            }
            Key::Enter => {
                self.end = Some(SessionEnd::Commit(self.all_text()));
            }
            Key::Esc => {
                // 悬空状态：已选词入栈未上屏——Esc 先把已选词上屏（composition 全量替换，尾巴随之消失），
                // 无已选词才整句取消。
                if self.picked.is_empty() {
                    self.end = Some(SessionEnd::Cancel);
                } else {
                    self.end = Some(SessionEnd::Commit(self.picked_text()));
                }
            }
            Key::PageDown => {
                let pc = self.page_count();
                if pc > 0 {
                    self.page = (self.page + 1).min(pc - 1);
                    self.selected = 0;
                }
            }
            Key::PageUp => {
                let pc = self.page_count();
                if pc > 0 {
                    self.page = self.page.saturating_sub(1);
                    self.selected = 0;
                }
            }
            Key::Up | Key::Left => self.move_selected(-1),
            Key::Down | Key::Right => self.move_selected(1),
            // M2 主动调权（18-m2-user-dict.md）：与相邻候选交换权重。立即重排
            // （recompute 重置 page/selected 后定位被调词），会话不结束——松手后可继续导航/上屏。
            Key::SwapLeft | Key::SwapRight => {
                let ps = self.page_size();
                let idx = self.page * ps + self.selected;
                let dir: i32 = if matches!(key, Key::SwapLeft) { -1 } else { 1 };
                let other = idx as i32 + dir;
                if idx >= self.all.len() || other < 0 || other as usize >= self.all.len() {
                    // 边界（1 号位 Alt+← / 末位 Alt+→）：消费但忽略
                } else {
                    let keep = self.all[idx].clone();
                    let b = &self.all[other as usize];
                    self.engine
                        .swap_weights(&keep.code, &keep.text, &b.code, &b.text);
                    // 立即重排 + 高亮跟随被调词（候选 text 唯一，generate_candidates 已去重）
                    self.recompute();
                    if let Some(pos) = self
                        .all
                        .iter()
                        .position(|c| c.code == keep.code && c.text == keep.text)
                    {
                        self.page = pos / ps;
                        self.selected = pos % ps;
                    }
                }
            }
            Key::Digit(_) | Key::Char(_) | Key::ShiftChar(_) => {}
        }
        self.effect()
    }

    /// 选候选：消费 `c.seg_len` 段。消费完 → 上屏 picked+词、会话结束；
    /// 未消费完 → picked 入栈（悬空，不上屏）、尾巴续接，会话继续。
    fn commit_index(&mut self, idx: usize) {
        let c = self.all[idx].clone();
        let consumed = c.seg_len.max(1);
        // 消费边界用有效段数（非空段）：尾/连续撇号产生的空段只服务 display，
        // 不构成消费边界——否则 `xi'` 选"系"（seg_len=1）会被判成部分消费，
        // 悬空"系"+ 空尾巴导致"空候选表"（实测 2026-08-13）。
        let n = self.seg.iter().filter(|s| !s.is_empty()).count();
        // 学习 key：Sentence 用其覆盖的前缀段（seg[..consumed]），
        // 其余用词条自身 code（枚举变体如"西安"code="xi'an" 亦为消费键）。
        let code_key = match c.kind {
            crate::CandidateKind::Sentence => self.seg[..consumed].join("'"),
            _ => c.code.clone(),
        };
        self.engine.record_selection(&code_key, &c.text);
        if consumed >= n {
            // 全部消费：上屏 picked + 本次词，会话结束。
            // composition 全程覆盖整个混合预编辑文本，SetText 全量替换，无重复上屏。
            let mut text = self.picked_text();
            text.push_str(&c.text);
            self.end = Some(SessionEnd::Commit(text));
        } else {
            // 部分消费：悬空——只入栈 + 尾巴续接，不产生任何 commit 信号；
            // 已选词视觉反馈由预编辑混合显示提供（effect().composition）。
            self.picked.push((c.text.clone(), code_key));
            self.raw = self.seg[consumed..].join("'");
            self.recompute();
        }
    }

    /// 已选词拼接文本（悬空栈：只含汉字，不含未消费拼音）。
    fn picked_text(&self) -> String {
        self.picked
            .iter()
            .map(|(t, _)| t.clone())
            .collect::<String>()
    }

    /// 当前全部待上屏文本：picked 拼接 + 未消费拼音 raw。
    fn all_text(&self) -> String {
        let mut text = self.picked_text();
        text.push_str(&self.raw);
        text
    }

    /// 重切分 → 重新生成候选 → page=0, selected=0。无候选也保持 active。
    /// segment 返回全部切分方案；本会话使用方案[0]（贪心/强制），
    /// engine 内部按"砍尾巴逐级前缀"（k=n..1）生成从长到短的候选。
    fn recompute(&mut self) {
        let plans = self.engine.schema.segment(&self.raw);
        self.seg = plans.first().cloned().unwrap_or_default();
        self.all = self
            .engine
            .generate_candidates(&self.raw, &self.seg, plans.len());
        self.page = 0;
        self.selected = 0;
    }

    fn page_size(&self) -> usize {
        self.engine.config().page_size.max(1)
    }

    /// 页内导航（Up/Left=-1，Down/Right=+1）：边界环绕——页尾继续 → 翻下一页
    /// （selected=0）；页首回退 → 翻上一页（selected=页尾）。首/末页夹紧。
    fn move_selected(&mut self, dir: i32) {
        let len = self.page_candidates().len();
        if len == 0 {
            return;
        }
        let next = self.selected as i32 + dir;
        if next < 0 {
            if self.page > 0 {
                self.page -= 1;
                self.selected = self.page_candidates().len().saturating_sub(1);
            } else {
                self.selected = 0;
            }
        } else if next as usize >= len {
            if self.page + 1 < self.page_count() {
                self.page += 1;
                self.selected = 0;
            } else {
                self.selected = len - 1;
            }
        } else {
            self.selected = next as usize;
        }
    }

    /// 鼠标悬停同步：把页内高亮定位到指定行（夹紧到页内行尾）。
    /// 只改 selected 不重算候选——视觉由候选窗本地重绘，会话保持一致性。
    pub fn set_selected(&mut self, row: usize) {
        let len = self.page_candidates().len();
        if len > 0 {
            self.selected = row.min(len - 1);
        }
    }

    fn page_count(&self) -> usize {
        let ps = self.page_size();
        if self.all.is_empty() {
            0
        } else {
            self.all.len().div_ceil(ps)
        }
    }

    fn page_candidates(&self) -> &[Candidate] {
        let ps = self.page_size();
        let start = self.page * ps;
        if start >= self.all.len() {
            &[]
        } else {
            &self.all[start..(start + ps).min(self.all.len())]
        }
    }

    /// 不交按键取当前快照（REPL/测试用）。
    pub fn effect(&self) -> Effect {
        let page_cands = self.page_candidates().to_vec();
        // 混合预编辑（悬空显示）：已选词汉字 + 未选部分拼音分段。
        // 如选"床前"后：`床前ming'yue'guang`；commit 时由 end.text 全量替换上屏。
        // 方案[0] join 即含用户强制分隔符（`'` 硬切分空段保留），按 `'` 即有反馈。
        let tail_preview = self.engine.schema.display(&self.seg);
        let preview = if self.picked.is_empty() {
            tail_preview
        } else {
            let mut s = self.picked_text();
            s.push_str(&tail_preview);
            s
        };
        let selected = if page_cands.is_empty() {
            0
        } else {
            self.selected
        };
        Effect {
            composition: preview.clone(),
            reading: preview,
            candidates: page_cands,
            selected,
            page: PageInfo {
                page: self.page,
                page_count: self.page_count(),
                page_size: self.page_size(),
                total: self.all.len(),
            },
            end: self.end.clone(),
        }
    }

    /// 有未提交的原始输入（TSF 据此决定按键是否放行给应用）。
    pub fn is_active(&self) -> bool {
        !self.raw.is_empty() && self.end.is_none()
    }
}
