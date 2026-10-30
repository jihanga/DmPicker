//! 검색창의 성과표 시트: 배치·그리기·키·마우스, 성과표로 만든 플레이리스트 결과 줄의 세 칸
//! 상태와 조건 판정은 `crate::scorecard`, 이 파일은 popup.rs에 붙는 화면 쪽만

use super::*;

/// 성과표 시트 높이, 만들기 버튼 (376..410) 아래 16, 설정 시트보다 낮아서 창 높이는 그대로
pub(super) const SHEET_H: f32 = 426.0;
/// 성과표 플레이리스트는 레벨 칸 아랫줄에 고정 칸 셋: Max·Perfect, 스코어 "100.00%", 서열 "20.9" (간격 8)
/// 표시 칸이 맨 앞이라 비어도 뒤 두 칸 자리는 늘 같음
const COLS: [f32; 3] = [48.0, 52.0, 30.0];
const COL_GAP: f32 = 8.0;
pub(super) const LEVEL_W: f32 = 48.0 + 52.0 + 30.0 + COL_GAP * 2.0;
/// 값 칸을 누르고 있으면 반복을 시작할 때까지, 그 뒤 반복 간격 (Windows 키 반복과 비슷하게)
const REPEAT_DELAY_MS: u32 = 400;
const REPEAT_MS: u32 = 40;

/// 물리 px 기준 성과표 시트: 클릭 영역과 각 글줄의 위쪽
struct ScoreLayout {
    /// 묶음 제목 "기록 불러오기", "조건"
    titles: [f32; 2],
    /// 줄마다 (위, 아래): 버튼, 범위, 기록, 플레이, 스코어, 맥스콤보, 서열
    rows: [(f32, f32); 7],
    mode: D2D_RECT_F,
    /// 전체·범위 세그먼트와 그 오른쪽 범위 칸 (전체면 흐림)
    scope: D2D_RECT_F,
    range: D2D_RECT_F,
    load: D2D_RECT_F,
    /// 조건 줄의 체크와 라벨, 누르면 그 조건을 켜고 끔
    checks: [D2D_RECT_F; 4],
    played: D2D_RECT_F,
    score: (D2D_RECT_F, D2D_RECT_F),
    mc: D2D_RECT_F,
    floor: (D2D_RECT_F, D2D_RECT_F),
    /// 아래 칸 구분선
    foot: f32,
    make: D2D_RECT_F,
}

/// 성과표 시트에서 칸이 있는 줄 (`ScoreLayout::rows`), 만들기는 줄 강조 없이 버튼만 밝음
fn score_row(slot: Slot) -> Option<usize> {
    Some(match slot {
        Slot::Mode => 0,
        Slot::Scope | Slot::Range => 1,
        Slot::Load => 2,
        Slot::Played => 3,
        Slot::ScoreLo | Slot::ScoreHi => 4,
        Slot::MaxCombo => 5,
        Slot::FloorLo | Slot::FloorHi => 6,
        Slot::Make => return None,
    })
}

/// 화살표·휠·누르고 있기로 값을 오르내리는 칸
fn is_value_slot(slot: Slot) -> bool {
    matches!(slot, Slot::Range | Slot::ScoreLo | Slot::ScoreHi | Slot::FloorLo | Slot::FloorHi)
}

impl Popup {
    /// 성과표 시트를 여닫음, 닫으면 닉네임을 저장하게 `Choice`
    pub(super) fn toggle_scorecard(&mut self) -> Action {
        if self.view == View::Scorecard {
            self.set_view(View::Results);
            Action::Choice
        } else {
            self.set_view(View::Scorecard);
            Action::None
        }
    }

    /// 시트를 열 때 (`set_view`): 입력 칸은 닉네임, 받아 둔 기록이 없으면 바로 받음
    pub(super) fn enter_scorecard(&mut self) {
        self.edit.insert(&self.scorecard.nickname);
        self.seen = self.query();
        self.searched = self.query();
        self.scorecard.set_focus(None);
        self.scorecard.request(self.hwnd, false);
        self.refresh_scorecard();
    }

