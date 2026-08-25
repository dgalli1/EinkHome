//! evdev input — the Linux touch/button path every direct-fb device uses
//! (Kobo, reMarkable, Cervantes, generic boards).  Ported from KOReader's
//! `frontend/device/input.lua` MT-protocol-B handling, narrowed to
//! EinkHome's single-pointer HAL.
//!
//! Node roles (KOReader `device/kobo/device.lua`): the NTX button pad sits
//! on a stable node (`/dev/input/event0`), the touch panel on the next one;
//! reMarkable separates pen (`event0`), buttons (`event1`) and touch
//! (`event2`).  Rather than hardcoding indices we classify nodes by their
//! capability bits: a node carrying `ABS_MT_POSITION_X` is a touch panel,
//! one carrying `BTN_TOOL_PEN` a digitizer, everything else with `EV_KEY`
//! is a button pad.
//!
//! Axis ranges come from `EVIOCGABS` at runtime — the same values KOReader
//! reads through `input.open` — so coordinate scaling follows the panel
//! instead of a per-model table.  Device quirks (axis swap, mirroring,
//! first finger slot) stay per-device data.

use eh_hal::{InputEvent, KeyCode};
use std::collections::VecDeque;
use std::os::unix::io::RawFd;

// ── linux/input.h constants (value-stable since 2.6) ────────────────────

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;

pub const SYN_REPORT: u16 = 0;

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;

pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_TOOL_PEN: u16 = 0x140;

pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_HOME: u16 = 102;
pub const KEY_BACK: u16 = 158;
pub const KEY_POWER: u16 = 116;
pub const KEY_WAKEUP: u16 = 143;

/// `_IOR('E', nr, 24)` — `EVIOCGABS` (one `input_absinfo` back).
pub const fn eviocgabs(abs: u16) -> libc::c_ulong {
    (2 << 30) | (24 << 16) | (0x45 << 8) | (0x40 + abs as libc::c_ulong)
}

/// `EVIOCGBIT(type, 64)` — capability bitmap back.
const fn eviocgbit(kind: u16) -> libc::c_ulong {
    (2 << 30) | (64 << 16) | (0x45 << 8) | (0x20 + kind as libc::c_ulong)
}

/// `struct input_absinfo` (the fields we read).
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

/// Per-panel coordinate quirks (KOReader `device/kobo/device.lua` defaults:
/// `touch_switch_xy = true`, `touch_mirrored_x = true`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchQuirks {
    pub switch_xy: bool,
    pub mirrored_x: bool,
    pub mirrored_y: bool,
    /// First active slot when the panel never uses slot 0 (Aura H2O:
    /// KOReader `main_finger_slot = 1`).
    pub main_slot: i32,
}

impl Default for TouchQuirks {
    fn default() -> Self {
        Self {
            switch_xy: true,
            mirrored_x: true,
            mirrored_y: false,
            main_slot: 0,
        }
    }
}

/// One opened evdev node and its role in the decode state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// Button pad (`EV_KEY` only): Kobo NTX pad, reMarkable gpio-keys.
    Buttons,
    /// Touch panel (MT protocol B or single-touch).
    Touch,
    /// Wacom digitizer (reMarkable pen).
    Pen,
}

/// A raw `struct input_event` payload (libc's type nests an arch-dependent
/// `timeval`; this carries just what the decoder reads).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// Button mapping: linux key code → HAL key.  Per-device tables (KOReader
/// `event_map.lua`s).
pub type ButtonMap = &'static [(u16, KeyCode)];

/// Kobo NTX pad (event0): page rocker left/right, home, back, power.
pub const KOBO_BUTTONS: ButtonMap = &[
    (KEY_LEFT, KeyCode::PrevPage),
    (KEY_RIGHT, KeyCode::NextPage),
    (KEY_HOME, KeyCode::Home),
    (KEY_BACK, KeyCode::Back),
    (KEY_POWER, KeyCode::Unknown(KEY_POWER as u32)),
];

