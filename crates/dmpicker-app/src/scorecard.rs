//! V-ARCHIVE 성과표로 만드는 임시 플레이리스트: 기록 받기, 범위·조건 판정, 플레이리스트 만들기
//! 검색창 성과표 시트의 상태도 여기, 그리기와 키 처리 연결은 popup.rs

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use dmpicker_search::{Diff, Mode, Song};
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::playlist::{Entry, Pattern, Playlist, LOCAL_PREFIX};

/// 기록이 도착함, 검색창 창으로 옴 (main.rs·playlist.rs의 WM_APP 메시지와 안 겹치게)
pub const READY: u32 = WM_APP + 5;
const FAILED: &str = "기록을 불러오지 못했어요";
const NOT_FOUND: &str = "V-ARCHIVE에서 닉네임을 찾지 못했어요";
const LATER: &str = "잠시 뒤 다시 시도해 주세요";
/// 스코어 조건 값, 화살표로 이 사이를 오감
pub const SCORES: [f64; 12] = [0.0, 80.0, 90.0, 95.0, 97.0, 98.0, 99.0, 99.5, 99.8, 99.9, 99.99, 100.0];
/// 서열 조건으로 고를 수 있는 값 (0.1 단위), 서열표 소수 자리 규칙은 가정하지 않고 0.1씩
pub const FLOOR_MIN: u16 = 11;
pub const FLOOR_MAX: u16 = 209;
/// 서열 상한 기본값, 16 이상은 거의 안 찾음
const FLOOR_DEFAULT_MAX: u16 = 153;

/// 범위: 랜덤셀렉터 V-ARCHIVE Wizard와 같은 칸, 대상은 카탈로그의 그 모드 전체 패턴
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    /// SC가 아닌 레벨 n (1..=11)
    Level(u8),
    /// SC가 아닌 레벨 12 이상
    High,
    Sc,
    /// SC 레벨 1~5
    Sc5,
    /// SC 레벨 6~10
    Sc10,
    /// SC 레벨 11~15
    Sc15,
}

impl Range {
    pub const ALL: [Range; 16] = [
        Range::Level(1),
        Range::Level(2),
        Range::Level(3),
        Range::Level(4),
        Range::Level(5),
        Range::Level(6),
        Range::Level(7),
        Range::Level(8),
        Range::Level(9),
        Range::Level(10),
        Range::Level(11),
        Range::High,
        Range::Sc,
        Range::Sc5,
        Range::Sc10,
        Range::Sc15,
    ];

    pub fn label(self) -> String {
        match self {
            Range::Level(n) => format!("{n}L"),
            Range::High => "12L~15L".into(),
            Range::Sc => "SC".into(),
            Range::Sc5 => "SC~5".into(),
            Range::Sc10 => "SC~10".into(),
            Range::Sc15 => "SC~15".into(),
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.label() == s)
    }

    /// 패턴 하나가 이 범위인지, 레벨 미정 패턴 (0)은 SC 전부에만 듦
    pub fn contains(self, diff: Diff, level: u8) -> bool {
        let sc = diff == Diff::SC;
        match self {
            Range::Level(n) => !sc && level == n,
            Range::High => !sc && level >= 12,
            Range::Sc => sc,
            Range::Sc5 => sc && (1..=5).contains(&level),
            Range::Sc10 => sc && (6..=10).contains(&level),
            Range::Sc15 => sc && level >= 11,
        }
    }

    fn step(self, d: i32) -> Self {
        let i = Self::ALL.iter().position(|&r| r == self).unwrap_or(0) as i32;
        Self::ALL[(i + d).clamp(0, Self::ALL.len() as i32 - 1) as usize]
    }
}

/// 패턴 하나의 기록
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub score: f64,
    pub max_combo: bool,
    /// 기록한 날 "2025-12-31", 없으면 빈 글
    pub date: String,
}

/// (곡 id, 난이도)별 기록, 한 버튼 모드 것
pub type Records = HashMap<(u32, Diff), Record>;

