//! ASB view: interactive TUI that subscribes to all configured topics and
//! displays received messages with filtering, sorting, and multiple view modes.
//!
//! Configuration (CALConfig.toml): same layout as `asb_dump`, but the service
//! id is `"asb_view"`.  Falls back to the first service with topics if
//! `"asb_view"` is not present.
//!
//! Usage: asb_view [FILE]
//!   If FILE is given, load a previously saved capture instead of connecting
//!   to the ASB.
//!
//! Key bindings:
//!   j / ↓      next message
//!   k / ↑      previous message
//!   /          open filter input
//!   Esc        clear filter / cancel input
//!   s          cycle sort: Time ↔ Topic
//!   i          toggle time display: ISO ↔ elapsed seconds
//!   F          form view (default)
//!   X          XML view
//!   T          Tree view (cargo-tree style hierarchy)
//!   Tab        focus detail pane (form view)
//!   Space/Enter  collapse/expand node (detail pane, form view)
//!   y          copy value to clipboard (detail pane, form view)
//!   Esc        unfocus detail pane / clear filter
//!   h          lock/cycle highlight color for selected topic (8 colors)
//!   H          clear all locked highlights
//!   w          save filtered messages to a file (prompts for path)
//!   q          quit

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

/// 8 distinct background colors for locked topic highlights.
const LOCK_BG_COLORS: [Color; 8] = [
    Color::Rgb(80, 20, 20),
    Color::Rgb(20, 70, 20),
    Color::Rgb(20, 20, 90),
    Color::Rgb(80, 65, 0),
    Color::Rgb(70, 0, 70),
    Color::Rgb(0, 65, 65),
    Color::Rgb(80, 40, 0),
    Color::Rgb(50, 0, 90),
];

/// 8 distinct foreground colors for auto-coloring system identifiers.
const ID_FG_COLORS: [Color; 8] = [
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::Rgb(255, 165, 0),
    Color::Rgb(180, 180, 255),
];

use rcal::QName;
use rcal::asb::get_asb_config_location;
use rcal::cal::{AbstractReader, MessageListener, TopicQos, get_cal};
use rcal::calconfig::{SerializationFormat, parse_config_from_file};
use rcal::externalizer::{PrettyExternalizer, XmlExternalizer, read_from_bytes, write_to_bytes};
use rcal::uci::types::{SecurityInformationType, SecurityInformationType_};
use rcal::uci::{CalError, CalErrorKind, CalImplementationErrorKind, CalMessage, CalResult};

// ── AnyMsg ───────────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct AnyMsg(serde_json::Value);

impl CalMessage for AnyMsg {
    fn message_type_name() -> QName {
        QName::new(None, "any")
    }
    fn cal_create() -> Self {
        AnyMsg(serde_json::Value::Object(Default::default()))
    }
}

// ── Received message record ───────────────────────────────────────────────────

struct ReceivedMsg {
    topic: String,
    received_at: Instant,
    wall_time: DateTime<Utc>,
    identifier: String,
    raw: serde_json::Value,
}

// ── Save / load ───────────────────────────────────────────────────────────────
//
// Format: one line per message, each line is:
//   <RcalMessageRecord><WallTime>…</WallTime><Identifier>…</Identifier><Topic>…</Topic><Message>{message xml}</Message></RcalMessageRecord>
// The message XML retains its xmlns attributes; no xmlns on the wrapper element.

fn save_to_file(
    path: &str,
    records: &[(DateTime<Utc>, String, String, serde_json::Value)],
) -> Result<usize, String> {
    let ext = XmlExternalizer::new(SerializationFormat::Xml);
    let mut lines = Vec::with_capacity(records.len());
    for (wall_time, identifier, topic, raw) in records {
        let msg = AnyMsg(raw.clone());
        let xml_bytes = write_to_bytes(&ext, &msg, topic).map_err(|e| e.to_string())?;
        let xml = String::from_utf8(xml_bytes).map_err(|e| e.to_string())?;
        let xml = xml.trim_end_matches('\n');
        lines.push(format!(
            "<RcalMessageRecord><WallTime>{}</WallTime><Identifier>{}</Identifier><Topic>{}</Topic><Message>{xml}</Message></RcalMessageRecord>",
            wall_time.to_rfc3339(),
            identifier,
            topic,
        ));
    }
    std::fs::write(path, lines.join("\n") + "\n").map_err(|e| e.to_string())?;
    Ok(records.len())
}

