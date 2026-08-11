//! 会话状态机。契约 01-contract.md §4 session.rs / §4.1 按键行为。

use crate::{Candidate, Effect, Engine, Key, PageInfo, SessionEnd};
use std::sync::Arc;

/// 一次输入会话。TSF/REPL 创建后逐键喂入。
pub struct Session {
    engine: Arc<Engine>,
    raw: String,
    seg: Vec<String>,
    /// 已确认上屏词栈：(文本, 词条 code)——续接选词入栈，退格回退栈顶
    picked: Vec<(String, String)>,
    /// 全表候选
    all: Vec<Candidate>,
    page: usize,
    selected: usize,
    end: Option<SessionEnd>,
    /// 本轮按键的部分上屏词（续接选词时设置，effect() 输出给 TSF）
    part_commit: Option<String>,
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
            part_commit: None,
        }
    }

    pub fn on_key(&mut self, key: Key) -> Effect {
        // 会话结束后兜底：不再处理，返回 Cancel
        if self.end.is_some() {
            return Effect { end: Some(SessionEnd::Cancel), ..Effect::default() };
        }
        self.part_commit = None;
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
                self.end = Some(SessionEnd::Cancel);
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
    /// 未消费完 → picked 入栈、尾巴续接（part_commit 输出本次上屏词）。
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
            // 全部消费：上屏 picked + 本次词，会话结束
            let mut text = self.picked.iter().map(|(t, _)| t.clone()).collect::<String>();
            text.push_str(&c.text);
            self.end = Some(SessionEnd::Commit(text));
        } else {
            // 部分消费：上屏本次词（part_commit），尾巴留预编辑继续
            self.picked.push((c.text.clone(), code_key));
            self.raw = self.seg[consumed..].join("'");
            self.part_commit = Some(c.text);
            self.recompute();
        }
    }

    /// 当前全部待上屏文本：picked 拼接 + 未消费拼音 raw。
    fn all_text(&self) -> String {
        let mut text = self.picked.iter().map(|(t, _)| t.clone()).collect::<String>();
        text.push_str(&self.raw);
        text
    }

    /// 重切分 → 重新生成候选 → page=0, selected=0。无候选也保持 active。
    /// segment 返回全部切分方案；本会话使用方案[0]（贪心/强制），
    /// engine 内部按"砍尾巴逐级前缀"（k=n..1）生成从长到短的候选。
    fn recompute(&mut self) {
        let plans = self.engine.schema.segment(&self.raw);
        self.seg = plans.first().cloned().unwrap_or_default();
        self.all = self.engine.generate_candidates(&self.raw, &self.seg);
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
            (self.all.len() + ps - 1) / ps
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
        // 微软式：预编辑文本 = 拼音分段（ce'shi），候选列表只放候选窗；
        // commit 上屏时由 end.text 替换（TSF 侧 apply_effect 的 Commit 分支）；
        // 续接时 composition 只显示尾巴（已选词已由 part_commit 上屏）。
        // 方案[0] join 即含用户强制分隔符（`'` 硬切分空段保留），按 `'` 即有反馈。
        let preview = self.engine.schema.display(&self.seg);
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
            part_commit: self.part_commit.clone(),
        }
    }

    /// 有未提交的原始输入（TSF 据此决定按键是否放行给应用）。
    pub fn is_active(&self) -> bool {
        !self.raw.is_empty() && self.end.is_none()
    }
}