/// 조건, 꺼진 조건은 판정에서 빠지고 값은 그대로 둠
#[derive(Clone, Debug, PartialEq)]
pub struct Cond {
    pub played_on: bool,
    /// 기록이 있어야 함 (false면 없어야 함)
    pub played: bool,
    pub score_on: bool,
    /// 이상, 이하 (%)
    pub score: (f64, f64),
    pub mc_on: bool,
    pub mc: bool,
    pub floor_on: bool,
    /// 이상, 이하 (0.1 단위)
    pub floor: (u16, u16),
}

/// 랜덤셀렉터 기본값과 같음: 맥스콤보 X만 켜짐
impl Default for Cond {
    fn default() -> Self {
        Self { played_on: false, played: false, score_on: false, score: (0.0, 99.99), mc_on: true, mc: false, floor_on: false, floor: (FLOOR_MIN, FLOOR_DEFAULT_MAX) }
    }
}

impl Cond {
    /// 기록·서열이 켠 조건에 다 맞음, 스코어·맥스콤보는 기록이 있어야, 서열은 서열이 있어야 통과
    pub fn matches(&self, record: Option<&Record>, floor: Option<f32>) -> bool {
        (!self.played_on || record.is_some() == self.played)
            && (!self.score_on || record.is_some_and(|r| self.score.0 <= r.score && r.score <= self.score.1))
            && (!self.mc_on || record.is_some_and(|r| r.max_combo == self.mc))
            && (!self.floor_on || floor.is_some_and(|f| (self.floor.0..=self.floor.1).contains(&((f * 10.0).round() as u16))))
    }

    /// 켠 조건만 플레이리스트 이름에
    fn describe(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.played_on {
            parts.push(if self.played { "플레이 함" } else { "플레이 안 함" }.into());
        }
        if self.score_on {
            parts.push(match self.score {
                (lo, hi) if lo <= 0.0 => format!("{} 이하", percent(hi)),
                (lo, hi) if hi >= 100.0 => format!("{} 이상", percent(lo)),
                (lo, hi) => format!("{}~{}", percent(lo), percent(hi)),
            });
        }
        if self.mc_on {
            parts.push(if self.mc { "MC O" } else { "MC X" }.into());
        }
        if self.floor_on {
            parts.push(format!("서열 {}~{}", tenths(self.floor.0), tenths(self.floor.1)));
        }
        parts
    }
}

pub fn percent(v: f64) -> String {
    format!("{v:.2}%")
}

pub fn tenths(v: u16) -> String {
    format!("{}.{}", v / 10, v % 10)
}

/// 앞 값은 `SCORES`에서 다음·이전 값으로
fn step_score(v: f64, d: i32) -> f64 {
    let next = if d > 0 { SCORES.iter().find(|&&s| s > v + 1e-9) } else { SCORES.iter().rev().find(|&&s| s < v - 1e-9) };
    next.copied().unwrap_or(v)
}

/// 시트의 키보드 포커스 칸, 위에서 아래 순서
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Mode,
    /// 범위 줄의 전체·범위 세그먼트
    Scope,
    Range,
    Load,
    Played,
    ScoreLo,
    ScoreHi,
    MaxCombo,
    FloorLo,
    FloorHi,
    Make,
}

impl Slot {
    pub const ALL: [Slot; 11] = [Slot::Mode, Slot::Scope, Slot::Range, Slot::Load, Slot::Played, Slot::ScoreLo, Slot::ScoreHi, Slot::MaxCombo, Slot::FloorLo, Slot::FloorHi, Slot::Make];

    pub fn step(self, d: i32) -> Self {
        let i = Self::ALL.iter().position(|&s| s == self).unwrap_or(0) as i32;
        Self::ALL[(i + d).clamp(0, Self::ALL.len() as i32 - 1) as usize]
    }
}

/// 받은 기록과 그때
pub struct Board {
    pub records: Records,
    pub at: Instant,
}

/// 작업 스레드가 보내는 결과
struct Reply {
    key: (String, Mode),
    result: Result<Records, String>,
}

