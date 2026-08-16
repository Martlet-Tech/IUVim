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
            // M2 隐藏候选（Shift+Delete）：先删用户库条目（自造词/覆盖），否则屏蔽
            // 基础库词条。立即重排（recompute 重置 page/selected），高亮落在原位置附近。
            Key::HideCandidate => {
                let ps = self.page_size();
                let idx = self.page * ps + self.selected;
                if idx < self.all.len() {
                    let target = self.all[idx].clone();
                    self.engine.hide_entry(&target.code, &target.text);
                    self.recompute();
                    if !self.all.is_empty() {
                        let pc = self.page_count();
                        self.page = (idx / ps).min(pc - 1);
                        self.selected =
                            (idx % ps).min(self.page_candidates().len().saturating_sub(1));
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
        // 夹紧 consumed.min(n)：防御 seg_len 异常 > 当前段数（2026-08-14：
        // 劣质整句候选曾 seg_len=3 > seg 段数 2 → seg[..3] 越界 panic 被 guard
        // 吞掉导致无法上屏——全完整过滤已治本，此处双保险）。
        let code_key = match c.kind {
            crate::CandidateKind::Sentence => self.seg[..consumed.min(n)].join("'"),
            _ => c.code.clone(),
        };
        self.engine.record_selection(&code_key, &c.text);
        if consumed >= n {
            // 全部消费：上屏 picked + 本次词，会话结束。
            // composition 全程覆盖整个混合预编辑文本，SetText 全量替换，无重复上屏。
            let mut text = self.picked_text();
            text.push_str(&c.text);
            // M2 自造词（18-m2-user-dict.md）：逐字选择（picked 全部单字）+ 整串 ≥2 字
            // → 引擎记录（场景 0/a/b 权重判定在 engine::record_phrase 内）。
            if self.picked.len() >= 1
                && self.picked.iter().all(|(t, _)| t.chars().count() == 1)
                && c.text.chars().count() == 1
                && text.chars().count() >= 2
            {
                let mut codes: Vec<&str> =
                    self.picked.iter().map(|(_, code)| code.as_str()).collect();
                codes.push(code_key.as_str());
                self.engine.record_phrase(&codes.join("'"), &text);
            }
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

    /// 待上屏**原文**（picked + raw，无切分撇号——raw 是用户敲的字母串，撇号只存在于
    /// 切分显示层）。关闭输入法（Ctrl+Space）/焦点切换（Alt+Tab）时的原文上屏语义，
    /// TSF 侧 `flush_session` 用。
    pub fn pending_text(&self) -> String {
        self.all_text()
    }

    /// 重切分 → 重新生成候选 → page=0, selected=0。无候选也保持 active。
    /// segment 返回全部切分方案；**消费端词频重排**（engine.rank_plans，2026-08-14：
    /// 方案[0] = 词频最优而非贪心——分节显示/主路径跟随用户最可能打的词，
    /// keneng → ke'neng、dier → di'er），engine 内部按"砍尾巴逐级前缀"
    /// （k=n..1）生成从长到短的候选，整句遍历所有方案。
    fn recompute(&mut self) {
        let plans = self.engine.schema.segment(&self.raw);
        let plans = self.engine.rank_plans(plans);
        self.seg = plans.first().cloned().unwrap_or_default();
        self.all = self
            .engine
            .generate_candidates(&self.raw, &self.seg, &plans);
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
        // **预编辑显示规则（2026-08-16，对齐主流输入法）**：
        // 1. 只显示用户输入的字母 + ' 分节（绝不出现超出输入的扩展——禁止 ji'shi'ben）；
        // 2. 有匹配（输入可扩展成词）→ 分节显示（' 连接，边界 = 输入合法切分）；
        // 3. 分节边界可跟随候选（导航变化，但切分必须基于输入）：xian→西安 显示 xi'an；
        // 4. 无匹配（input 逐扩展无词）→ 原样不分节（input）；
        // 5. 用户按的 ' 恒保留参与分节（xi'、zhu'jincheng→zhu'jin'cheng）。
        let tail_preview = if page_cands.is_empty() {
            self.engine.schema.display(&self.seg)
        } else {
            let idx = self.selected.min(page_cands.len() - 1);
            self.candidate_preview(&page_cands[idx])
        };
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
            all_candidates: self.all.clone(),
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

    /// 预编辑显示（对齐主流输入法，2026-08-16 修正规则）。判定顺序：
    /// 1. **用户强制撇号**（raw 含 `'`）：恒输入切分（用户 `'` 参与分节显示——
    ///    schema 硬边界，空段保留），不跟随候选——xi' → "xi'"、zhu'jincheng → zhu'jin'cheng；
    /// 2. **原文兜底**（text == 输入去撇号，无匹配）：原样 plain（不分节）——input → "input"；
    /// 3. **消费段不完整**（简拼整词 jisb→记事本/jishiben、简拼键 nh、前缀档 n→你）：
    ///    输入切分（display(seg)）——jisb → "ji's'b"、nh → "n'h"、n → "n"（候选 code 超出输入/非全音节不跟随）；
    /// 4. **消费段完整 且 候选 code（去撇号）== 输入**：跟随候选切分（code + 尾巴）——
    ///    xian→西安 "xi'an"、fenge→分割 "fen'ge"/风额 "feng'e"；
    /// 5. 其余（单字"分"等 code≠输入）：输入切分（display(seg)）——fenge 导航到"分"仍 "fen'ge"。
    fn candidate_preview(&self, c: &Candidate) -> String {
        if self.raw.contains('\'') {
            return self.engine.schema.display(&self.seg);
        }
        let plain: String = self.raw.chars().filter(|c| *c != '\'').collect();
        if c.text == plain {
            return plain;
        }
        let consumed = c.seg_len.max(1).min(self.seg.len());
        let consumed_full = self.seg[..consumed]
            .iter()
            .all(|s| !s.is_empty() && self.engine.is_syllable(s));
        if !consumed_full {
            return self.engine.schema.display(&self.seg);
        }
        let code_plain: String = c.code.chars().filter(|c| *c != '\'').collect();
        if code_plain == plain {
            let mut s = c.code.clone();
            if consumed < self.seg.len() {
                s.push('\'');
                s.push_str(&self.engine.schema.display(&self.seg[consumed..]));
            }
            return s;
        }
        self.engine.schema.display(&self.seg)
    }

    /// 有未提交的原始输入（TSF 据此决定按键是否放行给应用）。
    pub fn is_active(&self) -> bool {
        !self.raw.is_empty() && self.end.is_none()
    }
}