    /// 시트를 떠날 때 (`restore_list_query`): 쓰던 닉네임과 치던 숫자는 남김 (설정 파일에 저장됨)
    pub(super) fn leave_scorecard(&mut self) {
        if self.view == View::Scorecard {
            self.scorecard.nickname = self.edit.text().trim().to_owned();
            self.scorecard.set_focus(None);
        }
    }

    /// 입력이 멈춤 (`search_now`): 입력 칸 글이 닉네임
    pub(super) fn scorecard_typed(&mut self, q: &str) {
        self.scorecard.nickname = q.trim().to_owned();
        self.refresh_scorecard();
    }

    /// 작업 스레드의 기록 응답, 개수를 다시 셈
    pub fn scorecard_reply(&mut self) {
        self.scorecard.take();
        self.refresh_scorecard();
    }

    fn refresh_scorecard(&mut self) {
        self.scorecard_count = self.scorecard.select(&self.catalog.songs).map(|picked| {
            // 숨긴 곡은 결과에서 빠지고 (`playlist_rows`), 내 목록에 없는 곡은 흐린 줄
            let shown: Vec<usize> = picked.iter().map(|&(i, _)| i).filter(|&i| !self.hidden_songs().contains(&self.catalog.songs[i].id)).collect();
            let off = shown.iter().filter(|&&i| !self.catalog.allowed(i, self.owned.as_ref())).count();
            (shown.len(), off)
        });
    }

    /// 칸 값을 왼쪽 (-1)·오른쪽 (+1)으로, Shift를 누르고 있으면 서열은 1씩
    fn step_score_slot(&mut self, slot: Slot, d: i32) {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        self.scorecard.step(slot, d, shift);
        self.refresh_scorecard();
    }

