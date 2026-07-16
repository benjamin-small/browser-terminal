//! The multiplexer data model: sessions → windows → a layout tree of panes.
//! Pure tree/layout logic — the engine layer owns event emission and editor
//! lifecycles, driven by the outcome structs returned here.

pub mod keys;

use crate::editor::LineEditor;
use crate::signature::Scope;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub type PaneId = u32;
pub type WindowId = u32;
pub type SessionId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// Children side by side (a `split --right`).
    Row,
    /// Children stacked (a `split --down`).
    Col,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayoutNode {
    Leaf(PaneId),
    Split { dir: Dir, children: Vec<(f32, LayoutNode)> },
}

/// Fractional rectangle in [0,1] pane-container space. TS turns these into
/// percentages; Rust never sees pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const FULL: Rect = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

pub struct PaneShell {
    pub editor: LineEditor,
    pub cols: u16,
    pub rows: u16,
    /// A pipeline task is in flight. (The abort handle lives host-side.)
    pub running: bool,
}

impl PaneShell {
    fn new() -> Self {
        PaneShell { editor: LineEditor::new(), cols: 80, rows: 24, running: false }
    }
}

pub struct Window {
    pub id: WindowId,
    pub name: String,
    pub layout: LayoutNode,
    pub active_pane: PaneId,
    pub zoomed: Option<PaneId>,
}

pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub windows: IndexMap<WindowId, Window>,
    pub active_window: WindowId,
    pub vars: Scope,
}

pub struct Mux {
    pub sessions: IndexMap<SessionId, Session>,
    pub active_session: SessionId,
    pub panes: IndexMap<PaneId, PaneShell>,
    next_id: u32,
}

/// What a mutation did — the engine turns this into events and host-side
/// task/xterm lifecycle changes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MuxOutcome {
    pub opened_panes: Vec<PaneId>,
    pub closed_panes: Vec<PaneId>,
    pub closed_sessions: Vec<SessionId>,
    pub layout_changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDir {
    Next,
    Left,
    Right,
    Up,
    Down,
}

impl Default for Mux {
    fn default() -> Self {
        Self::new()
    }
}