/// reMarkable gpio-keys (`frontend/device/remarkable/event_map.lua`:
/// 102 Home, 105 LPgBack, 106 RPgFwd, 116 Power, 143 Resume).
pub const REMARKABLE_BUTTONS: ButtonMap = &[
    (KEY_HOME, KeyCode::Home),
    (KEY_LEFT, KeyCode::PrevPage),
    (KEY_RIGHT, KeyCode::NextPage),
    (KEY_POWER, KeyCode::Unknown(KEY_POWER as u32)),
    (KEY_WAKEUP, KeyCode::Unknown(KEY_WAKEUP as u32)),
];

/// Cervantes front panel.
pub const CERVANTES_BUTTONS: ButtonMap = &[
    (KEY_HOME, KeyCode::Home),
    (KEY_BACK, KeyCode::Back),
    (KEY_POWER, KeyCode::Unknown(KEY_POWER as u32)),
];

/// Pure MT-B / single-touch / pen decoder: feed raw events per node kind,
/// collect HAL events at `SYN_REPORT`.  Separated from I/O so tests can
/// drive it with synthetic event streams (KOReader's `input.lua` state
/// machine, narrowed to one pointer).
#[derive(Debug)]
pub struct Decoder {
    quirks: TouchQuirks,
    screen: (u32, u32),
    /// Touch-panel axis ranges (x_min, x_max, y_min, y_max).
    touch_range: (i32, i32, i32, i32),
    /// Digitizer axis ranges; x_min == x_max when no pen node exists.
    pen_range: (i32, i32, i32, i32),
    button_map: ButtonMap,

    slot: i32,
    /// The MT slot we currently report (single-pointer HAL).
    tracked: Option<i32>,
    /// Per-slot last position + activity, indexed from `main_slot`.
    slots: [(i32, i32, bool); 8],
    /// Whether the touch node speaks MT-B (then `BTN_TOUCH` is contact
    /// confirmation, not a parallel single-touch stream).
    mt_touch: bool,
    st_pos: (i32, i32),
    st_down: bool,
    pen_pos: (i32, i32),
    pen_down: bool,
    /// Emission bookkeeping for the tracked contact: has Down gone out,
    /// and what was the last position sent.
    emitted_down: bool,
    last_sent: Option<(i32, i32)>,
    pending: Vec<InputEvent>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(
            TouchQuirks::default(),
            (600, 800),
            (0, 1, 0, 1),
            (0, 0, 0, 0),
            &[],
        )
    }
}

impl Decoder {
    pub fn new(
        quirks: TouchQuirks,
        screen: (u32, u32),
        touch_range: (i32, i32, i32, i32),
        pen_range: (i32, i32, i32, i32),
        button_map: ButtonMap,
    ) -> Self {
        Self {
            quirks,
            screen,
            touch_range,
            pen_range,
            button_map,
            slot: 0,
            tracked: None,
            slots: [(0, 0, false); 8],
            mt_touch: false,
            st_pos: (0, 0),
            st_down: false,
            pen_pos: (0, 0),
            pen_down: false,
            emitted_down: false,
            last_sent: None,
            pending: Vec::new(),
        }
    }

    /// Mark the touch node as MT protocol B (KOReader's `hasMultitouch`):
    /// `BTN_TOUCH` then confirms contact instead of driving a parallel
    /// single-touch stream.
    pub fn set_mt_touch(&mut self, mt: bool) {
        self.mt_touch = mt;
    }

    fn scale(raw: i32, lo: i32, hi: i32, span: u32) -> i32 {
        let range = (hi - lo).max(1);
        let v = (raw - lo).clamp(0, range) as i64;
        ((v * i64::from(span)) / i64::from(range)).min(i64::from(span) - 1) as i32
    }