/// 성과표 시트 상태, 닉네임·모드·범위·조건은 설정 파일에 저장
pub struct Scorecard {
    /// 시트가 열려 있는 동안엔 입력 칸 글, 닫을 때와 불러올 때 여기에 맞춤
    pub nickname: String,
    pub mode: Mode,
    pub range: Range,
    /// 범위 대신 그 모드 전체 패턴, 범위는 그대로 둬서 다시 고르면 이어 씀
    pub all: bool,
    pub cond: Cond,
    /// 키보드 포커스, 마우스로 누르면 풂
    pub focus: Option<Slot>,
    /// 포커스 칸에 직접 치고 있는 숫자, Enter나 포커스를 옮기면 반영 (`commit`)
    typed: Option<String>,
    /// (닉네임, 모드)별 받은 기록, 이번 실행 동안만
    boards: HashMap<(String, Mode), Board>,
    pending: HashSet<(String, Mode)>,
    /// (닉네임, 모드)별 마지막 실패 문구
    failed: HashMap<(String, Mode), String>,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
}

impl Default for Scorecard {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            nickname: String::new(),
            mode: Mode::B4,
            range: Range::Sc15,
            all: false,
            cond: Cond::default(),
            focus: None,
            typed: None,
            boards: HashMap::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
            tx,
            rx,
        }
    }
}

impl Scorecard {
    fn key(&self) -> (String, Mode) {
        (self.nickname.trim().to_owned(), self.mode)
    }

    /// 지금 닉네임·모드의 받은 기록
    pub fn board(&self) -> Option<&Board> {
        self.boards.get(&self.key())
    }

    pub fn loading(&self) -> bool {
        self.pending.contains(&self.key())
    }

    /// 기록 줄 문구: 받는 중, 실패 이유, 받은 개수와 그때
    pub fn status(&self) -> String {
        if self.nickname.trim().is_empty() {
            return "위 칸에 V-ARCHIVE 닉네임을 입력해 주세요".into();
        }
        if self.loading() {
            return "불러오는 중…".into();
        }
        if let Some(why) = self.failed.get(&self.key()) {
            return why.clone();
        }
        match self.board() {
            Some(b) => {
                let mins = b.at.elapsed().as_secs() / 60;
                let ago = if mins == 0 { "방금".into() } else { format!("{mins}분 전") };
                format!("{} 기록 {}개 · {ago}", self.mode.label(), b.records.len())
            }
            None => "Enter를 누르면 기록을 불러와요".into(),
        }
    }

    /// 기록을 작업 스레드에서 받음, 응답이 오면 `hwnd`에 [`READY`]
    /// `again`이 아니면 받아 둔 기록이 있을 때 안 받음
    pub fn request(&mut self, hwnd: HWND, again: bool) {
        let key = self.key();
        if key.0.is_empty() || (!again && self.boards.contains_key(&key)) || !self.pending.insert(key.clone()) {
            return;
        }
        self.failed.remove(&key);
        let (tx, hwnd) = (self.tx.clone(), hwnd.0 as isize);
        let failed_key = key.clone();
        let spawned = std::thread::Builder::new().spawn(move || {
            let result = fetch(&key.0, key.1);
            let _ = tx.send(Reply { key, result });
            notify(hwnd);
        });
        if spawned.is_err() {
            let _ = self.tx.send(Reply { key: failed_key, result: Err(FAILED.into()) });
            notify(hwnd);
        }
    }

    /// 도착한 응답을 넣음
    pub fn take(&mut self) {
        while let Ok(reply) = self.rx.try_recv() {
            self.pending.remove(&reply.key);
            match reply.result {
                Ok(records) => {
                    self.failed.remove(&reply.key);
                    self.boards.insert(reply.key, Board { records, at: Instant::now() });
                }
                Err(why) => {
                    self.failed.insert(reply.key, why);
                }
            }
        }
    }