fn load_from_file(path: &str) -> Result<(Vec<ReceivedMsg>, Instant), String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let ext = XmlExternalizer::new(SerializationFormat::Xml);
    let start = Instant::now();
    let mut first_wall: Option<DateTime<Utc>> = None;
    let mut messages = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with("<RcalMessageRecord>") {
            continue;
        }
        let Some(topic) = tag_text(line, "Topic") else {
            continue;
        };
        let Some(message_xml) = tag_inner(line, "Message") else {
            continue;
        };

        let wall_time = tag_text(line, "WallTime")
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let identifier = tag_text(line, "Identifier").unwrap_or_default();

        if first_wall.is_none() {
            first_wall = Some(wall_time);
        }
        let received_at = first_wall
            .and_then(|first| wall_time.signed_duration_since(first).to_std().ok())
            .map(|d| start + d)
            .unwrap_or(start);

        let raw = read_from_bytes::<AnyMsg>(&ext, message_xml.as_bytes())
            .map(|m| m.0)
            .unwrap_or_else(|_| serde_json::Value::Object(Default::default()));

        messages.push(ReceivedMsg {
            topic,
            received_at,
            wall_time,
            identifier,
            raw,
        });
    }
    Ok((messages, start))
}

/// Extract the text content of the first `<tag>…</tag>` in `s`.
fn tag_text(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(s[start..end].to_string())
}

/// Extract the raw inner content of `<tag>…</tag>`, using rfind for the close
/// so that nested XML inside (which uses different element names) is preserved.
fn tag_inner(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s.rfind(&close)?;
    if end < start {
        return None;
    }
    Some(s[start..end].to_string())
}

fn extract_identifier(v: &serde_json::Value) -> String {
    let hdr = v.get("MessageHeader");
    if let Some(label) = hdr
        .and_then(|h| h.get("SystemID"))
        .and_then(|s| s.get("DescriptiveLabel"))
        .and_then(|d| d.as_str())
        .filter(|s| !s.is_empty())
    {
        return label.to_string();
    }
    if let Some(uuid) = hdr
        .and_then(|h| h.get("SystemID"))
        .and_then(|s| s.get("UUID"))
        .and_then(|u| u.as_str())
        .filter(|s| !s.is_empty())
    {
        return uuid.to_string();
    }
    String::new()
}

// ── Listener ──────────────────────────────────────────────────────────────────

struct Collector {
    topic: String,
    state: Arc<Mutex<AppState>>,
    notify: Arc<tokio::sync::Notify>,
    logger: slog::Logger,
}

fn extract_header_str<'a>(v: &'a serde_json::Value, field: &str) -> &'a str {
    v.get("MessageHeader")
        .and_then(|h| h.get(field))
        .and_then(|f| f.as_str())
        .unwrap_or("")
}