    /// 값 칸 누르고 있기를 끝냄, 누르고 있었으면 true
    pub(super) fn stop_score_hold(&mut self) -> bool {
        if self.score_hold.take().is_none() {
            return false;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_REPEAT);
            let _ = ReleaseCapture();
        }
        true
    }

    /// 누르고 있는 동안 (`TIMER_REPEAT`) 커서가 그 칸 그 절반에 있을 때만 바꿈, 밖으로 끌면 멈췄다 돌아오면 이어서
    pub(super) fn scorecard_repeat(&mut self) {
        match self.score_hold {
            Some((slot, i)) if self.view == View::Scorecard => {
                if self.hover == Hit::ScoreSlot(slot, i) {
                    self.step_score_slot(slot, if i == 0 { -1 } else { 1 });
                }
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_REPEAT, REPEAT_MS, None);
                }
            }
            _ => {
                self.stop_score_hold();
            }
        }
    }

    /// 값 칸 위에서 휠을 굴림, 한 칸에 한 단계 (위로 굴리면 늘림), 정밀 휠은 모아서 칸으로 셈
    pub(super) fn scorecard_wheel(&mut self, d: i32) -> Action {
        let Hit::ScoreSlot(slot, _) = self.hover else { return Action::None };
        if !is_value_slot(slot) {
            return Action::None;
        }
        if d.signum() != self.wheel_acc.signum() {
            self.wheel_acc = 0;
        }
        self.wheel_acc += d;
        let steps = self.wheel_acc / WHEEL_DELTA as i32;
        self.wheel_acc -= steps * WHEEL_DELTA as i32;
        for _ in 0..steps.abs() {
            self.step_score_slot(slot, steps.signum());
        }
        self.scorecard.set_focus(None);
        if steps != 0 { Action::Choice } else { Action::None }
    }

    /// 칸을 누름, 세그먼트는 누른 칸 번호, 값 칸은 왼쪽 절반 0·오른쪽 절반 1
    pub(super) fn scorecard_click(&mut self, slot: Slot, i: usize) -> Action {
        // 값 칸을 누르면 그 칸에 포커스, 이어서 숫자를 바로 칠 수 있음
        self.scorecard.set_focus(is_value_slot(slot).then_some(slot));
        match slot {
            Slot::Mode => {
                self.scorecard.mode = Mode::ALL[i];
                self.scorecard.request(self.hwnd, false);
            }
            Slot::Load => return self.load_scorecard(true),
            Slot::Make => return self.make_scorecard_playlist(),
            // 두 칸 세그먼트와 값 칸 모두 왼쪽이 -1
            _ => {
                self.step_score_slot(slot, if i == 0 { -1 } else { 1 });
                // 값 칸은 누르고 있으면 계속 바뀜, 저장은 뗄 때 한 번
                if is_value_slot(slot) {
                    self.score_hold = Some((slot, i));
                    unsafe {
                        SetCapture(self.hwnd);
                        SetTimer(Some(self.hwnd), TIMER_REPEAT, REPEAT_DELAY_MS, None);
                    }
                    return Action::None;
                }
            }
        }
        self.refresh_scorecard();
        Action::Choice
    }

    /// 조건 줄 앞 체크를 누름
    pub(super) fn scorecard_check(&mut self, slot: Slot) -> Action {
        self.scorecard.set_focus(None);
        self.scorecard.toggle(slot);
        self.refresh_scorecard();
        Action::Choice
    }

    /// 입력 칸의 닉네임으로 기록을 받음, `again`이면 받아 둔 기록이 있어도 다시
    fn load_scorecard(&mut self, again: bool) -> Action {
        self.complete_composition();
        self.scorecard.nickname = self.edit.text().trim().to_owned();
        self.scorecard.request(self.hwnd, again);
        self.refresh_scorecard();
        Action::Choice
    }

    /// 조건에 맞는 패턴으로 임시 플레이리스트를 만들어 결과로 엶, 기록이 없으면 먼저 받음
    /// 시트 앞 검색어는 버리고 그 id를 검색어로 (히스토리의 플레이리스트 줄을 연 것과 같음)
    fn make_scorecard_playlist(&mut self) -> Action {
        self.complete_composition();
        self.scorecard.nickname = self.edit.text().trim().to_owned();
        let Some(picked) = self.scorecard.select(&self.catalog.songs) else { return self.load_scorecard(false) };
        if picked.is_empty() {
            return Action::None;
        }
        let data = self.scorecard.playlist(&self.catalog.songs, &picked);
        let id = data.id.clone();
        // 같은 조건으로 만든 적 있으면 히스토리 사본도 새 기록으로
        self.library.update(&data);
        self.playlist = Some(PlaylistView { id: id.clone(), data: Some(data), note: None, waiting: false, restore: None });
        self.scorecard.set_focus(None);
        self.list_edit = None;
        self.clear_input();
        self.edit.insert(&id);
        self.seen = self.query();
        self.view = View::Results;
        self.kb_focus = None;
        // 커서가 버튼 영역 밖이면 떠날 때처럼 버튼이 들어감 (`set_view`와 같음)
        if !self.in_zone && self.leave_since.is_none() {
            self.leave_since = Some(Instant::now());
        }
        self.searched.clear();
        self.search_now();
        Action::Choice
    }

    /// 성과표 시트의 키, 처리하지 않은 키 (입력 칸 편집, 포커스 없을 때 ←→)는 None
    pub(super) fn scorecard_key(&mut self, vk: VIRTUAL_KEY, shift: bool) -> Option<Action> {
        if self.view != View::Scorecard {
            return None;
        }
        let focus = self.scorecard.focus;
        // 값 칸에 직접 친 숫자: Esc는 취소, Backspace는 한 글자, Enter·←→는 반영만 (↑↓는 반영하고 옮김)
        let typing = focus.is_some_and(|f| self.scorecard.typed(f).is_some());
        if typing {
            match vk {
                VK_ESCAPE => {
                    self.scorecard.cancel_typed();
                    return Some(Action::None);
                }
                VK_BACK => {
                    self.scorecard.backspace();
                    return Some(Action::None);
                }
                VK_RETURN | VK_LEFT | VK_RIGHT => {
                    self.scorecard.commit();
                    self.refresh_scorecard();
                    return Some(Action::Choice);
                }
                _ => {}
            }
        }
        let act = match (vk, focus) {
            (VK_ESCAPE, _) => {
                self.set_view(View::Results);
                Action::Choice
            }
            // 맨 위에서 위로 가면 포커스를 풀고 입력 칸 (닉네임)으로
            (VK_UP, Some(Slot::Mode) | None) => {
                self.scorecard.set_focus(None);
                self.refresh_scorecard();
                if typing { Action::Choice } else { Action::None }
            }
            (VK_UP | VK_DOWN, f) => {
                let d = if vk == VK_UP { -1 } else { 1 };
                self.scorecard.set_focus(Some(f.map_or(Slot::Mode, |f| f.step(d))));
                self.refresh_scorecard();
                if typing { Action::Choice } else { Action::None }
            }
            (VK_LEFT | VK_RIGHT, Some(slot)) => {
                if self.scorecard.step(slot, if vk == VK_LEFT { -1 } else { 1 }, shift) {
                    self.scorecard.request(self.hwnd, false);
                }
                self.refresh_scorecard();
                Action::Choice
            }
            (VK_SPACE, Some(Slot::Load)) | (VK_RETURN, Some(Slot::Load)) => self.load_scorecard(true),
            (VK_SPACE, Some(slot)) if slot != Slot::Make => {
                self.scorecard.toggle(slot);
                self.refresh_scorecard();
                Action::Choice
            }
            // 기록을 받기 전이면 Enter가 불러오기, 포커스가 없으면 Space는 닉네임 글자
            (VK_RETURN, _) | (VK_SPACE, Some(Slot::Make)) => self.make_scorecard_playlist(),
            // 시트 뒤에 숨은 결과 줄을 움직이지 않게
            (VK_PRIOR | VK_NEXT, _) => Action::None,
            _ => return None,
        };
        Some(act)
    }

    /// 성과표 시트: 묶음 제목 아래 40 높이 줄, 라벨 칸 112 오른쪽에 컨트롤 (높이 30, 줄 가운데)
    /// 그리기와 판정이 같이 씀
    fn score_layout(&self) -> ScoreLayout {
        let top = self.panel_top();
        let y = |v: f32| top + self.px(v);
        let (l, r) = (self.px(lay::MARGIN + lay::INSET), self.px(lay::MARGIN + lay::W - lay::INSET));
        let x0 = l + self.px(112.0);
        let rows = [36.0, 76.0, 116.0, 188.0, 228.0, 268.0, 308.0];
        let ctl = |t: f32, x: f32, w: f32| rect(x, y(t + 5.0), x + self.px(w), y(t + 35.0));
        // 스코어·서열은 값 칸 둘 사이에 "~"
        let pair = |t: f32| (ctl(t, x0, 84.0), ctl(t, x0 + self.px(108.0), 84.0));
        let button = |label: &str, font: fn(&Fonts) -> &IDWriteTextFormat, t: f32, h: f32| {
            let w = (self.measure(label, font) + self.px(28.0)).round();
            rect(r - w, y(t), r, y(t + h))
        };
        ScoreLayout {
            titles: [y(14.0), y(166.0)],
            rows: rows.map(|t| (y(t), y(t + 40.0))),
            mode: ctl(rows[0], x0, 224.0),
            scope: ctl(rows[1], x0, 120.0),
            range: ctl(rows[1], x0 + self.px(128.0), 140.0),
            load: button(self.load_label(), |f| &f.item, rows[2] + 5.0, 30.0),
            checks: [3, 4, 5, 6].map(|i| rect(l, y(rows[i]), x0 - self.px(8.0), y(rows[i] + 40.0))),
            played: ctl(rows[3], x0, 140.0),
            score: pair(rows[4]),
            mc: ctl(rows[5], x0, 140.0),
            floor: pair(rows[6]),
            foot: y(360.0),
            make: button("플레이리스트 만들기", |f| &f.item_bold, 376.0, 34.0),
        }
    }

    fn load_label(&self) -> &'static str {
        if self.scorecard.board().is_some() { "다시 불러오기" } else { "불러오기" }
    }

    /// 성과표 시트에서 (`x`, `y`)의 칸, 다 편 시트가 보일 때만 부름 (`hit`)
    pub(super) fn scorecard_hit(&self, x: f32, y: f32) -> Hit {
        let sl = self.score_layout();
        for (r, slot, n) in [(sl.mode, Slot::Mode, Mode::ALL.len()), (sl.scope, Slot::Scope, 2), (sl.played, Slot::Played, 2), (sl.mc, Slot::MaxCombo, 2)] {
            if inside(&r, x, y) {
                return Hit::ScoreSlot(slot, segment_at(&r, x, n));
            }
        }
        // 값 칸은 왼쪽 절반이 줄이기, 오른쪽 절반이 늘리기
        let values = [(sl.range, Slot::Range), (sl.score.0, Slot::ScoreLo), (sl.score.1, Slot::ScoreHi), (sl.floor.0, Slot::FloorLo), (sl.floor.1, Slot::FloorHi)];
        for (r, slot) in values.into_iter().chain([(sl.load, Slot::Load), (sl.make, Slot::Make)]) {
            if inside(&r, x, y) {
                return Hit::ScoreSlot(slot, segment_at(&r, x, 2));
            }
        }
        for (r, slot) in sl.checks.into_iter().zip([Slot::Played, Slot::ScoreLo, Slot::MaxCombo, Slot::FloorLo]) {
            if inside(&r, x, y) {
                return Hit::ScoreCheck(slot);
            }
        }
        Hit::Inside
    }

    /// 그리는 결과가 성과표로 만든 플레이리스트, 레벨 칸 아랫줄이 Max·Perfect, 스코어, 서열 세 칸
    pub(super) fn local_playlist(&self) -> bool {
        self.sheet == Sheet::Results && self.playlist_data().is_some_and(|p| playlist::is_local(&p.id))
    }

    /// 성과표 플레이리스트 결과 `i`번째 줄의 레벨 칸 아랫줄 (`lx`부터, 글자 기준선 `base`)
    /// 왼쪽부터 Max·Perfect, 스코어, 서열 (모두 오른쪽 맞춤), 칸 너비는 고정이라 비어 있어도 자리는 그대로
    /// 성과표 플레이리스트가 아니면 안 그리고 false
    pub(super) fn draw_local_marks(&self, i: usize, lx: f32, base: f32, floor: Option<f32>, b: &ID2D1SolidColorBrush) -> Result<bool> {
        let Some((score, mark)) = self.local_playlist().then(|| self.playlist_entry(i)?.note.as_deref()).flatten().and_then(scorecard::note_marks) else {
            return Ok(false);
        };
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let floor = floor.map(|fl| format!("{fl:.1}")).unwrap_or_default();
        let mut x = lx;
        for (text, w) in [mark, score, floor.as_str()].into_iter().zip(COLS) {
            let w = self.px(w);
            if !text.is_empty() {
                let l = g.layout(text, &f.sub, w, self.px(16.0))?;
                unsafe {
                    l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                    g.dc.DrawTextLayout(gfx::pt(x, base - baseline(&l)), &l, b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
            x += w + self.px(COL_GAP);
        }
        Ok(true)
    }

    /// 성과표 시트 (다 편 시트 자리), `l0`·`r0`은 패널 안쪽 16 정렬선
    pub(super) fn draw_scorecard(&self, (l0, r0): (f32, f32)) -> Result<()> {
        let (g, f) = (self.gfx.as_ref().unwrap(), self.fonts.as_ref().unwrap());
        let dc = &g.dc;
        let s = self.scale;
        let sc = &self.scorecard;
        let c = &sc.cond;
        let (text_b, sub_b, faint_b) = (g.brush(TEXT)?, g.brush(SUB)?, g.brush(FAINT)?);
        let l0t = l0 - BEARING;
        let sl = self.score_layout();
        let lit = |slot: Slot| sc.focus == Some(slot) || matches!(self.hover, Hit::ScoreSlot(h, _) if h == slot);
        // 값 칸: 세그먼트 트랙 같은 채움에 가운데 글, 호버·포커스면 진하게
        let value = |r: D2D_RECT_F, text: &str, on: bool| -> Result<()> {
            unsafe {
                dc.FillRoundedRectangle(&rounded(r, self.px(8.0)), &g.brush(white(if on { FILL_STRONG } else { FILL_TRACK }))?);
                let l = g.layout_centered(text, &f.item_bold, r.right - r.left, r.bottom - r.top)?;
                dc.DrawTextLayout(gfx::pt(r.left, r.top), &l, &text_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            Ok(())
        };
        // 줄 안 세로 가운데 글, `x`부터 `w`까지
        let line = |text: &str, row: usize, x: f32, w: f32, b: &ID2D1SolidColorBrush| -> Result<()> {
            let (t, bottom) = sl.rows[row];
            let l = g.layout(text, &f.item, w, bottom - t)?;
            unsafe {
                l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                dc.DrawTextLayout(gfx::pt(x, t), &l, b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            Ok(())
        };
        unsafe {
            // 키보드 포커스 줄: 결과 줄 강조와 같은 모양
            if let Some(row) = sc.focus.and_then(score_row) {
                let (t, b) = sl.rows[row];
                let (pl, pr) = (self.px(lay::MARGIN + lay::PAD), self.px(lay::MARGIN + lay::W - lay::PAD));
                dc.FillRoundedRectangle(&rrect(pl, t, pr, b, self.px(lay::PANEL_R - lay::PAD)), &g.brush(white(FILL_HOVER))?);
            }
            self.text_line("기록 불러오기", &f.sub, sl.titles[0], self.px(16.0), &faint_b, (l0, r0))?;
            self.text_line("조건", &f.sub, sl.titles[1], self.px(16.0), &faint_b, (l0, r0))?;
            let label_w = sl.mode.left - l0;
            line("버튼", 0, l0t, label_w, &sub_b)?;
            draw_segments(g, f, sl.mode, &Mode::ALL.map(Mode::label), self.seg[SEG_SC_MODE].ends(), lay::SEG_R, s)?;
            line("범위", 1, l0t, label_w, &sub_b)?;
            draw_segments(g, f, sl.scope, &["전체", "범위"], self.seg[SEG_SCOPE].ends(), lay::SEG_R, s)?;
            // 전체를 고르면 범위 칸은 꺼진 조건처럼 흐리게, 값은 그대로 남음
            if sc.all {
                dc.PushLayer(&gfx::layer(0.38, None), None);
            }
            value(sl.range, &format!("‹   {}   ›", sc.range.label()), lit(Slot::Range))?;
            if sc.all {
                dc.PopLayer();
            }
            line("기록", 2, l0t, label_w, &sub_b)?;
            line(&sc.status(), 2, sl.mode.left, sl.load.left - self.px(12.0) - sl.mode.left, &faint_b)?;
            if sc.nickname.trim().is_empty() || sc.loading() {
                draw_button_off(g, &f.item, sl.load, self.load_label(), s)?;
            } else {
                draw_button(g, &f.item, sl.load, self.load_label(), lit(Slot::Load), s)?;
            }

            // 조건 줄: 체크, 라벨, 컨트롤 (꺼진 줄은 컨트롤을 흐리게)
            let conds = [(3, Slot::Played, "플레이", c.played_on), (4, Slot::ScoreLo, "스코어", c.score_on), (5, Slot::MaxCombo, "맥스콤보", c.mc_on), (6, Slot::FloorLo, "서열", c.floor_on)];
            for (row, slot, label, on) in conds {
                let (t, b) = sl.rows[row];
                let (cx, cy) = (l0 + self.px(9.0), (t + b) / 2.0);
                let d = self.px(9.0);
                let bx = rrect(cx - d, cy - d, cx + d, cy + d, self.px(5.0));
                if on {
                    dc.FillRoundedRectangle(&bx, &text_b);
                    let ink = g.brush(TINT)?;
                    let p = |x: f32, y: f32| gfx::pt(cx + x * s, cy + y * s);
                    dc.DrawLine(p(-4.5, 0.0), p(-1.5, 3.0), &ink, 2.0 * s, None);
                    dc.DrawLine(p(-1.5, 3.0), p(4.5, -3.5), &ink, 2.0 * s, None);
                } else {
                    dc.DrawRoundedRectangle(&bx, &if matches!(self.hover, Hit::ScoreCheck(h) if h == slot) { g.brush(TEXT)? } else { g.brush(SUB)? }, 1.5 * s, None);
                }
                line(label, row, l0t + self.px(28.0), label_w, if on { &text_b } else { &sub_b })?;
                if !on {
                    dc.PushLayer(&gfx::layer(0.38, None), None);
                }
                match slot {
                    Slot::Played => draw_segments(g, f, sl.played, &["함", "안 함"], self.seg[SEG_PLAYED].ends(), lay::SEG_R, s)?,
                    Slot::MaxCombo => draw_segments(g, f, sl.mc, &["O", "X"], self.seg[SEG_MC].ends(), lay::SEG_R, s)?,
                    _ => {
                        let (a, b2, texts) = if slot == Slot::ScoreLo {
                            (sl.score.0, sl.score.1, [scorecard::percent(c.score.0), scorecard::percent(c.score.1)])
                        } else {
                            (sl.floor.0, sl.floor.1, [scorecard::tenths(c.floor.0), scorecard::tenths(c.floor.1)])
                        };
                        let (lo, hi) = if slot == Slot::ScoreLo { (Slot::ScoreLo, Slot::ScoreHi) } else { (Slot::FloorLo, Slot::FloorHi) };
                        // 직접 치는 중이면 친 글 뒤에 밑줄 캐럿
                        let shown = |s: Slot, text: &str| sc.typed(s).map_or_else(|| text.to_owned(), |t| format!("{t}_"));
                        value(a, &shown(lo, &texts[0]), lit(lo))?;
                        value(b2, &shown(hi, &texts[1]), lit(hi))?;
                        let tl = g.layout_centered("~", &f.item, b2.left - a.right, a.bottom - a.top)?;
                        dc.DrawTextLayout(gfx::pt(a.right, a.top), &tl, &sub_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                }
                if !on {
                    dc.PopLayer();
                }
            }

            // 아래 칸: 구분선, 조건에 맞는 패턴 수, 만들기 버튼
            let (pl, pr) = (self.px(lay::MARGIN), self.px(lay::MARGIN + lay::W));
            dc.FillRectangle(&rect(pl, sl.foot, pr, sl.foot + s.max(1.0).round()), &g.brush(white(FILL_TRACK))?);
            let count = match self.scorecard_count {
                None => "기록을 먼저 불러와 주세요".to_owned(),
                Some((0, _)) => "조건에 맞는 패턴이 없어요".to_owned(),
                Some((n, 0)) => format!("{n}개 패턴"),
                Some((n, off)) => format!("{n}개 패턴  ·  내 목록에 없는 곡 {off}개"),
            };
            let cl = g.layout(&count, &f.item, sl.make.left - self.px(12.0) - l0, sl.make.bottom - sl.make.top)?;
            cl.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            dc.DrawTextLayout(gfx::pt(l0t, sl.make.top), &cl, &sub_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            if self.scorecard_count.is_some_and(|(n, _)| n > 0) {
                draw_button(g, &f.item_bold, sl.make, "플레이리스트 만들기", lit(Slot::Make), s)?;
            } else {
                draw_button_off(g, &f.item_bold, sl.make, "플레이리스트 만들기", s)?;
            }
        }
        Ok(())
    }
}