    /// 칸 값을 왼쪽 (-1)·오른쪽 (+1)으로, 서열은 `big`이면 1씩, 모드가 바뀌면 true (기록을 받아야 할 수 있음)
    pub fn step(&mut self, slot: Slot, d: i32, big: bool) -> bool {
        self.typed = None;
        let (score, floor) = (self.cond.score, self.cond.floor);
        let next_floor = |v: u16| (v as i32 + d * if big { 10 } else { 1 }) as f64 / 10.0;
        match slot {
            Slot::Mode => {
                let i = (self.mode.index() as i32 + d).rem_euclid(4) as usize;
                self.mode = Mode::ALL[i];
                return true;
            }
            Slot::Scope => self.all = d < 0, // 왼쪽 칸이 "전체"
            // 범위 값을 바꾸면 범위로 고르는 쪽으로
            Slot::Range => {
                self.range = self.range.step(d);
                self.all = false;
            }
            Slot::Played => self.cond.played = d < 0, // 왼쪽 칸이 "함"
            Slot::MaxCombo => self.cond.mc = d < 0,   // 왼쪽 칸이 "O"
            Slot::ScoreLo => self.put(slot, step_score(score.0, d)),
            Slot::ScoreHi => self.put(slot, step_score(score.1, d)),
            Slot::FloorLo => self.put(slot, next_floor(floor.0)),
            Slot::FloorHi => self.put(slot, next_floor(floor.1)),
            Slot::Load | Slot::Make => {}
        }
        // 꺼진 조건의 값을 바꾸면 그 조건을 켬
        if let Some(on) = self.check(slot) {
            *on = true;
        }
        false
    }

    /// 스코어 (%)·서열 칸에 값을 넣음, 범위 밖이면 맞추고 이상·이하가 엇갈리면 다른 쪽을 따라 옮김
    fn put(&mut self, slot: Slot, v: f64) {
        let c = &mut self.cond;
        let score = |v: f64| (v.clamp(0.0, 100.0) * 100.0).round() / 100.0;
        let floor = |v: f64| ((v * 10.0).round() as i64).clamp(FLOOR_MIN as i64, FLOOR_MAX as i64) as u16;
        match slot {
            Slot::ScoreLo => {
                c.score.0 = score(v);
                c.score.1 = c.score.1.max(c.score.0);
            }
            Slot::ScoreHi => {
                c.score.1 = score(v);
                c.score.0 = c.score.0.min(c.score.1);
            }
            Slot::FloorLo => {
                c.floor.0 = floor(v);
                c.floor.1 = c.floor.1.max(c.floor.0);
            }
            Slot::FloorHi => {
                c.floor.1 = floor(v);
                c.floor.0 = c.floor.0.min(c.floor.1);
            }
            _ => {}
        }
    }

    /// 포커스를 옮김, 치던 숫자는 먼저 반영
    pub fn set_focus(&mut self, focus: Option<Slot>) {
        self.commit();
        self.focus = focus;
    }

    /// 스코어·서열 칸에 포커스가 있으면 숫자와 `.`를 받음, 받았으면 true
    pub fn type_char(&mut self, c: char) -> bool {
        let number_slot = matches!(self.focus, Some(Slot::ScoreLo | Slot::ScoreHi | Slot::FloorLo | Slot::FloorHi));
        if !number_slot || !(c.is_ascii_digit() || c == '.') {
            return false;
        }
        let typed = self.typed.get_or_insert_with(String::new);
        if typed.len() < 6 {
            typed.push(c);
        }
        true
    }

    /// 치던 숫자를 한 글자 지움, 치던 게 없으면 false
    pub fn backspace(&mut self) -> bool {
        let Some(typed) = &mut self.typed else { return false };
        typed.pop();
        if typed.is_empty() {
            self.typed = None;
        }
        true
    }

    /// 치던 숫자를 버림, 치던 게 없으면 false
    pub fn cancel_typed(&mut self) -> bool {
        self.typed.take().is_some()
    }

    /// 치던 숫자를 포커스 칸 값으로, 반영했으면 true (못 읽는 글은 버림)
    pub fn commit(&mut self) -> bool {
        let (Some(typed), Some(slot)) = (self.typed.take(), self.focus) else { return false };
        let Ok(v) = typed.parse::<f64>() else { return false };
        self.put(slot, v);
        if let Some(on) = self.check(slot) {
            *on = true;
        }
        true
    }

    /// `slot` 칸에 치고 있는 숫자
    pub fn typed(&self, slot: Slot) -> Option<&str> {
        self.typed.as_deref().filter(|_| self.focus == Some(slot))
    }

    /// 조건 칸이 든 줄의 체크
    fn check(&mut self, slot: Slot) -> Option<&mut bool> {
        let c = &mut self.cond;
        Some(match slot {
            Slot::Played => &mut c.played_on,
            Slot::ScoreLo | Slot::ScoreHi => &mut c.score_on,
            Slot::MaxCombo => &mut c.mc_on,
            Slot::FloorLo | Slot::FloorHi => &mut c.floor_on,
            _ => return None,
        })
    }

