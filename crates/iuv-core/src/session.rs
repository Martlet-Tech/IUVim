//! 会话状态机。契约 01-contract.md §4 session.rs / §4.1 按键行为。

use crate::{fullwidth_text, Candidate, Effect, Engine, ImeState, Key, PageInfo, ScriptMode, SessionEnd};
use std::sync::{Arc, Mutex};

/// 一次输入会话。TSF/REPL 创建后逐键喂入。
pub struct Session {
    /// 资源引擎（调权/隐藏/自造词/配置/简繁等资源操作）。
    engine: Arc<Engine>,
    /// 候选生成核心（ImeEngine 接缝，39-rime-pipeline.md）：classic 或 rime。
    ime: Arc<dyn crate::api::ImeEngine>,
    /// 实例运行时四态（32-status-toolbar.md §5.1）：**live 读**——工具栏点简繁等
    /// 切换后 `effect()`/`to_output` 立即读取新值，当前候选/预编辑马上重渲，不重建会话。
    /// 与引擎配置解耦：进程级 Engine 单例共享多实例，运行时态必须 per-实例。
    runtime: Arc<Mutex<ImeState>>,
    raw: String,
    /// 字面尾巴（issue「d冒号表现不一致」）：会话内符号键触发进入，其后一切按键按
    /// 文本输入语义追加至此——预编辑显示 拼音+尾巴、提交原样上屏（对齐搜狗）。
    /// Backspace 逐字删除、删空自动回拼音态（on_key 顶部唯一锁定块处理）。
    tail: String,
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
    /// 默认运行时 = 引擎配置 `initial_state`（REPL/测试路径；TSF 用 `with_runtime`）。
    pub(crate) fn new(engine: Arc<Engine>) -> Session {
        let runtime = engine.config().initial_state;
        Session::with_runtime(engine, Arc::new(Mutex::new(runtime)))
    }

    /// 注入实例运行时态（32-status-toolbar.md §5.1：per-实例四态，live 读）。
    pub(crate) fn with_runtime(
        engine: Arc<Engine>,
        runtime: Arc<Mutex<ImeState>>,
    ) -> Session {
        let ime = engine.clone() as Arc<dyn crate::api::ImeEngine>;
        Session::over(engine, ime, runtime)
    }