fn extract_system_id(v: &serde_json::Value) -> String {
    let hdr = v.get("MessageHeader");
    if let Some(label) = hdr
        .and_then(|h| h.get("SystemID"))
        .and_then(|s| s.get("DescriptiveLabel"))
        .and_then(|d| d.as_str())
        .filter(|s| !s.is_empty())
    {
        return label.to_string();
    }
    hdr.and_then(|h| h.get("SystemID"))
        .and_then(|s| s.get("UUID"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string()
}

impl MessageListener<AnyMsg> for Collector {
    fn on_message(&self, msg: &Arc<AnyMsg>) {
        let raw = msg.0.clone();
        let norm = normalize_value(&raw);
        let identifier = extract_identifier(&norm);
        let classification = norm
            .get("MessageHeader")
            .and_then(|h| h.get("SecurityInformation"))
            .and_then(|si| serde_json::from_value::<SecurityInformationType_>(si.clone()).ok())
            .map(|si| si.to_banner())
            .unwrap_or_default();
        let system_id = extract_system_id(&norm);
        let service_id = extract_header_str(&norm, "ServiceID");
        let msg_type = AnyMsg::message_type_name().to_string();
        slog::debug!(
            self.logger,
            "message received";
            "topic" => &self.topic,
            "msg_type" => &msg_type,
            "classification" => &classification,
            "system_id" => &system_id,
            "service_id" => service_id,
        );
        let record = ReceivedMsg {
            topic: self.topic.clone(),
            received_at: Instant::now(),
            wall_time: Utc::now(),
            identifier,
            raw,
        };
        if let Ok(mut st) = self.state.lock() {
            st.messages.push(record);
        }
        self.notify.notify_one();
    }
}

// ── View / sort modes ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Form,
    Xml,
    Pretty,
}

#[derive(Clone, Copy, PartialEq)]
enum SortField {
    Time,
    Topic,
}

#[derive(Clone, Copy, PartialEq)]
enum TimeFmt {
    Iso,
    Elapsed,
}

// ── App state ─────────────────────────────────────────────────────────────────

struct AppState {
    messages: Vec<ReceivedMsg>,
    start: Instant,
}

struct Ui {
    state: Arc<Mutex<AppState>>,
    list_state: ListState,
    view: ViewMode,
    sort: SortField,
    time_fmt: TimeFmt,
    filter: String,
    filter_input: bool,
    collapsed: HashSet<String>, // dot-joined path keys
    detail_scroll: u16,
    detail_focused: bool,
    detail_cursor: usize,
    form_line_cache: Vec<FormLine>,
    /// topic -> index into LOCK_BG_COLORS (locked by 'h')
    locked_highlights: HashMap<String, usize>,
    /// identifier -> index into ID_FG_COLORS (auto-assigned)
    system_colors: HashMap<String, usize>,
    next_system_color: usize,
    save_input: bool,
    save_path: String,
    status_msg: Option<String>,
}

impl Ui {
    fn new(state: Arc<Mutex<AppState>>) -> Self {
        let mut list_state = ListState::default();
        list_state.select(None);
        Self {
            state,
            list_state,
            view: ViewMode::Form,
            sort: SortField::Time,
            time_fmt: TimeFmt::Elapsed,
            filter: String::new(),
            filter_input: false,
            collapsed: HashSet::new(),
            detail_scroll: 0,
            detail_focused: false,
            detail_cursor: 0,
            form_line_cache: Vec::new(),
            locked_highlights: HashMap::new(),
            system_colors: HashMap::new(),
            next_system_color: 0,
            save_input: false,
            save_path: String::new(),
            status_msg: None,
        }
    }

    fn visible_indices(&self, messages: &[ReceivedMsg]) -> Vec<usize> {
        let filter_lc = self.filter.to_lowercase();
        let mut idxs: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| filter_lc.is_empty() || m.topic.to_lowercase().contains(&filter_lc))
            .map(|(i, _)| i)
            .collect();
        match self.sort {
            SortField::Time => {} // already insertion order = time order
            SortField::Topic => {
                idxs.sort_by(|&a, &b| messages[a].topic.cmp(&messages[b].topic).then(a.cmp(&b)));
            }
        }
        idxs
    }

    fn draw(&mut self, f: &mut Frame) {
        let messages = {
            let st = self.state.lock().unwrap();
            // We need to release the lock before drawing, so collect what we need.
            // We borrow indices and render info rather than cloning large values.
            st.messages.len()
        };

        // Layout
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(f.area());

        let main = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(outer[0]);

        let st = self.state.lock().unwrap();
        let idxs = self.visible_indices(&st.messages);

        // Pre-assign system colors for newly seen identifiers.
        for &i in &idxs {
            let id = &st.messages[i].identifier;
            if !id.is_empty() && !self.system_colors.contains_key(id) {
                let c = self.next_system_color % ID_FG_COLORS.len();
                self.next_system_color += 1;
                self.system_colors.insert(id.clone(), c);
            }
        }

        // Topic of the currently selected message for transient same-topic highlight.
        let selected_topic: Option<&str> = self
            .list_state
            .selected()
            .and_then(|si| idxs.get(si))
            .map(|&i| st.messages[i].topic.as_str());

        // Left pane — message list
        let items: Vec<ListItem> = idxs
            .iter()
            .enumerate()
            .map(|(vis_i, &i)| {
                let m = &st.messages[i];
                let time_str = match self.time_fmt {
                    TimeFmt::Iso => m.wall_time.format("%T%.3f").to_string(),
                    TimeFmt::Elapsed => {
                        let e = m.received_at.duration_since(st.start).as_secs_f32();
                        format!("{e:8.2}s")
                    }
                };
                let id_str: &str = if m.identifier.is_empty() {
                    &m.topic
                } else {
                    &m.identifier
                };
                let id_color = self
                    .system_colors
                    .get(id_str)
                    .map(|&ci| ID_FG_COLORS[ci])
                    .unwrap_or(Color::Gray);

                let line = Line::from(vec![
                    Span::raw(format!("{time_str}  ")),
                    Span::styled(
                        format!("{:<20}", m.topic),
                        Style::default().fg(Color::White),
                    ),
                    Span::raw("  "),
                    Span::styled(id_str.to_string(), Style::default().fg(id_color)),
                ]);

                // Row background: locked > transient same-topic > default.
                // (selected item bg is overridden by list highlight_style.)
                let is_selected = self.list_state.selected() == Some(vis_i);
                let row_bg = if is_selected {
                    None
                } else if let Some(&ci) = self.locked_highlights.get(&m.topic) {
                    Some(LOCK_BG_COLORS[ci])
                } else if selected_topic == Some(m.topic.as_str()) {
                    Some(Color::Rgb(35, 55, 55))
                } else {
                    None
                };

                let item = ListItem::new(line);
                if let Some(bg) = row_bg {
                    item.style(Style::default().bg(bg))
                } else {
                    item
                }
            })
            .collect();

        let list_title = format!(
            " Messages ({}/{}) [sort:{}] ",
            idxs.len(),
            messages,
            match self.sort {
                SortField::Time => "time",
                SortField::Topic => "topic",
            }
        );
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(list_title))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        f.render_stateful_widget(list, main[0], &mut self.list_state);

        // Right pane — detail
        let detail_height = main[1].height.saturating_sub(2) as usize; // minus borders

        let detail_text = match self.list_state.selected().and_then(|i| idxs.get(i)) {
            None => {
                self.form_line_cache.clear();
                vec![Line::from("(no message selected)")]
            }
            Some(&idx) => {
                let m = &st.messages[idx];
                match self.view {
                    ViewMode::Pretty => {
                        self.form_line_cache.clear();
                        let ext = PrettyExternalizer::new();
                        let amsg = AnyMsg(m.raw.clone());
                        write_to_bytes(&ext, &amsg, &m.topic)
                            .ok()
                            .and_then(|b| String::from_utf8(b).ok())
                            .unwrap_or_else(|| "(render error)".to_string())
                            .lines()
                            .map(|l| Line::from(l.to_string()))
                            .collect()
                    }
                    ViewMode::Xml => {
                        self.form_line_cache.clear();
                        let ext = XmlExternalizer::new(SerializationFormat::PrettyXml);
                        let amsg = AnyMsg(m.raw.clone());
                        write_to_bytes(&ext, &amsg, &m.topic)
                            .ok()
                            .and_then(|b| String::from_utf8(b).ok())
                            .unwrap_or_else(|| "(xml error)".to_string())
                            .lines()
                            .map(|l| Line::from(l.to_string()))
                            .collect()
                    }
                    ViewMode::Form => {
                        let norm = normalize_value(&m.raw);
                        let form_lines = render_form(&norm, &self.collapsed);
                        let total = form_lines.len();
                        // Clamp cursor
                        if self.detail_cursor >= total && total > 0 {
                            self.detail_cursor = total - 1;
                        }
                        // Auto-scroll to keep cursor visible
                        if self.detail_focused && total > 0 {
                            let scroll = self.detail_scroll as usize;
                            if self.detail_cursor < scroll {
                                self.detail_scroll = self.detail_cursor as u16;
                            } else if self.detail_cursor >= scroll + detail_height {
                                self.detail_scroll =
                                    (self.detail_cursor + 1).saturating_sub(detail_height) as u16;
                            }
                        }
                        let cursor = if self.detail_focused {
                            Some(self.detail_cursor)
                        } else {
                            None
                        };
                        let lines = form_lines
                            .iter()
                            .enumerate()
                            .map(|(i, fl)| {
                                if cursor == Some(i) {
                                    Line::from(
                                        fl.line
                                            .spans
                                            .iter()
                                            .map(|s| {
                                                Span::styled(
                                                    s.content.clone(),
                                                    s.style.bg(Color::DarkGray),
                                                )
                                            })
                                            .collect::<Vec<_>>(),
                                    )
                                } else {
                                    fl.line.clone()
                                }
                            })
                            .collect();
                        self.form_line_cache = form_lines;
                        lines
                    }
                }
            }
        };

        let view_label = match self.view {
            ViewMode::Form => "Form",
            ViewMode::Xml => "XML",
            ViewMode::Pretty => "Tree",
        };
        let detail_border_style = if self.detail_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let detail = Paragraph::new(detail_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(detail_border_style)
                    .title(format!(" Detail [{view_label}] ")),
            )
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));

        drop(st);

        f.render_widget(detail, main[1]);

        // Status bar
        let filter_part = if self.filter_input {
            format!("Filter: {}█", self.filter)
        } else if self.filter.is_empty() {
            "[/] filter  ".to_string()
        } else {
            format!("Filter: {}  [Esc clear]  ", self.filter)
        };
        let status = if self.save_input {
            format!("Save to: {}█  [Enter] save  [Esc] cancel", self.save_path)
        } else if let Some(ref msg) = self.status_msg {
            msg.clone()
        } else if self.detail_focused {
            "Detail: [j/k] navigate  [Space/Enter] collapse  [y] copy  [Tab/Esc] back  [q] quit"
                .to_string()
        } else {
            format!(
                "{}  [Tab] detail  [s] sort  [i] time  [F]orm [X]ml [T]ree  [h] lock color  [H] clear  [w] save  [q] quit",
                filter_part
            )
        };
        f.render_widget(
            Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
            outer[1],
        );
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        // Quit always wins
        if matches!(code, KeyCode::Char('q'))
            || (matches!(code, KeyCode::Char('c')) && modifiers.contains(KeyModifiers::CONTROL))
        {
            return true;
        }

        self.status_msg = None;

        if self.save_input {
            match code {
                KeyCode::Esc => {
                    self.save_input = false;
                    self.save_path.clear();
                }
                KeyCode::Enter => {
                    self.save_input = false;
                    let path = std::mem::take(&mut self.save_path);
                    let records: Vec<(DateTime<Utc>, String, String, serde_json::Value)> = {
                        let st = self.state.lock().unwrap();
                        let idxs = self.visible_indices(&st.messages);
                        idxs.iter()
                            .map(|&i| {
                                let m = &st.messages[i];
                                (
                                    m.wall_time,
                                    m.identifier.clone(),
                                    m.topic.clone(),
                                    m.raw.clone(),
                                )
                            })
                            .collect()
                    };
                    match save_to_file(&path, &records) {
                        Ok(n) => {
                            self.status_msg = Some(format!("Saved {n} messages to {path}"));
                        }
                        Err(e) => {
                            self.status_msg = Some(format!("Save failed: {e}"));
                        }
                    }
                }
                KeyCode::Backspace => {
                    self.save_path.pop();
                }
                KeyCode::Char(c) => {
                    self.save_path.push(c);
                }
                _ => {}
            }
            return false;
        }

        if self.filter_input {
            match code {
                KeyCode::Esc => {
                    self.filter_input = false;
                    self.filter.clear();
                    self.list_state.select(None);
                }
                KeyCode::Enter => {
                    self.filter_input = false;
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                }
                _ => {}
            }
            return false;
        }

        if self.detail_focused {
            match code {
                KeyCode::Tab | KeyCode::Esc => {
                    self.detail_focused = false;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let max = self.form_line_cache.len().saturating_sub(1);
                    self.detail_cursor = (self.detail_cursor + 1).min(max);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.detail_cursor = self.detail_cursor.saturating_sub(1);
                }
                KeyCode::Char(' ') | KeyCode::Enter => {
                    if let Some(path) = self
                        .form_line_cache
                        .get(self.detail_cursor)
                        .and_then(|fl| fl.collapsible_path.clone())
                    {
                        if self.collapsed.contains(&path) {
                            self.collapsed.remove(&path);
                        } else {
                            self.collapsed.insert(path);
                        }
                    }
                }
                KeyCode::Char('y') => {
                    if let Some(val) = self
                        .form_line_cache
                        .get(self.detail_cursor)
                        .and_then(|fl| fl.leaf_value.as_deref())
                    {
                        copy_to_clipboard(val);
                    }
                }
                _ => {}
            }
            return false;
        }

        let st = self.state.lock().unwrap();
        let count = self.visible_indices(&st.messages).len();
        drop(st);

        match code {
            KeyCode::Tab => {
                if self.view == ViewMode::Form && self.list_state.selected().is_some() {
                    self.detail_focused = true;
                    self.detail_cursor = 0;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = match self.list_state.selected() {
                    None if count > 0 => Some(0),
                    Some(i) if i + 1 < count => Some(i + 1),
                    other => other,
                };
                self.list_state.select(next);
                self.detail_scroll = 0;
                self.form_line_cache.clear();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let next = match self.list_state.selected() {
                    Some(0) | None => None,
                    Some(i) => Some(i - 1),
                };
                self.list_state.select(next);
                self.detail_scroll = 0;
                self.form_line_cache.clear();
            }
            KeyCode::PageDown | KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.detail_scroll = self.detail_scroll.saturating_add(10);
            }
            KeyCode::PageUp | KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10);
            }
            KeyCode::Char('/') => {
                self.filter_input = true;
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.list_state.select(None);
            }
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    SortField::Time => SortField::Topic,
                    SortField::Topic => SortField::Time,
                };
                self.list_state.select(None);
            }
            KeyCode::Char('i') => {
                self.time_fmt = match self.time_fmt {
                    TimeFmt::Iso => TimeFmt::Elapsed,
                    TimeFmt::Elapsed => TimeFmt::Iso,
                };
            }
            KeyCode::Char('F') => {
                self.view = ViewMode::Form;
                self.detail_scroll = 0;
                self.detail_focused = false;
            }
            KeyCode::Char('X') => {
                self.view = ViewMode::Xml;
                self.detail_scroll = 0;
                self.detail_focused = false;
            }
            KeyCode::Char('T') => {
                self.view = ViewMode::Pretty;
                self.detail_scroll = 0;
                self.detail_focused = false;
            }
            KeyCode::Char('h') => {
                // Lock/cycle highlight color for selected message's topic.
                let st = self.state.lock().unwrap();
                let idxs = self.visible_indices(&st.messages);
                if let Some(&msg_i) = self.list_state.selected().and_then(|si| idxs.get(si)) {
                    let topic = st.messages[msg_i].topic.clone();
                    drop(st);
                    let next = self
                        .locked_highlights
                        .get(&topic)
                        .map(|&ci| (ci + 1) % LOCK_BG_COLORS.len())
                        .unwrap_or(0);
                    self.locked_highlights.insert(topic, next);
                }
            }
            KeyCode::Char('H') => {
                self.locked_highlights.clear();
            }
            KeyCode::Char('w') => {
                self.save_input = true;
                self.save_path.clear();
            }
            _ => {}
        }
        false
    }
}

