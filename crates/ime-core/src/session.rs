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
            return Effect { end: Some(SessionEnd::Cancel), ..Effect::default() };
        }
        match key {
            Key::Char(c) if c.is_ascii_lowercase() || c == '\'' => {
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
            Key::Up => {
                let len = self.page_candidates().len();
                if len > 0 {
                    self.selected = self.selected.saturating_sub(1).min(len - 1);
                }
            }
            Key::Down => {
                let len = self.page_candidates().len();
                if len > 0 {
                    self.selected = (self.selected + 1).min(len - 1);
                }
            }
            Key::Digit(_) | Key::Char(_) => {}
        }
        self.effect()
    }

    /// 选候选：消费 `c.seg_len` 段。消费完 → 上屏 picked+词、会话结束；
    /// 未消费完 → picked 入栈（悬空，不上屏）、尾巴续接，会话继续。
    fn commit_index(&mut self, idx: usize) {
        let c = self.all[idx].clone();
        let consumed = c.seg_len.max(1);
        let n = self.seg.len();
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
        self.picked.iter().map(|(t, _)| t.clone()).collect::<String>()
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
        self.all = self.engine.generate_candidates(&self.raw, &self.seg, plans.len());
        self.page = 0;
        self.selected = 0;
    }

    fn page_size(&self) -> usize {
        self.engine.config().page_size.max(1)
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
        let selected = if page_cands.is_empty() { 0 } else { self.selected };
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