    /// 挂自定义候选核心开会话（39-rime-pipeline.md Step2：rime 核心经此接入；
    /// 资源操作仍走 classic Engine——共享同一 Dict 实例）。
    pub fn over(
        engine: Arc<Engine>,
        ime: Arc<dyn crate::api::ImeEngine>,
        runtime: Arc<Mutex<ImeState>>,
    ) -> Session {
        Session {
            engine,
            ime,
            runtime,
            raw: String::new(),
            tail: String::new(),
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
        // 字面模式锁定（tail 非空）：唯一的新状态处理点——一切按键按"文本输入"
        // 语义处理，Backspace 逐字删除、删空自动回拼音态（tail 复位后走下方
        // 常规臂），无任何散落判断。翻页/箭头/调权/隐藏字面态无候选可作用，
        // 消费但忽略。
        if !self.tail.is_empty() {
            match key {
                Key::Char(c) | Key::ShiftChar(c) => self.tail.push(c),
                Key::Digit(n) => self.tail.push((b'0' + n) as char),
                Key::Space => self.tail.push(' '),
                Key::Backspace => {
                    self.tail.pop();
                }
                Key::Enter => self.end = Some(SessionEnd::Commit(self.all_text())),
                // 字面意图下提交半个词很怪：Esc 取消整个会话
                Key::Esc => self.end = Some(SessionEnd::Cancel),
                _ => {}
            }
            return self.effect();
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
                if let Some((text, code)) = self.picked.pop() {
                    // rime 式逐字退（39-rime-pipeline.md §6）：多字词退末字——
                    // 末字音节还原回未确认区，其余部分留栈；单字词整词退。
                    // 音节数≠字数（简拼整词如 code="nhm"）无法对位 → 整词退兜底。
                    let chars: Vec<char> = text.chars().collect();
                    let syls: Vec<&str> = code.split('\'').collect();
                    if chars.len() >= 2 && syls.len() == chars.len() {
                        let new_text: String = chars[..chars.len() - 1].iter().collect();
                        let new_code = syls[..syls.len() - 1].join("'");
                        self.picked.push((new_text, new_code));
                        let back_syllable = syls[syls.len() - 1].to_string();
                        self.raw = if self.raw.is_empty() {
                            back_syllable
                        } else {
                            format!("{}'{}", back_syllable, self.raw)
                        };
                    } else {
                        // 整词退：词 code 拼回 raw 头部（原续接逆操作）
                        self.raw = if self.raw.is_empty() {
                            code
                        } else {
                            format!("{}'{}", code, self.raw)
                        };
                    }
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
                    let idx = self.selected_idx();
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
                let idx = self.selected_idx();
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
                let idx = self.selected_idx();
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
            // 其余可打印符号键（未被 keymap 占用——占用键已在桥层重映射为翻页等）：
            // 进入字面尾巴。预编辑显示 拼音+尾巴、提交原样上屏（issue「d冒号表现不一致」：
            // 旧实现放行给应用，Word/记事本/Excel 插入位置各异 → :d / d: / 的:）。
            Key::Char(c) => self.tail.push(c),
            // 防御性忽略：Tab/Delete/Home/End/Insert/F1-F12 正常情况下经组合查表被归一化为
            // 会话动作（或未绑定放行给应用），不该直达会话；兜底忽略不扰动 composition。
            Key::Digit(_)
            | Key::ShiftChar(_)
            | Key::Tab
            | Key::Delete
            | Key::Home
            | Key::End
            | Key::Insert
            | Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12 => {}
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
            // 提交文本套宽度转换（原文兜底候选如 "window" 全角下 → "ｗｉｎｄｏｗ"）；
            // 自造词记录用上面的原文 text（不录全角），仅上屏时转换。
            self.end = Some(SessionEnd::Commit(self.to_output(text)));
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

    /// 当前全部待上屏文本：picked 拼接 + 未消费拼音 raw + 字面尾巴。
    /// 输出前套宽度转换（全角模式原文上屏转全角，`nihao` → `ｎｉｈａｏ`）——
    /// Enter/无候选空格/pending_text（flush）均走此路径，一处覆盖。
    fn all_text(&self) -> String {
        let mut text = self.picked_text();
        text.push_str(&self.raw);
        text.push_str(&self.tail);
        self.to_output(text)
    }

    /// 提交文本宽度 + 字形转换：先 `fullwidth_text`（ASCII→全角），再简→繁
    /// （仅 `script == Traditional` 且转换器已装配；汉字/拼音/符号直通，幂等）。
    /// 会话外直接上屏的数字/符号已在 TSF 侧转全角（fullwidth_pending），此处只处理
    /// 会话内原文上屏（预编辑 raw）。显示路径（picked_text 用于 composition）不转换。
    /// 宽度/字形读**实例运行时态**（32-status-toolbar.md §5.1，非引擎 config）。
    fn to_output(&self, text: String) -> String {
        let width = self.runtime.lock().unwrap_or_else(|e| e.into_inner()).width;
        let w = fullwidth_text(&text, width);
        self.convert_script(&w)
    }

    /// 简→繁转换（31-script-traditional.md）：`script == Traditional` 且有转换器 → 转换；
    /// 否则原文返回。内部候选/自造词恒简体，仅在输出边界转换。
    /// 字形读**实例运行时态**（live：点简繁后当前候选/预编辑立即重渲）。
    fn convert_script(&self, text: &str) -> String {
        let script = self.runtime.lock().unwrap_or_else(|e| e.into_inner()).script;
        if script != ScriptMode::Traditional {
            return text.to_string();
        }
        match self.engine.script_converter() {
            Some(c) => c.convert(text),
            None => text.to_string(),
        }
    }

    /// 候选文本简→繁（显示边界：候选窗/预编辑显示繁体，commit 内部仍用简体原文）。
    fn convert_candidate(&self, c: &Candidate) -> Candidate {
        let text = self.convert_script(&c.text);
        Candidate {
            text,
            kind: c.kind,
            code: c.code.clone(),
            weight: c.weight,
            seg_len: c.seg_len,
        }
    }

    /// 待上屏**原文**（picked + raw，无切分撇号——raw 是用户敲的字母串，撇号只存在于
    /// 切分显示层）。关闭输入法（Ctrl+Space）/焦点切换（Alt+Tab）时的原文上屏语义，
    /// TSF 侧 `flush_session` 用。
    pub fn pending_text(&self) -> String {
        self.all_text()
    }

    /// 经引擎接口重算（Step 1 收编编排，39-rime-pipeline.md）：translate 一次完成
    /// 切分 → 方案词频重排 → 候选生成；会话层只存分段视图首段（= 原 seg）与候选。
    /// 无候选也保持 active。
    fn recompute(&mut self) {
        let preceding = self.picked_text();
        let ctx = crate::api::EngineCtx { preceding_text: &preceding };
        let tr = self
            .ime
            .translate(&ctx, &crate::api::PendingInput { raw: &self.raw });
        self.seg = tr
            .segmentation
            .into_iter()
            .next()
            .map(|s| s.syllables)
            .unwrap_or_default();
        self.all = tr.candidates;
        self.page = 0;
        self.selected = 0;
    }

    fn page_size(&self) -> usize {
        self.engine.page_size() as usize
    }

    /// 当前选中候选中索引（`page * page_size + selected`，P1.6 抽取：3 处样板收敛）。
    fn selected_idx(&self) -> usize {
        self.page * self.page_size() + self.selected
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
        // 字面模式：预编辑 = 拼音 + 字面尾巴，无汉字候选（对齐搜狗"汉字候选消失"）——
        // 快照空 → 桥端走现成的「快照为空 hide」分支收起候选窗，内联预编辑由应用
        // 渲染（d:）；游戏桥同源生效。尾巴恒原样拼接（不参与简繁转换，路径
        // d:\tools 等场景按字面输出）。
        let literal_mode = !self.tail.is_empty();
        if literal_mode {
            return Effect {
                composition: self.convert_script(&format!(
                    "{}{}{}",
                    self.picked_text(),
                    self.raw,
                    self.tail
                )),
                reading: String::new(),
                candidates: Vec::new(),
                all_candidates: Vec::new(),
                selected: 0,
                page: PageInfo {
                    page: 0,
                    page_count: 0,
                    page_size: self.page_size(),
                    total: 0,
                },
                end: self.end.clone(),
            };
        }
        let page_cands = self.page_candidates().to_vec();
        // 混合预编辑（悬空显示）：已选词汉字 + 未选部分拼音分段。
        // 如选"床前"后：`床前ming'yue'guang`；commit 时由 end.text 全量替换上屏。
        // 尾巴预编辑 = 引擎接口输出（preedit，五条显示规则已收编 classic 核心，
        // 39-rime-pipeline.md §4）：导航跟随候选切分（jian→吉安 显 ji'an）、
        // 强制撇号/兜底/简拼各归其规则；会话层只拼已确认前文。
        let preceding = self.picked_text();
        let ctx = crate::api::EngineCtx { preceding_text: &preceding };
        let tail_preview = self.ime.preedit(
            &ctx,
            &crate::api::PendingInput { raw: &self.raw },
            if page_cands.is_empty() {
                None
            } else {
                let idx = self.selected.min(page_cands.len() - 1);
                Some(&page_cands[idx])
            },
        );
        let preview = if self.picked.is_empty() {
            tail_preview
        } else {
            let mut s = self.picked_text();
            s.push_str(&tail_preview);
            s
        };
        // 显示边界简→繁（31-script-traditional.md）：composition/reading（预编辑，含
        // picked 汉字 + 拼音尾巴）与候选文本统一转换；内部 self.all/picked 恒简体
        // （commit/自造词/调权/屏蔽键不变）。原文兜底候选（纯拼音 window 等）转换
        // 恒等；「text == 预编辑原文去撇号」的原文兜底判定（preview_rules 规则 2）不受转换影响。
        let preview_disp = self.convert_script(&preview);
        Effect {
            composition: preview_disp.clone(),
            reading: preview_disp,
            candidates: page_cands.iter().map(|c| self.convert_candidate(c)).collect(),
            all_candidates: self.all.iter().map(|c| self.convert_candidate(c)).collect(),
            selected: self.selected,
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