// ── Value normalizer ──────────────────────────────────────────────────────────

/// Strip XML metadata keys (`$text` promotion, `xmlns` namespace attrs) so
/// the Form view doesn't expose deserialization artefacts.
fn normalize_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(t) => {
            if let Some(text) = t.get("$text") {
                let has_only_meta = t.keys().all(|k| k == "$text" || is_xml_meta_key(k));
                if has_only_meta {
                    return normalize_value(text);
                }
            }
            let filtered: serde_json::Map<_, _> = t
                .iter()
                .filter(|(k, _)| !is_xml_meta_key(k))
                .map(|(k, val)| (k.clone(), normalize_value(val)))
                .collect();
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(normalize_value).collect())
        }
        other => other.clone(),
    }
}

fn is_xml_meta_key(k: &str) -> bool {
    k == "$text" || k == "xmlns" || k.starts_with("xmlns:") || k.starts_with("@xmlns")
}

// ── Form renderer ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct FormLine {
    line: Line<'static>,
    /// Path to toggle on Space/Enter (collapsible table/array headers only)
    collapsible_path: Option<String>,
    /// Value string to copy on 'y' (leaf nodes only)
    leaf_value: Option<String>,
}

fn render_form(v: &serde_json::Value, collapsed: &HashSet<String>) -> Vec<FormLine> {
    let mut lines = Vec::new();
    render_value(v, "", 0, collapsed, &mut lines);
    lines
}