    /// 조건 줄의 체크를 바꿈, 조건 칸이 아니면 false
    pub fn toggle(&mut self, slot: Slot) -> bool {
        let Some(on) = self.check(slot) else { return false };
        *on = !*on;
        true
    }

    /// 받은 기록으로 고른 패턴, 기록이 없으면 None
    pub fn select(&self, songs: &[Song]) -> Option<Vec<(usize, Diff)>> {
        let range = (!self.all).then_some(self.range);
        Some(select(songs, self.mode, range, &self.board()?.records, &self.cond))
    }

    /// 고른 패턴으로 만든 플레이리스트, 같은 닉네임·모드·범위·조건이면 같은 id
    pub fn playlist(&self, songs: &[Song], picked: &[(usize, Diff)]) -> Playlist {
        let records = self.board().map(|b| &b.records);
        let entries = picked
            .iter()
            .map(|&(i, diff)| {
                let id = songs[i].id;
                let record = records.and_then(|r| r.get(&(id, diff)));
                Entry {
                    pattern: Pattern { title: id, mode: self.mode, diff },
                    note: Some(note(record)),
                    effect: None,
                    target_score: self.cond.score_on.then_some(self.cond.score.1),
                }
            })
            .collect();
        let range = if self.all { "전체".to_owned() } else { self.range.label() };
        let mut name = format!("성과표 · {} {range}", self.mode.label());
        for part in self.cond.describe() {
            name.push_str(" · ");
            name.push_str(&part);
        }
        let c = &self.cond;
        let key = format!(
            "{}|{}|{}|{} {} {} {:?} {} {} {} {:?}",
            self.nickname.trim(),
            self.mode.label(),
            range,
            c.played_on,
            c.played,
            c.score_on,
            c.score,
            c.mc_on,
            c.mc,
            c.floor_on,
            c.floor
        );
        Playlist { id: format!("{LOCAL_PREFIX}{:08x}", fnv(&key)), name, updated_at: String::new(), entries }
    }

    /// 설정 파일에 넣는 값
    pub fn to_json(&self) -> Value {
        let c = &self.cond;
        json!({
            "nickname": self.nickname.trim(),
            "mode": self.mode.label(),
            "range": self.range.label(),
            "range_all": self.all,
            "played_on": c.played_on, "played": c.played,
            "score_on": c.score_on, "score": [c.score.0, c.score.1],
            "mc_on": c.mc_on, "mc": c.mc,
            "floor_on": c.floor_on, "floor": [c.floor.0, c.floor.1],
        })
    }

    /// 설정 파일 값, 없거나 못 읽는 값은 그대로
    pub fn apply_json(&mut self, v: &Value) {
        if let Some(n) = v["nickname"].as_str() {
            self.nickname = n.to_owned();
        }
        if let Some(m) = v["mode"].as_str().and_then(Mode::from_label) {
            self.mode = m;
        }
        if let Some(r) = v["range"].as_str().and_then(Range::from_label) {
            self.range = r;
        }
        if let Some(b) = v["range_all"].as_bool() {
            self.all = b;
        }
        let c = &mut self.cond;
        for (key, slot) in [("played_on", &mut c.played_on), ("played", &mut c.played), ("score_on", &mut c.score_on), ("mc_on", &mut c.mc_on), ("mc", &mut c.mc), ("floor_on", &mut c.floor_on)] {
            if let Some(b) = v[key].as_bool() {
                *slot = b;
            }
        }
        if let (Some(lo), Some(hi)) = (v["score"][0].as_f64(), v["score"][1].as_f64()) {
            if (0.0..=hi).contains(&lo) && hi <= 100.0 {
                c.score = (lo, hi);
            }
        }
        if let (Some(lo), Some(hi)) = (v["floor"][0].as_u64(), v["floor"][1].as_u64()) {
            // 범위를 바꾸기 전 버전이 저장한 0.0도 받아서 최소값으로 맞춤
            let clamp = |v: u64| v.clamp(FLOOR_MIN as u64, FLOOR_MAX as u64) as u16;
            if lo <= hi {
                c.floor = (clamp(lo), clamp(hi));
            }
        }
    }
}