    /// Touch-panel coordinates → surface pixels (quirks applied).
    fn touch_point(&self, x: i32, y: i32) -> (i32, i32) {
        let (xlo, xhi, ylo, yhi) = self.touch_range;
        let (w, h) = self.screen;
        let (px, py) = if self.quirks.switch_xy {
            (Self::scale(y, ylo, yhi, w), Self::scale(x, xlo, xhi, h))
        } else {
            (Self::scale(x, xlo, xhi, w), Self::scale(y, ylo, yhi, h))
        };
        let px = if self.quirks.mirrored_x {
            w as i32 - 1 - px
        } else {
            px
        };
        let py = if self.quirks.mirrored_y {
            h as i32 - 1 - py
        } else {
            py
        };
        (px, py)
    }

    /// Digitizer coordinates → surface pixels (never axis-swapped;
    /// KOReader scales wacom X/Y straight onto screen_width/height).
    fn pen_point(&self, x: i32, y: i32) -> (i32, i32) {
        let (xlo, xhi, ylo, yhi) = self.pen_range;
        let (w, h) = self.screen;
        (Self::scale(x, xlo, xhi, w), Self::scale(y, ylo, yhi, h))
    }

    /// Feed one raw event from a node of `kind`; returns the HAL events
    /// flushed by this event (non-empty only at `SYN_REPORT` boundaries).
    pub fn feed(&mut self, kind: NodeKind, ev: &RawEvent) -> Vec<InputEvent> {
        match ev.kind {
            // Buttons report immediately (KOReader emits keys as they
            // arrive); only pointer coordinates batch on SYN.
            EV_KEY => self.feed_key(kind, ev),
            EV_ABS => {
                self.feed_abs(kind, ev);
                Vec::new()
            }
            EV_SYN if ev.code == SYN_REPORT => {
                self.flush_syn();
                core::mem::take(&mut self.pending)
            }
            _ => Vec::new(),
        }
    }

    /// Emit the tracked contact's transition for this frame: Down on the
    /// first complete position, Move on change, Up on release (then
    /// retarget to the next active slot).
    fn flush_syn(&mut self) {
        if let Some(i) = self.tracked_idx() {
            let (x, y, active) = self.slots[i];
            if active {
                let (px, py) = self.touch_point(x, y);
                if !self.emitted_down {
                    self.emitted_down = true;
                    self.last_sent = Some((px, py));
                    self.pending.push(InputEvent::PointerDown { x: px, y: py });
                } else if self.last_sent != Some((px, py)) {
                    self.last_sent = Some((px, py));
                    self.pending.push(InputEvent::PointerMove { x: px, y: py });
                }
                return;
            }
            // Released: lift at the last position, then retarget.
            let (px, py) = self.touch_point(x, y);
            self.pending.push(InputEvent::PointerUp { x: px, y: py });
            self.tracked = (0..self.slots.len())
                .find(|&j| self.slots[j].2)
                .map(|j| j as i32 + self.quirks.main_slot);
            self.emitted_down = false;
            self.last_sent = None;
        }
    }

    fn tracked_idx(&self) -> Option<usize> {
        self.tracked
            .map(|t| (t - self.quirks.main_slot).clamp(0, self.slots.len() as i32 - 1) as usize)
    }