fn render_value(
    v: &serde_json::Value,
    path: &str,
    depth: usize,
    collapsed: &HashSet<String>,
    out: &mut Vec<FormLine>,
) {
    let indent = "  ".repeat(depth);

    match v {
        serde_json::Value::Object(t) => {
            for (k, child) in t {
                let child_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                let child_collapsed = collapsed.contains(&child_path);
                match child {
                    serde_json::Value::Object(inner) => {
                        let marker = if child_collapsed { "▶" } else { "▼" };
                        let count = if child_collapsed {
                            format!(" ({} fields)", inner.len())
                        } else {
                            String::new()
                        };
                        out.push(FormLine {
                            line: Line::from(vec![
                                Span::styled(
                                    format!("{indent}{marker} "),
                                    Style::default().fg(Color::Yellow),
                                ),
                                Span::styled(
                                    format!("{k}{count}"),
                                    Style::default()
                                        .fg(Color::Yellow)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]),
                            collapsible_path: Some(child_path.clone()),
                            leaf_value: None,
                        });
                        if !child_collapsed {
                            render_value(child, &child_path, depth + 1, collapsed, out);
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        let marker = if child_collapsed { "▶" } else { "▼" };
                        let count = if child_collapsed {
                            format!(" [{} items]", arr.len())
                        } else {
                            String::new()
                        };
                        out.push(FormLine {
                            line: Line::from(vec![
                                Span::styled(
                                    format!("{indent}{marker} "),
                                    Style::default().fg(Color::Cyan),
                                ),
                                Span::styled(
                                    format!("{k}{count}"),
                                    Style::default()
                                        .fg(Color::Cyan)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]),
                            collapsible_path: Some(child_path.clone()),
                            leaf_value: None,
                        });
                        if !child_collapsed {
                            for (i, item) in arr.iter().enumerate() {
                                let item_path = format!("{child_path}[{i}]");
                                out.push(FormLine {
                                    line: Line::from(Span::styled(
                                        format!("{indent}  [{i}]"),
                                        Style::default().fg(Color::Cyan),
                                    )),
                                    collapsible_path: None,
                                    leaf_value: None,
                                });
                                render_value(item, &item_path, depth + 2, collapsed, out);
                            }
                        }
                    }
                    leaf => {
                        let val_str = leaf_display(leaf);
                        out.push(FormLine {
                            line: Line::from(vec![
                                Span::raw(format!("{indent}  ")),
                                Span::styled(format!("{k}: "), Style::default().fg(Color::Green)),
                                Span::raw(val_str.clone()),
                            ]),
                            collapsible_path: None,
                            leaf_value: Some(val_str),
                        });
                    }
                }
            }
        }
        other => {
            let val_str = leaf_display(other);
            out.push(FormLine {
                line: Line::from(Span::raw(format!("{indent}{val_str}"))),
                collapsible_path: None,
                leaf_value: Some(val_str),
            });
        }
    }
}

fn leaf_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Array(a) => format!("[{} items]", a.len()),
        serde_json::Value::Object(t) => format!("{{{} fields}}", t.len()),
    }
}

// ── Clipboard ─────────────────────────────────────────────────────────────────

fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    // Try Wayland, then X11 variants
    let candidates: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (cmd, args) in candidates {
        if let Ok(mut child) = Command::new(cmd).args(*args).stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> CalResult<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(path) = args.get(1) {
        let (messages, start) = load_from_file(path)
            .map_err(|e| CalError::new(CalErrorKind::InitializationFailure, e))?;
        let app_state = Arc::new(Mutex::new(AppState { messages, start }));
        let notify = Arc::new(tokio::sync::Notify::new());
        return run_tui(app_state, notify);
    }

    let config_path = get_asb_config_location(None)?;
    let config = Arc::new(parse_config_from_file(&config_path)?);

    // Find service — prefer "asb_view", fall back to first service with topics
    let service = config
        .get_service("asb_view")
        .or_else(|| config.service.iter().find(|s| !s.topic.is_empty()))
        .ok_or_else(|| {
            CalError::new(
                CalErrorKind::InitializationFailure,
                "no [[service]] with topics in config",
            )
        })?
        .clone();

    let tconfig = config
        .get_transport_for_service(&service.id)
        .ok_or_else(|| {
            CalError::new(
                CalErrorKind::InitializationFailure,
                format!("no transport configured for service \"{}\"", service.id),
            )
        })?
        .clone();

    let logger = rcal::logging::build_logger(&config.system.logging);
    slog::info!(logger, "asb_view starting"; "config" => &config_path);
    slog::debug!(logger, "using transport"; "id" => &tconfig.id);

    let mut bus = get_cal(
        service.id.clone(),
        Some(tconfig.id.clone()),
        Arc::clone(&config),
        logger.clone(),
    )
    .await?;

    let start = Instant::now();
    let app_state = Arc::new(Mutex::new(AppState {
        messages: Vec::new(),
        start,
    }));
    let notify = Arc::new(tokio::sync::Notify::new());

    let mut readers: Vec<Box<dyn AbstractReader<AnyMsg>>> = Vec::new();
    for topic in &service.topic {
        slog::debug!(logger, "subscribing to topic"; "id" => &topic.id);
        let mut reader = bus.create_reader::<AnyMsg>(&topic.id, TopicQos::default())?;
        reader.add_listener(Arc::new(Collector {
            topic: topic.id.clone(),
            state: Arc::clone(&app_state),
            notify: Arc::clone(&notify),
            logger: logger.clone(),
        }))?;
        readers.push(reader);
    }

    if readers.is_empty() {
        return Err(CalError::new(
            CalErrorKind::InitializationFailure,
            "no topics configured — add [[service.topic]] entries",
        ));
    }
    slog::info!(logger, "listening"; "topics" => readers.len());

    // Run TUI in a blocking thread so tokio can keep the CAL listeners alive.
    let state_clone = Arc::clone(&app_state);
    let notify_clone = Arc::clone(&notify);
    let tui_result = tokio::task::spawn_blocking(move || run_tui(state_clone, notify_clone))
        .await
        .map_err(|e| {
            CalError::new_impl(
                CalImplementationErrorKind::UserInterfaceError,
                e.to_string(),
            )
        })??;

    Ok(tui_result)
}

fn run_tui(state: Arc<Mutex<AppState>>, _notify: Arc<tokio::sync::Notify>) -> CalResult<()> {
    enable_raw_mode().map_err(|e| {
        CalError::new_impl(
            CalImplementationErrorKind::UserInterfaceError,
            e.to_string(),
        )
    })?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| {
        CalError::new_impl(
            CalImplementationErrorKind::UserInterfaceError,
            e.to_string(),
        )
    })?;

    let mut terminal = ratatui::init();
    let mut ui = Ui::new(Arc::clone(&state));

    loop {
        terminal.draw(|f| ui.draw(f)).map_err(|e| {
            CalError::new_impl(
                CalImplementationErrorKind::UserInterfaceError,
                e.to_string(),
            )
        })?;

        // Poll for keyboard events with a short timeout so we also redraw on
        // incoming messages (notified via the Notify channel from listeners).
        if event::poll(Duration::from_millis(50)).map_err(|e| {
            CalError::new_impl(
                CalImplementationErrorKind::UserInterfaceError,
                e.to_string(),
            )
        })? {
            if let Event::Key(key) = event::read().map_err(|e| {
                CalError::new_impl(
                    CalImplementationErrorKind::UserInterfaceError,
                    e.to_string(),
                )
            })? {
                if key.kind == KeyEventKind::Press && ui.handle_key(key.code, key.modifiers) {
                    break;
                }
            }
        }
    }

    ratatui::restore();
    Ok(())
}