/// 범위 안 패턴 중 조건에 맞는 것 (`songs` 인덱스, 난이도), 범위가 None이면 그 모드 전체 패턴
/// 서열 (없으면 레벨) → 곡 이름 → 난이도 순
pub fn select(songs: &[Song], mode: Mode, range: Option<Range>, records: &Records, cond: &Cond) -> Vec<(usize, Diff)> {
    let mut picked: Vec<(usize, Diff)> = songs
        .iter()
        .enumerate()
        .flat_map(|(i, s)| {
            Diff::ALL.into_iter().filter_map(move |d| {
                let level = s.level(mode, d)?;
                (range.is_none_or(|r| r.contains(d, level)) && cond.matches(records.get(&(s.id, d)), s.floor(mode, d))).then_some((i, d))
            })
        })
        .collect();
    let order = |&(i, d): &(usize, Diff)| {
        let s = &songs[i];
        s.floor(mode, d).unwrap_or_else(|| s.level(mode, d).unwrap_or(0) as f32)
    };
    picked.sort_by(|a, b| order(a).total_cmp(&order(b)).then_with(|| songs[a.0].name.cmp(&songs[b.0].name)).then(a.1.cmp(&b.1)));
    picked
}

/// 항목 코멘트: "현재 96.73% · MC · 2025-12-31" 또는 "기록 없음"
fn note(record: Option<&Record>) -> String {
    let Some(r) = record else { return NO_RECORD.into() };
    let mut s = format!("현재 {}", percent(r.score));
    if r.max_combo {
        s.push_str(" · MC");
    }
    if !r.date.is_empty() {
        s.push_str(" · ");
        s.push_str(&r.date);
    }
    s
}

const NO_RECORD: &str = "기록 없음";

/// 결과 줄 오른쪽 아래 칸의 (스코어, 표시), `note`가 만든 코멘트에서 읽음
/// 표시는 퍼펙트 (100%) "Perfect", 맥스콤보 "Max", 둘 다 아니면 빈 글, 기록이 없으면 ("NEW", "")
pub fn note_marks(note: &str) -> Option<(&str, &'static str)> {
    if note == NO_RECORD {
        return Some(("NEW", ""));
    }
    let mut parts = note.strip_prefix("현재 ")?.split(" · ");
    let score = parts.next()?;
    let mark = match (score, parts.next()) {
        ("100.00%", _) => "Perfect",
        (_, Some("MC")) => "Max",
        _ => "",
    };
    Some((score, mark))
}

/// 사본 파일·히스토리에 남는 id라 실행마다 같아야 함 (FNV-1a)
fn fnv(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

/// `HWND`는 스레드로 못 넘겨서 정수로 받음
fn notify(hwnd: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as _)), READY, WPARAM(0), LPARAM(0));
    }
}

/// 요청 경로에 넣는 닉네임 (한글은 UTF-8 퍼센트 인코딩)
/// `.`도 인코딩해서 ".." 닉네임이 경로를 거슬러 올라가지 않게
fn encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn fetch(nickname: &str, mode: Mode) -> Result<Records, String> {
    let button = mode.label().trim_end_matches('B');
    let url = format!("https://v-archive.net/api/v2/archive/{}/button/{button}", encode(nickname));
    let bytes = crate::http::varchive(&url).map_err(|error| match error.status {
        Some(404) => NOT_FOUND,
        Some(429 | 503) => LATER,
        _ => FAILED,
    })?;
    parse(&bytes)
}