    fn feed_key(&mut self, kind: NodeKind, ev: &RawEvent) -> Vec<InputEvent> {
        match (kind, ev.code) {
            // Buttons report immediately (KOReader emits keys as they
            // arrive); only pointer coordinates batch on SYN.
            (NodeKind::Buttons, _) => {
                if let Some((_, key)) = self.button_map.iter().find(|(c, _)| *c == ev.code) {
                    let key = *key;
                    return match ev.value {
                        1 => vec![InputEvent::KeyDown { key }],
                        0 => vec![InputEvent::KeyUp { key }],
                        _ => Vec::new(),
                    };
                }
                Vec::new()
            }
            (NodeKind::Pen, BTN_TOOL_PEN) => {
                if ev.value == 0 && self.pen_down {
                    self.pen_down = false;
                    let (x, y) = self.pen_point(self.pen_pos.0, self.pen_pos.1);
                    return vec![InputEvent::PointerUp { x, y }];
                }
                Vec::new()
            }
            (NodeKind::Pen, BTN_TOUCH) => {
                // On a digitizer BTN_TOUCH marks ink contact (1) vs hover (0).
                match (ev.value == 1, self.pen_down) {
                    (true, false) => {
                        self.pen_down = true;
                        let (x, y) = self.pen_point(self.pen_pos.0, self.pen_pos.1);
                        vec![InputEvent::PointerDown { x, y }]
                    }
                    (false, true) => {
                        self.pen_down = false;
                        let (x, y) = self.pen_point(self.pen_pos.0, self.pen_pos.1);
                        vec![InputEvent::PointerUp { x, y }]
                    }
                    _ => Vec::new(),
                }
            }
            (NodeKind::Touch, BTN_TOUCH) if !self.mt_touch => {
                // Single-touch panel: contact state around st_pos.  On an
                // MT-B panel BTN_TOUCH is contact confirmation — the slot
                // tracking owns down/up — so it is ignored here.
                match (ev.value == 1, self.st_down) {
                    (true, false) => {
                        self.st_down = true;
                        let (px, py) = self.touch_point(self.st_pos.0, self.st_pos.1);
                        vec![InputEvent::PointerDown { x: px, y: py }]
                    }
                    (false, true) => {
                        self.st_down = false;
                        let (px, py) = self.touch_point(self.st_pos.0, self.st_pos.1);
                        vec![InputEvent::PointerUp { x: px, y: py }]
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn feed_abs(&mut self, kind: NodeKind, ev: &RawEvent) {
        match (kind, ev.code) {
            (NodeKind::Touch, ABS_MT_SLOT) => {
                self.slot = ev.value;
            }
            (NodeKind::Touch, ABS_MT_TRACKING_ID) => {
                let active = ev.value >= 0;
                let idx = self.slot_index();
                self.slots[idx].2 = active;
                if active {
                    // First contact wins (single-pointer HAL).
                    if self.tracked.is_none() {
                        self.tracked = Some(self.slot);
                        self.emitted_down = false;
                        self.last_sent = None;
                    }
                }
                // The lift is emitted at SYN (flush_syn) with the slot's
                // final position.
            }
            (NodeKind::Touch, ABS_MT_POSITION_X) => self.mt_pos(Some(ev.value), None),
            (NodeKind::Touch, ABS_MT_POSITION_Y) => self.mt_pos(None, Some(ev.value)),
            (NodeKind::Touch, ABS_X) => {
                self.st_pos.0 = ev.value;
                if self.st_down {
                    let (px, py) = self.touch_point(ev.value, self.st_pos.1);
                    self.pending.push(InputEvent::PointerMove { x: px, y: py });
                }
            }
            (NodeKind::Touch, ABS_Y) => {
                self.st_pos.1 = ev.value;
                if self.st_down {
                    let (px, py) = self.touch_point(self.st_pos.0, ev.value);
                    self.pending.push(InputEvent::PointerMove { x: px, y: py });
                }
            }
            (NodeKind::Pen, ABS_X) => {
                self.pen_pos.0 = ev.value;
                if self.pen_down {
                    let (px, py) = self.pen_point(ev.value, self.pen_pos.1);
                    self.pending.push(InputEvent::PointerMove { x: px, y: py });
                }
            }
            (NodeKind::Pen, ABS_Y) => {
                self.pen_pos.1 = ev.value;
                if self.pen_down {
                    let (px, py) = self.pen_point(self.pen_pos.0, ev.value);
                    self.pending.push(InputEvent::PointerMove { x: px, y: py });
                }
            }
            _ => {}
        }
    }

    /// One half of an MT position update.  Positions are recorded here;
    /// the HAL events are emitted at `SYN_REPORT` with the complete pair
    /// (KOReader's event loop also acts only on `SYN_REPORT`).
    fn mt_pos(&mut self, x: Option<i32>, y: Option<i32>) {
        let idx = self.slot_index();
        let (old_x, old_y, active) = self.slots[idx];
        let (nx, ny) = (x.unwrap_or(old_x), y.unwrap_or(old_y));
        self.slots[idx] = (nx, ny, active || old_x != 0 || old_y != 0);
    }

    /// Slots are reported relative to `main_slot` (Aura H2O starts at 1).
    fn slot_index(&self) -> usize {
        (self.slot - self.quirks.main_slot).clamp(0, self.slots.len() as i32 - 1) as usize
    }
}

/// An opened evdev node.
struct Node {
    fd: RawFd,
    kind: NodeKind,
}

/// The live input side: opened nodes + decoder + a queue so a single read
/// burst that decodes into several HAL events is delivered across
/// consecutive `poll_event` calls instead of dropped.
pub struct EvDev {
    nodes: Vec<Node>,
    decoder: Decoder,
    queue: VecDeque<InputEvent>,
    buf: [u8; 256],
}

impl EvDev {
    /// Open `paths`, classifying each node by capability probe:
    /// `ABS_MT_POSITION_X` → touch, `BTN_TOOL_PEN` → pen, else buttons.
    /// Nodes that cannot be opened are skipped (a Kobo without a button
    /// pad attached still gets touch).  Fails only when nothing opens.
    pub fn open(
        paths: &[&str],
        quirks: TouchQuirks,
        screen: (u32, u32),
        button_map: ButtonMap,
    ) -> std::io::Result<Self> {
        let mut nodes = Vec::new();
        let mut touch_range = (0, 1, 0, 1);
        let mut pen_range = (0, 0, 0, 0);
        for &path in paths {
            let c = match std::ffi::CString::new(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let fd = unsafe {
                libc::open(
                    c.as_ptr(),
                    libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                continue;
            }
            let kind = classify(fd);
            match kind {
                NodeKind::Touch => {
                    if let Some(r) = axis_range(fd, ABS_MT_POSITION_X, ABS_MT_POSITION_Y) {
                        touch_range = r;
                    } else if let Some(r) = axis_range(fd, ABS_X, ABS_Y) {
                        touch_range = r;
                    }
                }
                NodeKind::Pen => {
                    if let Some(r) = axis_range(fd, ABS_X, ABS_Y) {
                        pen_range = r;
                    }
                }
                NodeKind::Buttons => {}
            }
            nodes.push(Node { fd, kind });
        }
        if nodes.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no evdev nodes could be opened",
            ));
        }
        let mut decoder = Decoder::new(quirks, screen, touch_range, pen_range, button_map);
        decoder.set_mt_touch(touch_range != (0, 1, 0, 1));
        Ok(Self {
            nodes,
            decoder,
            queue: VecDeque::new(),
            buf: [0; 256],
        })
    }

    /// Block up to `timeout_ms` for input on any node (poll(2)).
    pub fn wait(&self, timeout_ms: i32) {
        let mut polls: Vec<libc::pollfd> = self
            .nodes
            .iter()
            .map(|n| libc::pollfd {
                fd: n.fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let rc = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, timeout_ms) };
        let _ = rc;
    }

    /// Drain one decoded event, or `None` when every node is dry.
    pub fn poll_event(&mut self) -> Option<InputEvent> {
        if let Some(ev) = self.queue.pop_front() {
            return Some(ev);
        }
        let ev_size = core::mem::size_of::<libc::input_event>();
        for node in &self.nodes {
            loop {
                let n = unsafe {
                    libc::read(
                        node.fd,
                        self.buf.as_mut_ptr() as *mut libc::c_void,
                        self.buf.len(),
                    )
                };
                if n <= 0 {
                    break;
                }
                let count = n as usize / ev_size;
                for i in 0..count {
                    let bytes = &self.buf[i * ev_size..(i + 1) * ev_size];
                    // struct input_event { struct timeval, u16 type, u16 code, i32 value }
                    let tv = core::mem::size_of::<libc::timeval>();
                    let word = |off: usize| -> u32 {
                        let mut b = [0u8; 4];
                        b.copy_from_slice(&bytes[off..off + 4]);
                        u32::from_ne_bytes(b)
                    };
                    let raw = RawEvent {
                        kind: word(tv) as u16,
                        code: word(tv + 2) as u16,
                        value: word(tv + 4) as i32,
                    };
                    for ev in self.decoder.feed(node.kind, &raw) {
                        self.queue.push_back(ev);
                    }
                }
                if let Some(ev) = self.queue.pop_front() {
                    return Some(ev);
                }
            }
        }
        None
    }
}

impl Drop for EvDev {
    fn drop(&mut self) {
        for node in &self.nodes {
            unsafe {
                libc::close(node.fd);
            }
        }
    }
}

/// Classify an opened node by probing its capability bits (KOReader
/// classifies by node name; caps are stable where names are not).
fn classify(fd: RawFd) -> NodeKind {
    let mut abs_bits = [0u8; 64];
    let n = unsafe { libc::ioctl(fd, eviocgbit(EV_ABS), abs_bits.as_mut_ptr()) };
    if n > 0 && bit(&abs_bits, ABS_MT_POSITION_X) {
        return NodeKind::Touch;
    }
    let mut key_bits = [0u8; 64];
    let n = unsafe { libc::ioctl(fd, eviocgbit(EV_KEY), key_bits.as_mut_ptr()) };
    if n > 0 && bit(&key_bits, BTN_TOOL_PEN) {
        return NodeKind::Pen;
    }
    NodeKind::Buttons
}

fn bit(bits: &[u8; 64], code: u16) -> bool {
    bits.get((code / 8) as usize)
        .is_some_and(|b| b & (1 << (code % 8)) != 0)
}

/// `(x_min, x_max, y_min, y_max)` from `EVIOCGABS`, or `None`.
fn axis_range(fd: RawFd, x: u16, y: u16) -> Option<(i32, i32, i32, i32)> {
    let mut ax = AbsInfo::default();
    let rc = unsafe { libc::ioctl(fd, eviocgabs(x), &mut ax as *mut AbsInfo) };
    if rc < 0 || ax.maximum <= ax.minimum {
        return None;
    }
    let (xmin, xmax) = (ax.minimum, ax.maximum);
    let mut ay = AbsInfo::default();
    let rc = unsafe { libc::ioctl(fd, eviocgabs(y), &mut ay as *mut AbsInfo) };
    if rc < 0 || ay.maximum <= ay.minimum {
        return None;
    }
    Some((xmin, xmax, ay.minimum, ay.maximum))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Clara-HD-class panel: MT-B, axis-swapped, x-mirrored (KOReader
    /// Kobo defaults), 1072×1448 screen, 0..=767/0..=1023 raw ranges.
    fn kobo_decoder() -> Decoder {
        let mut d = Decoder::new(
            TouchQuirks::default(),
            (1072, 1448),
            (0, 767, 0, 1023),
            (0, 0, 0, 0),
            KOBO_BUTTONS,
        );
        // EvDev::open sets this from the node's capability probe.
        d.set_mt_touch(true);
        d
    }

    fn key(code: u16, value: i32) -> RawEvent {
        RawEvent {
            kind: EV_KEY,
            code,
            value,
        }
    }
    fn abs(code: u16, value: i32) -> RawEvent {
        RawEvent {
            kind: EV_ABS,
            code,
            value,
        }
    }
    fn syn() -> RawEvent {
        RawEvent {
            kind: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        }
    }

    /// Collect everything a full event burst decodes into.
    fn burst(d: &mut Decoder, kind: NodeKind, events: &[RawEvent]) -> Vec<InputEvent> {
        let mut out = Vec::new();
        for ev in events {
            out.extend(d.feed(kind, ev));
        }
        out
    }

    #[test]
    fn mtb_tap_decodes_down_up() {
        let mut d = kobo_decoder();
        // Panel stream for a tap at raw (400, 500): slot 0, tracking id 1,
        // position, contact, then release.
        let events = burst(
            &mut d,
            NodeKind::Touch,
            &[
                abs(ABS_MT_SLOT, 0),
                abs(ABS_MT_TRACKING_ID, 1),
                abs(ABS_MT_POSITION_X, 400),
                abs(ABS_MT_POSITION_Y, 500),
                key(BTN_TOUCH, 1),
                syn(),
                abs(ABS_MT_TRACKING_ID, -1),
                key(BTN_TOUCH, 0),
                syn(),
            ],
        );
        // Down at the first position flush, Up at the release flush.
        let downs: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, InputEvent::PointerDown { .. }))
            .collect();
        let ups: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, InputEvent::PointerUp { .. }))
            .collect();
        assert_eq!(downs.len(), 1, "one down: {events:?}");
        assert_eq!(ups.len(), 1, "one up: {events:?}");
        // Axis-swapped + x-mirrored: screen x from raw y, mirrored.
        match *downs[0] {
            InputEvent::PointerDown { x, y } => {
                // scale maps [lo, hi] → [0, span): raw y 500/1023 across the
                // 1072-wide screen, mirrored; raw x 400/767 down the
                // 1448-tall screen.
                assert_eq!(x, 1072 - 1 - (500 * 1072) / 1023);
                assert_eq!(y, (400 * 1448) / 767);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn mtb_drag_decodes_move() {
        let mut d = kobo_decoder();
        let mut events = burst(
            &mut d,
            NodeKind::Touch,
            &[
                abs(ABS_MT_SLOT, 0),
                abs(ABS_MT_TRACKING_ID, 7),
                abs(ABS_MT_POSITION_X, 100),
                abs(ABS_MT_POSITION_Y, 100),
                syn(),
                abs(ABS_MT_POSITION_X, 200),
                syn(),
                abs(ABS_MT_POSITION_X, 300),
                syn(),
                abs(ABS_MT_TRACKING_ID, -1),
                syn(),
            ],
        );
        let moves: Vec<_> = events
            .drain(..)
            .filter(|e| matches!(e, InputEvent::PointerMove { .. }))
            .collect();
        assert_eq!(moves.len(), 2, "two moves: {events:?}");
        // Monotonic along the swapped axis.
        assert!(moves[0] != moves[1]);
    }

    #[test]
    fn second_finger_is_ignored_until_first_lifts() {
        let mut d = kobo_decoder();
        let events = burst(
            &mut d,
            NodeKind::Touch,
            &[
                abs(ABS_MT_SLOT, 0),
                abs(ABS_MT_TRACKING_ID, 1),
                abs(ABS_MT_POSITION_X, 100),
                abs(ABS_MT_POSITION_Y, 100),
                syn(),
                abs(ABS_MT_SLOT, 1),
                abs(ABS_MT_TRACKING_ID, 2),
                abs(ABS_MT_POSITION_X, 500),
                abs(ABS_MT_POSITION_Y, 500),
                syn(),
                abs(ABS_MT_SLOT, 1),
                abs(ABS_MT_TRACKING_ID, -1),
                syn(),
                abs(ABS_MT_SLOT, 0),
                abs(ABS_MT_TRACKING_ID, -1),
                syn(),
            ],
        );
        let ups = events
            .iter()
            .filter(|e| matches!(e, InputEvent::PointerUp { .. }))
            .count();
        // The second finger's lift is not reported while finger 0 is down.
        assert_eq!(ups, 1, "only the tracked finger lifts: {events:?}");
    }

    #[test]
    fn aura_h2o_slots_start_at_one() {
        // KOReader main_finger_slot = 1: the first finger uses slot 1.
        let mut d = Decoder::new(
            TouchQuirks {
                main_slot: 1,
                ..TouchQuirks::default()
            },
            (1080, 1429),
            (0, 767, 0, 1023),
            (0, 0, 0, 0),
            KOBO_BUTTONS,
        );
        let events = burst(
            &mut d,
            NodeKind::Touch,
            &[
                abs(ABS_MT_SLOT, 1),
                abs(ABS_MT_TRACKING_ID, 1),
                abs(ABS_MT_POSITION_X, 400),
                abs(ABS_MT_POSITION_Y, 500),
                syn(),
                abs(ABS_MT_SLOT, 1),
                abs(ABS_MT_TRACKING_ID, -1),
                syn(),
            ],
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, InputEvent::PointerDown { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, InputEvent::PointerUp { .. })));
    }

    #[test]
    fn single_touch_panel_decodes() {
        // Cervantes-class legacy panel: no MT slots, ABS_X/Y + BTN_TOUCH.
        let mut d = Decoder::new(
            TouchQuirks::default(),
            (758, 1024),
            (0, 4095, 0, 4095),
            (0, 0, 0, 0),
            CERVANTES_BUTTONS,
        );
        let events = burst(
            &mut d,
            NodeKind::Touch,
            &[
                abs(ABS_X, 1000),
                abs(ABS_Y, 2000),
                key(BTN_TOUCH, 1),
                syn(),
                key(BTN_TOUCH, 0),
                syn(),
            ],
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, InputEvent::PointerDown { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, InputEvent::PointerUp { .. })));
    }

    #[test]
    fn buttons_map_through_table() {
        let mut d = kobo_decoder();
        let down = d.feed(NodeKind::Buttons, &key(KEY_HOME, 1));
        let up = d.feed(NodeKind::Buttons, &key(KEY_HOME, 0));
        assert_eq!(down, vec![InputEvent::KeyDown { key: KeyCode::Home }]);
        assert_eq!(up, vec![InputEvent::KeyUp { key: KeyCode::Home }]);
        // Unmapped keys vanish (KOReader event_map semantics).
        assert!(d.feed(NodeKind::Buttons, &key(999, 1)).is_empty());
    }

    #[test]
    fn wacom_pen_scales_onto_screen() {
        // reMarkable 1: digitizer 15725×20967 raw → 1404×1872 screen
        // (KOReader wacom_scale_x/y), no axis swap.
        let mut d = Decoder::new(
            TouchQuirks {
                switch_xy: false,
                mirrored_x: false,
                mirrored_y: false,
                main_slot: 0,
            },
            (1404, 1872),
            (0, 0, 0, 0),
            (0, 15725, 0, 20967),
            REMARKABLE_BUTTONS,
        );
        let events = burst(
            &mut d,
            NodeKind::Pen,
            &[
                abs(ABS_X, 7862), // half of the raw range
                abs(ABS_Y, 10483),
                key(BTN_TOOL_PEN, 1),
                key(BTN_TOUCH, 1),
                syn(),
                key(BTN_TOUCH, 0),
                key(BTN_TOOL_PEN, 0),
                syn(),
            ],
        );
        let down = events
            .iter()
            .find(|e| matches!(e, InputEvent::PointerDown { .. }))
            .expect("pen down");
        match *down {
            InputEvent::PointerDown { x, y } => {
                assert_eq!(x, 1403 / 2, "pen x scales onto the screen");
                assert_eq!(y, 1871 / 2, "pen y scales onto the screen");
            }
            _ => unreachable!(),
        }
        assert!(events
            .iter()
            .any(|e| matches!(e, InputEvent::PointerUp { .. })));
    }

    #[test]
    fn ioctl_numbers_match_koreader() {
        use crate::mxcfb::MxcFlavor;
        // KOReader mxcfb_kobo_h.lua constants.
        assert_eq!(MxcFlavor::V1Ntx.send_update_ioctl(), 1078216238);
        assert_eq!(MxcFlavor::V2.send_update_ioctl(), 1078478382);
        assert_eq!(MxcFlavor::Hwtcon.send_update_ioctl(), 1076119086);
    }
}