impl Mux {
    /// A mux always has at least one session/window/pane.
    pub fn new() -> Self {
        let mut mux = Mux {
            sessions: IndexMap::new(),
            active_session: 0,
            panes: IndexMap::new(),
            next_id: 0,
        };
        let sid = mux.create_session("main".to_string());
        mux.active_session = sid;
        mux
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn create_pane(&mut self) -> PaneId {
        let id = self.next_id();
        self.panes.insert(id, PaneShell::new());
        id
    }

    fn create_window(&mut self, name: String) -> Window {
        let pane = self.create_pane();
        Window {
            id: self.next_id(),
            name,
            layout: LayoutNode::Leaf(pane),
            active_pane: pane,
            zoomed: None,
        }
    }

    fn create_session(&mut self, name: String) -> SessionId {
        let window = self.create_window("main".to_string());
        let wid = window.id;
        let sid = self.next_id();
        let mut windows = IndexMap::new();
        windows.insert(wid, window);
        self.sessions.insert(
            sid,
            Session { id: sid, name, windows, active_window: wid, vars: Scope::new() },
        );
        sid
    }

    // --- accessors ---

    pub fn active_session(&self) -> &Session {
        &self.sessions[&self.active_session]
    }

    pub fn active_window(&self) -> &Window {
        let s = self.active_session();
        &s.windows[&s.active_window]
    }

    pub fn active_pane(&self) -> PaneId {
        self.active_window().active_pane
    }

    pub fn pane(&self, id: PaneId) -> Option<&PaneShell> {
        self.panes.get(&id)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut PaneShell> {
        self.panes.get_mut(&id)
    }

    /// The session that owns a pane (panes never move across sessions).
    pub fn session_of_pane(&self, pane: PaneId) -> Option<SessionId> {
        self.sessions
            .values()
            .find(|s| s.windows.values().any(|w| leaves(&w.layout).contains(&pane)))
            .map(|s| s.id)
    }

    fn active_window_mut(&mut self) -> &mut Window {
        let s = self
            .sessions
            .get_mut(&self.active_session)
            .expect("active session exists");
        let wid = s.active_window;
        s.windows.get_mut(&wid).expect("active window exists")
    }

    // --- operations ---

    /// Split the active pane; the new pane becomes active.
    pub fn split(&mut self, dir: Dir) -> (PaneId, MuxOutcome) {
        let new_pane = self.create_pane();
        let window = self.active_window_mut();
        window.zoomed = None;
        let target = window.active_pane;
        split_leaf(&mut window.layout, target, dir, new_pane);
        window.active_pane = new_pane;
        (
            new_pane,
            MuxOutcome {
                opened_panes: vec![new_pane],
                layout_changed: true,
                ..Default::default()
            },
        )
    }

    /// Kill the active pane, collapsing the tree. Cascades window → session;
    /// a fresh "main" session is created if the last one closes.
    pub fn kill_active_pane(&mut self) -> MuxOutcome {
        let mut outcome = MuxOutcome { layout_changed: true, ..Default::default() };
        let sid = self.active_session;
        let target = self.active_pane();
        self.panes.shift_remove(&target);
        outcome.closed_panes.push(target);

        let session = self.sessions.get_mut(&sid).expect("active session");
        let wid = session.active_window;
        let window = session.windows.get_mut(&wid).expect("active window");
        window.zoomed = None;

        match remove_leaf(&window.layout, target) {
            Some(new_layout) => {
                window.layout = new_layout;
                let remaining = leaves(&window.layout);
                window.active_pane = *remaining.last().expect("non-empty layout");
            }
            None => {
                // Window emptied.
                session.windows.shift_remove(&wid);
                match session.windows.keys().last().copied() {
                    Some(next_wid) => session.active_window = next_wid,
                    None => {
                        // Session emptied.
                        let closed_panes: Vec<PaneId> = Vec::new();
                        let _ = closed_panes;
                        self.sessions.shift_remove(&sid);
                        outcome.closed_sessions.push(sid);
                        match self.sessions.keys().last().copied() {
                            Some(next_sid) => self.active_session = next_sid,
                            None => {
                                let fresh = self.create_session("main".to_string());
                                self.active_session = fresh;
                                if let Some(s) = self.sessions.get(&fresh) {
                                    let w = &s.windows[&s.active_window];
                                    outcome.opened_panes.push(w.active_pane);
                                }
                            }
                        }
                    }
                }
            }
        }
        outcome
    }

    pub fn new_window(&mut self) -> (PaneId, MuxOutcome) {
        let window = self.create_window(format!("win{}", self.next_id));
        let pane = window.active_pane;
        let wid = window.id;
        let session = self
            .sessions
            .get_mut(&self.active_session)
            .expect("active session");
        session.windows.insert(wid, window);
        session.active_window = wid;
        (
            pane,
            MuxOutcome { opened_panes: vec![pane], layout_changed: true, ..Default::default() },
        )
    }

    pub fn cycle_window(&mut self, forward: bool) -> MuxOutcome {
        let session = self
            .sessions
            .get_mut(&self.active_session)
            .expect("active session");
        let ids: Vec<WindowId> = session.windows.keys().copied().collect();
        if ids.len() > 1 {
            let pos = ids
                .iter()
                .position(|w| *w == session.active_window)
                .unwrap_or(0);
            let next = if forward {
                (pos + 1) % ids.len()
            } else {
                (pos + ids.len() - 1) % ids.len()
            };
            session.active_window = ids[next];
        }
        MuxOutcome { layout_changed: true, ..Default::default() }
    }

    pub fn toggle_zoom(&mut self) -> MuxOutcome {
        let window = self.active_window_mut();
        window.zoomed = match window.zoomed {
            Some(_) => None,
            None => Some(window.active_pane),
        };
        MuxOutcome { layout_changed: true, ..Default::default() }
    }

    pub fn focus(&mut self, dir: FocusDir) -> MuxOutcome {
        let window = self.active_window_mut();
        let order = leaves(&window.layout);
        if order.len() <= 1 {
            return MuxOutcome::default();
        }
        let current = window.active_pane;
        let next = match dir {
            FocusDir::Next => {
                let pos = order.iter().position(|p| *p == current).unwrap_or(0);
                order[(pos + 1) % order.len()]
            }
            directional => {
                let rects = layout(&window.layout, Rect::FULL);
                let Some((_, from)) = rects.iter().find(|(p, _)| *p == current) else {
                    return MuxOutcome::default();
                };
                let (fx, fy) = from.center();
                let candidate = rects
                    .iter()
                    .filter(|(p, _)| *p != current)
                    .filter(|(_, r)| {
                        let (cx, cy) = r.center();
                        match directional {
                            FocusDir::Left => cx < fx - 0.01,
                            FocusDir::Right => cx > fx + 0.01,
                            FocusDir::Up => cy < fy - 0.01,
                            FocusDir::Down => cy > fy + 0.01,
                            FocusDir::Next => false,
                        }
                    })
                    .min_by(|(_, a), (_, b)| {
                        let da = dist2(from.center(), a.center());
                        let db = dist2(from.center(), b.center());
                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                    });
                match candidate {
                    Some((p, _)) => *p,
                    None => return MuxOutcome::default(),
                }
            }
        };
        window.active_pane = next;
        window.zoomed = window.zoomed.map(|_| next);
        MuxOutcome { layout_changed: true, ..Default::default() }
    }

    /// Focus a specific pane (host click). No-op if it isn't in the active
    /// window of the active session — clicking a pane in another session's
    /// panel switches sessions first (M6).
    pub fn focus_pane(&mut self, pane: PaneId) -> MuxOutcome {
        if let Some(sid) = self.session_of_pane(pane) {
            self.active_session = sid;
            let session = self.sessions.get_mut(&sid).expect("session exists");
            for (wid, window) in &mut session.windows {
                if leaves(&window.layout).contains(&pane) {
                    session.active_window = *wid;
                    window.active_pane = pane;
                    return MuxOutcome { layout_changed: true, ..Default::default() };
                }
            }
        }
        MuxOutcome::default()
    }

    pub fn new_session(&mut self, name: Option<String>) -> (PaneId, MuxOutcome) {
        let name = name.unwrap_or_else(|| format!("session{}", self.sessions.len()));
        let sid = self.create_session(name);
        self.active_session = sid;
        let s = &self.sessions[&sid];
        let pane = s.windows[&s.active_window].active_pane;
        (
            pane,
            MuxOutcome { opened_panes: vec![pane], layout_changed: true, ..Default::default() },
        )
    }

    pub fn cycle_session(&mut self, forward: bool) -> MuxOutcome {
        let ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        if ids.len() > 1 {
            let pos = ids
                .iter()
                .position(|s| *s == self.active_session)
                .unwrap_or(0);
            let next = if forward {
                (pos + 1) % ids.len()
            } else {
                (pos + ids.len() - 1) % ids.len()
            };
            self.active_session = ids[next];
        }
        MuxOutcome { layout_changed: true, ..Default::default() }
    }

    pub fn switch_session(&mut self, name: &str) -> Result<MuxOutcome, String> {
        match self.sessions.values().find(|s| s.name == name) {
            Some(s) => {
                self.active_session = s.id;
                Ok(MuxOutcome { layout_changed: true, ..Default::default() })
            }
            None => Err(format!("no session named `{name}`")),
        }
    }

    /// Resize a split child (divider drag): `path` walks Split children from
    /// the window root; `fraction` is the new share of child `path.last()`,
    /// its right/lower sibling absorbing the difference.
    pub fn resize_split(&mut self, path: &[usize], fraction: f32) -> MuxOutcome {
        let window = self.active_window_mut();
        let mut node = &mut window.layout;
        let Some((&last, parents)) = path.split_last() else {
            return MuxOutcome::default();
        };
        for &idx in parents {
            match node {
                LayoutNode::Split { children, .. } if idx < children.len() => {
                    node = &mut children[idx].1;
                }
                _ => return MuxOutcome::default(),
            }
        }
        if let LayoutNode::Split { children, .. } = node {
            if last + 1 < children.len() {
                let pair_total = children[last].0 + children[last + 1].0;
                let clamped = fraction.clamp(0.05, pair_total - 0.05);
                children[last].0 = clamped;
                children[last + 1].0 = pair_total - clamped;
                return MuxOutcome { layout_changed: true, ..Default::default() };
            }
        }
        MuxOutcome::default()
    }
}

fn dist2(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}

/// All pane ids in DFS (visual) order.
pub fn leaves(node: &LayoutNode) -> Vec<PaneId> {
    match node {
        LayoutNode::Leaf(p) => vec![*p],
        LayoutNode::Split { children, .. } => {
            children.iter().flat_map(|(_, c)| leaves(c)).collect()
        }
    }
}

/// Pure layout math: fractions in, fractional rects out.
pub fn layout(node: &LayoutNode, rect: Rect) -> Vec<(PaneId, Rect)> {
    match node {
        LayoutNode::Leaf(p) => vec![(*p, rect)],
        LayoutNode::Split { dir, children } => {
            let total: f32 = children.iter().map(|(f, _)| f).sum();
            let total = if total <= 0.0 { 1.0 } else { total };
            let mut out = Vec::new();
            let mut offset = 0.0f32;
            for (fraction, child) in children {
                let share = fraction / total;
                let sub = match dir {
                    Dir::Row => Rect {
                        x: rect.x + rect.w * offset,
                        y: rect.y,
                        w: rect.w * share,
                        h: rect.h,
                    },
                    Dir::Col => Rect {
                        x: rect.x,
                        y: rect.y + rect.h * offset,
                        w: rect.w,
                        h: rect.h * share,
                    },
                };
                out.extend(layout(child, sub));
                offset += share;
            }
            out
        }
    }
}

/// Layout honoring zoom: a zoomed pane takes the full rect.
pub fn layout_window(window: &Window, rect: Rect) -> Vec<(PaneId, Rect)> {
    match window.zoomed {
        Some(p) => vec![(p, rect)],
        None => layout(&window.layout, rect),
    }
}

fn split_leaf(node: &mut LayoutNode, target: PaneId, dir: Dir, new_pane: PaneId) -> bool {
    match node {
        LayoutNode::Leaf(p) if *p == target => {
            *node = LayoutNode::Split {
                dir,
                children: vec![
                    (0.5, LayoutNode::Leaf(target)),
                    (0.5, LayoutNode::Leaf(new_pane)),
                ],
            };
            true
        }
        LayoutNode::Leaf(_) => false,
        LayoutNode::Split { children, .. } => children
            .iter_mut()
            .any(|(_, c)| split_leaf(c, target, dir, new_pane)),
    }
}

/// Remove a leaf; `None` means the whole (sub)tree vanished. Single-child
/// splits collapse; sibling fractions renormalize.
fn remove_leaf(node: &LayoutNode, target: PaneId) -> Option<LayoutNode> {
    match node {
        LayoutNode::Leaf(p) if *p == target => None,
        LayoutNode::Leaf(p) => Some(LayoutNode::Leaf(*p)),
        LayoutNode::Split { dir, children } => {
            let mut kept: Vec<(f32, LayoutNode)> = Vec::new();
            for (fraction, child) in children {
                match remove_leaf(child, target) {
                    Some(sub) => kept.push((*fraction, sub)),
                    None => {}
                }
            }
            match kept.len() {
                0 => None,
                1 => Some(kept.remove(0).1),
                _ => {
                    let total: f32 = kept.iter().map(|(f, _)| f).sum();
                    for (f, _) in &mut kept {
                        *f /= total;
                    }
                    Some(LayoutNode::Split { dir: *dir, children: kept })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_covers(rects: &[(PaneId, Rect)]) {
        let area: f32 = rects.iter().map(|(_, r)| r.w * r.h).sum();
        assert!((area - 1.0).abs() < 1e-4, "areas must sum to 1, got {area}");
        for (p, r) in rects {
            assert!(r.w > 0.0 && r.h > 0.0, "pane {p} has zero size: {r:?}");
        }
    }

    #[test]
    fn new_mux_has_one_pane() {
        let mux = Mux::new();
        assert_eq!(mux.sessions.len(), 1);
        assert_eq!(mux.panes.len(), 1);
        let rects = layout_window(mux.active_window(), Rect::FULL);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].1, Rect::FULL);
    }

    #[test]
    fn split_right_then_down() {
        let mut mux = Mux::new();
        let first = mux.active_pane();
        let (second, out) = mux.split(Dir::Row);
        assert_eq!(out.opened_panes, vec![second]);
        assert_eq!(mux.active_pane(), second);

        let (third, _) = mux.split(Dir::Col);
        let rects = layout_window(mux.active_window(), Rect::FULL);
        assert_eq!(rects.len(), 3);
        assert_covers(&rects);

        let find = |p: PaneId| rects.iter().find(|(q, _)| *q == p).map(|(_, r)| *r).expect("pane rect");
        assert_eq!(find(first), Rect { x: 0.0, y: 0.0, w: 0.5, h: 1.0 });
        assert_eq!(find(second), Rect { x: 0.5, y: 0.0, w: 0.5, h: 0.5 });
        assert_eq!(find(third), Rect { x: 0.5, y: 0.5, w: 0.5, h: 0.5 });
    }

    #[test]
    fn kill_pane_collapses_and_refocuses() {
        let mut mux = Mux::new();
        let first = mux.active_pane();
        let (second, _) = mux.split(Dir::Row);
        let out = mux.kill_active_pane();
        assert_eq!(out.closed_panes, vec![second]);
        assert_eq!(mux.active_pane(), first);
        // Tree collapsed back to a single full-size leaf.
        let rects = layout_window(mux.active_window(), Rect::FULL);
        assert_eq!(rects, vec![(first, Rect::FULL)]);
    }

    #[test]
    fn killing_last_pane_recreates_a_session() {
        let mut mux = Mux::new();
        let out = mux.kill_active_pane();
        assert_eq!(out.closed_sessions.len(), 1);
        assert_eq!(out.opened_panes.len(), 1, "fresh session pane");
        assert_eq!(mux.sessions.len(), 1);
        assert_eq!(mux.panes.len(), 1);
    }

    #[test]
    fn fractions_renormalize_after_kill() {
        let mut mux = Mux::new();
        mux.split(Dir::Row); // two panes 0.5/0.5
        mux.split(Dir::Row); // active split again: 0.5 / (0.25/0.25) nested
        mux.kill_active_pane();
        let rects = layout_window(mux.active_window(), Rect::FULL);
        assert_eq!(rects.len(), 2);
        assert_covers(&rects);
    }

    #[test]
    fn focus_next_cycles_and_directional_moves() {
        let mut mux = Mux::new();
        let first = mux.active_pane();
        let (second, _) = mux.split(Dir::Row);
        mux.focus(FocusDir::Next);
        assert_eq!(mux.active_pane(), first);
        mux.focus(FocusDir::Right);
        assert_eq!(mux.active_pane(), second);
        mux.focus(FocusDir::Left);
        assert_eq!(mux.active_pane(), first);
        // No pane above: no-op.
        mux.focus(FocusDir::Up);
        assert_eq!(mux.active_pane(), first);
    }

    #[test]
    fn zoom_takes_full_rect_and_toggles() {
        let mut mux = Mux::new();
        let (second, _) = mux.split(Dir::Row);
        mux.toggle_zoom();
        let rects = layout_window(mux.active_window(), Rect::FULL);
        assert_eq!(rects, vec![(second, Rect::FULL)]);
        mux.toggle_zoom();
        assert_eq!(layout_window(mux.active_window(), Rect::FULL).len(), 2);
    }

    #[test]
    fn windows_cycle() {
        let mut mux = Mux::new();
        let pane_a = mux.active_pane();
        let (pane_b, _) = mux.new_window();
        assert_eq!(mux.active_pane(), pane_b);
        mux.cycle_window(true);
        assert_eq!(mux.active_pane(), pane_a);
        mux.cycle_window(false);
        assert_eq!(mux.active_pane(), pane_b);
    }

    #[test]
    fn sessions_fork_switch_and_cycle() {
        let mut mux = Mux::new();
        let pane_main = mux.active_pane();
        let (pane_work, _) = mux.new_session(Some("work".to_string()));
        assert_ne!(pane_main, pane_work);
        assert_eq!(mux.active_pane(), pane_work);

        mux.switch_session("main").expect("switch by name");
        assert_eq!(mux.active_pane(), pane_main);
        assert!(mux.switch_session("nope").is_err());

        mux.cycle_session(true);
        assert_eq!(mux.active_pane(), pane_work);
    }

    #[test]
    fn focus_pane_switches_session_and_window() {
        let mut mux = Mux::new();
        let pane_main = mux.active_pane();
        mux.new_session(Some("work".to_string()));
        let out = mux.focus_pane(pane_main);
        assert!(out.layout_changed);
        assert_eq!(mux.active_session().name, "main");
        assert_eq!(mux.active_pane(), pane_main);
    }

    #[test]
    fn resize_split_moves_divider_with_clamp() {
        let mut mux = Mux::new();
        mux.split(Dir::Row);
        mux.resize_split(&[0], 0.7);
        let rects = layout_window(mux.active_window(), Rect::FULL);
        let widths: Vec<f32> = rects.iter().map(|(_, r)| r.w).collect();
        assert!((widths[0] - 0.7).abs() < 1e-4, "{widths:?}");
        assert_covers(&rects);

        // Clamped: can't crush a pane below 5%.
        mux.resize_split(&[0], 0.999);
        let rects = layout_window(mux.active_window(), Rect::FULL);
        assert!(rects.iter().all(|(_, r)| r.w >= 0.049), "{rects:?}");
    }

    #[test]
    fn deep_nested_layout_stays_consistent() {
        let mut mux = Mux::new();
        for i in 0..6 {
            mux.split(if i % 2 == 0 { Dir::Row } else { Dir::Col });
        }
        let rects = layout_window(mux.active_window(), Rect::FULL);
        assert_eq!(rects.len(), 7);
        assert_covers(&rects);
        // Kill everything; mux must survive with a fresh pane.
        for _ in 0..7 {
            mux.kill_active_pane();
        }
        assert_eq!(mux.panes.len(), 1);
    }
}