/// 응답의 기록, `success`가 아니면 V-ARCHIVE 문구, 읽을 수 없는 기록은 건너뜀
fn parse(bytes: &[u8]) -> Result<Records, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| FAILED.to_owned())?;
    if v["success"].as_bool() != Some(true) {
        // 서버가 준 문구는 한 줄 자리에 그대로 보여서 길이를 자름
        let message = v["message"].as_str().map(str::trim).filter(|m| !m.is_empty()).unwrap_or(FAILED);
        return Err(message.chars().filter(|c| !c.is_control()).take(60).collect());
    }
    let records = v["records"].as_array().ok_or_else(|| FAILED.to_owned())?;
    Ok(records
        .iter()
        .filter_map(|r| {
            let title = u32::try_from(r["title"].as_u64()?).ok()?;
            let diff = Diff::from_label(r["pattern"].as_str()?)?;
            let score = r["score"].as_f64().or_else(|| r["score"].as_str()?.parse().ok())?;
            let max_combo = r["maxCombo"].as_bool().or_else(|| r["maxCombo"].as_i64().map(|n| n != 0)).unwrap_or(false);
            let date = r["updatedAt"].as_str().and_then(|s| s.get(..10)).unwrap_or_default().to_owned();
            Some(((title, diff), Record { score, max_combo, date }))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(score: f64, max_combo: bool) -> Record {
        Record { score, max_combo, date: String::new() }
    }

    #[test]
    fn max_combo_needs_record() {
        let c = Cond::default(); // 맥스콤보 X만 켜짐
        assert!(!c.matches(None, None));
        assert!(c.matches(Some(&record(98.0, false)), None));
        assert!(!c.matches(Some(&record(100.0, true)), None));
        // 플레이 안 함만 켜면 기록 없는 패턴이 통과
        let c = Cond { mc_on: false, played_on: true, ..Cond::default() };
        assert!(c.matches(None, None));
        assert!(!c.matches(Some(&record(98.0, false)), None));
    }

    #[test]
    fn score_bounds_inclusive() {
        let c = Cond { mc_on: false, score_on: true, score: (99.0, 99.99), ..Cond::default() };
        assert!(c.matches(Some(&record(99.0, false)), None));
        assert!(c.matches(Some(&record(99.99, false)), None));
        assert!(!c.matches(Some(&record(98.99, false)), None));
        assert!(!c.matches(Some(&record(100.0, true)), None));
        assert!(!c.matches(None, None));
    }

    #[test]
    fn floor_needs_floor() {
        let c = Cond { mc_on: false, floor_on: true, floor: (141, 153), ..Cond::default() };
        assert!(c.matches(None, Some(14.1)));
        assert!(c.matches(None, Some(15.3)));
        assert!(!c.matches(None, Some(15.4)));
        assert!(!c.matches(None, None));
    }

    #[test]
    fn range_sc10() {
        let r = Range::Sc10;
        assert!(r.contains(Diff::SC, 6) && r.contains(Diff::SC, 10));
        assert!(!r.contains(Diff::SC, 5) && !r.contains(Diff::SC, 11));
        assert!(!r.contains(Diff::MX, 8));
        assert!(Range::High.contains(Diff::MX, 13) && !Range::High.contains(Diff::SC, 13));
        assert_eq!(Range::from_label("12L~15L"), Some(Range::High));
    }

    #[test]
    fn parse_response() {
        let ok = br#"{"success":true,"records":[{"title":2,"pattern":"NM","score":96.73,"maxCombo":true,"updatedAt":"2025-12-31T00:16:55.000Z"},{"title":"x"}]}"#;
        let r = parse(ok).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[&(2, Diff::NM)], Record { score: 96.73, max_combo: true, date: "2025-12-31".into() });
        assert_eq!(parse(r#"{"success":false,"message":"없는 유저"}"#.as_bytes()), Err("없는 유저".into()));
        assert_eq!(note_marks(&note(r.get(&(2, Diff::NM)))), Some(("96.73%", "Max")));
        assert_eq!(note_marks(&note(Some(&record(100.0, true)))), Some(("100.00%", "Perfect")));
        assert_eq!(note_marks(&note(Some(&Record { score: 99.0, max_combo: false, date: "2025-01-01".into() }))), Some(("99.00%", "")));
        assert_eq!(note_marks(&note(None)), Some(("NEW", "")));
        assert_eq!(encode("지한 a"), "%EC%A7%80%ED%95%9C%20a");
    }

    /// 실제 V-ARCHIVE에 요청함: cargo test -- --ignored
    #[test]
    #[ignore]
    fn fetch_live() {
        let records = fetch("지한가", Mode::B4).unwrap();
        assert!(records.len() > 100);
        assert_eq!(fetch("zz_no_such_user_zz", Mode::B4), Err(NOT_FOUND.into()));
    }

    #[test]
    fn local_id() {
        let id = Scorecard::default().playlist(&[], &[]).id;
        assert!(crate::playlist::is_local(&id));
        assert_eq!(crate::playlist::id(&id), Some(id.as_str()));
        // "va-"로 시작하는 곡 검색어는 플레이리스트가 아님
        assert_eq!(crate::playlist::id("va-1"), None);
        assert_eq!(encode(".."), "%2E%2E");
    }

    #[test]
    fn floor_limits() {
        let mut s = Scorecard::default();
        assert_eq!(s.cond.floor, (11, 153));
        s.step(Slot::FloorLo, -1, true);
        assert_eq!(s.cond.floor.0, FLOOR_MIN);
        for _ in 0..10 {
            s.step(Slot::FloorHi, 1, true);
        }
        assert_eq!(s.cond.floor.1, FLOOR_MAX);
        s.apply_json(&json!({ "floor": [0, 159] }));
        assert_eq!(s.cond.floor, (FLOOR_MIN, 159));
    }

    #[test]
    fn select_all_with_floor() {
        // 4B NM 5 (서열 없음), MX 13 (서열 13.2), SC 14 (서열 14.1)
        let mut song = Song { id: 7, name: "곡".into(), ..Song::default() };
        song.levels[0] = [Some(5), None, Some(13), Some(14)];
        song.floors[0] = [None, None, Some(13.2), Some(14.1)];
        let songs = [song];
        let records = Records::new();
        let cond = Cond { mc_on: false, floor_on: true, floor: (130, 141), ..Cond::default() };
        // 전체 + 서열 조건은 난이도와 상관없이 서열이 있는 패턴, 서열 순
        assert_eq!(select(&songs, Mode::B4, None, &records, &cond), [(0, Diff::MX), (0, Diff::SC)]);
        assert_eq!(select(&songs, Mode::B4, Some(Range::Sc), &records, &cond), [(0, Diff::SC)]);
        // 조건 없이 전체면 모든 패턴
        let cond = Cond { mc_on: false, ..Cond::default() };
        assert_eq!(select(&songs, Mode::B4, None, &records, &cond).len(), 3);

        let mut s = Scorecard::default();
        s.step(Slot::Scope, -1, false);
        assert!(s.all);
        assert!(s.playlist(&[], &[]).name.starts_with("성과표 · 4B 전체"));
        // 범위 값을 바꾸면 범위로 돌아가고 고르던 범위에서 이어감
        s.step(Slot::Range, -1, false);
        assert!(!s.all);
        assert_eq!(s.range, Range::Sc10);
    }

    #[test]
    fn typed_numbers() {
        let mut s = Scorecard::default();
        s.set_focus(Some(Slot::FloorHi));
        assert!(!s.type_char('a'));
        "9.3".chars().for_each(|c| assert!(s.type_char(c)));
        assert_eq!(s.typed(Slot::FloorHi), Some("9.3"));
        s.set_focus(Some(Slot::FloorLo)); // 포커스를 옮기면 반영
        assert_eq!(s.cond.floor, (FLOOR_MIN, 93));
        assert!(s.cond.floor_on);
        // 상한보다 큰 하한은 상한도 같이 올림, 범위 밖은 맞춤
        "25".chars().for_each(|c| assert!(s.type_char(c)));
        assert!(s.commit());
        assert_eq!(s.cond.floor, (FLOOR_MAX, FLOOR_MAX));
        // 스코어는 0.01 단위, 못 읽는 글은 버림
        s.set_focus(Some(Slot::ScoreHi));
        "99.456".chars().for_each(|c| assert!(s.type_char(c)));
        s.commit();
        assert_eq!(s.cond.score.1, 99.46);
        assert!(s.type_char('.'));
        assert!(!s.commit());
        assert_eq!(s.cond.score.1, 99.46);
        // 세그먼트 칸에선 숫자를 안 받음 (닉네임 글자로)
        s.set_focus(Some(Slot::Mode));
        assert!(!s.type_char('4'));
    }

    #[test]
    fn score_steps() {
        assert_eq!(step_score(99.5, 1), 99.8);
        assert_eq!(step_score(99.5, -1), 99.0);
        assert_eq!(step_score(100.0, 1), 100.0);
        assert_eq!(step_score(96.0, 1), 97.0);
    }
}
