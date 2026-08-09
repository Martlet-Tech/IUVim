//! 会话状态机。契约 01-contract.md §4 session.rs / §4.1 按键行为。

use crate::{Candidate, Effect, Engine, Key, PageInfo, SessionEnd};
use std::sync::Arc;

/// 一次输入会话。TSF/REPL 创建后逐键喂入。
pub struct Session {
    engine: Arc<Engine>,
    raw: String,
    seg: Vec<String>,
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
                if !self.raw.is_empty() {
                    self.raw.pop();
                }
                if self.raw.is_empty() {
                    self.end = Some(SessionEnd::Cancel);
                    self.all.clear();
                    self.seg.clear();
                } else {
                    self.recompute();
                }
            }
            Key::Space => {
                if self.all.is_empty() {
                    self.end = Some(SessionEnd::Commit(self.raw.clone()));
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
                self.end = Some(SessionEnd::Commit(self.raw.clone()));
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

    fn commit_index(&mut self, idx: usize) {
        let c = self.all[idx].clone();
        let code_key = match c.kind {
            crate::CandidateKind::Sentence => self.seg.concat(),
            _ => c.code.clone(),
        };
        self.engine.record_selection(&code_key, &c.text);
        self.end = Some(SessionEnd::Commit(c.text));
    }

    /// 重切分 → 重新生成候选 → page=0, selected=0。无候选也保持 active。
    fn recompute(&mut self) {
        self.seg = self.engine.schema.segment(&self.raw);
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
        let composition = page_cands
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_else(|| self.raw.clone());
        let selected = if page_cands.is_empty() { 0 } else { self.selected };
        Effect {
            composition,
            reading: self.engine.schema.display(&self.seg),
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
