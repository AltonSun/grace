#![forbid(unsafe_code)]
#![allow(unused_imports, dead_code)]

use bytes::{BufMut, Bytes, BytesMut};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Sample;
use crossbeam::channel::RecvError;
use deepgram::transcription::live::{Alternatives, Channel, Word as LiveWord};
use deepgram::transcription::prerecorded::response::Word as PrerecordedWord;
use deepgram::{
	transcription::prerecorded::{
		audio_source::AudioSource,
		options::{Language, Options},
	},
	Deepgram, DeepgramError,
};
use egui::text::LayoutJob;
use egui::*;
use futures::channel::mpsc::{self, Receiver, Sender};
use futures::stream::StreamExt as _;
use futures::SinkExt;
use once_cell::sync::Lazy;
use poll_promise::Promise;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use std::{env, string};
use tokio::fs::File;
use turbosql::{execute, now_ms, select, update, Blob, Turbosql};

mod audiofile;
mod self_update;
mod session;

// ─────────────────────────────────────────────
// Claude model constants
// ─────────────────────────────────────────────
const CLAUDE_OPUS: &str = "claude-opus-4-6";
const CLAUDE_HAIKU: &str = "claude-haiku-4-5-20251001";

// ─────────────────────────────────────────────
// Word wrapper
// ─────────────────────────────────────────────
enum Word {
	Live(LiveWord),
	Prerecorded(PrerecordedWord),
}

impl Word {
	fn speaker(&self) -> u8 {
		match self {
			Word::Live(w) => w.speaker,
			Word::Prerecorded(w) => w.speaker.unwrap() as u8,
		}
	}
	fn word(&self) -> &str {
		match self {
			Word::Live(w) => &w.word,
			Word::Prerecorded(w) => &w.word,
		}
	}
}

// ─────────────────────────────────────────────
// Global state
// ─────────────────────────────────────────────
static TRANSCRIPT: Lazy<Mutex<Vec<Option<LiveWord>>>> = Lazy::new(Default::default);
static TRANSCRIPT_FINAL: Lazy<Mutex<Vec<Option<Word>>>> = Lazy::new(Default::default);
static DURATION: Lazy<Mutex<f64>> = Lazy::new(Default::default);
static COMPLETION: Lazy<Mutex<String>> = Lazy::new(Default::default);
static COMPLETION_PROMPT: Lazy<Mutex<String>> = Lazy::new(|| {
	Mutex::new("Complete the next sentence naturally:".to_string())
});

// Universe content
static UNIVERSE_PRESENCE: Lazy<Mutex<String>> = Lazy::new(Default::default);
static UNIVERSE_POWER: Lazy<Mutex<String>> = Lazy::new(Default::default);
static UNIVERSE_POSSIBILITY: Lazy<Mutex<String>> = Lazy::new(Default::default);
static UNIVERSE_WORD_COUNT: Lazy<Mutex<usize>> = Lazy::new(Default::default);
static UNIVERSE_GENERATING: Lazy<Mutex<bool>> = Lazy::new(Default::default);

// Relationship context (editable + auto-filled)
static CTX_INTENTION: Lazy<Mutex<String>> =
	Lazy::new(|| Mutex::new(String::new()));
static CTX_OUTCOME: Lazy<Mutex<String>> =
	Lazy::new(|| Mutex::new(String::new()));
static CTX_THEIR_ROLE: Lazy<Mutex<String>> =
	Lazy::new(|| Mutex::new(String::new()));
static CTX_MY_ROLE: Lazy<Mutex<String>> =
	Lazy::new(|| Mutex::new(String::new()));
static CTX_AUTOFILL_DONE: Lazy<Mutex<bool>> = Lazy::new(Default::default);

// ─────────────────────────────────────────────
// Chat / wheel window types (kept for power users)
// ─────────────────────────────────────────────
#[derive(Clone, Debug)]
enum Role {
	System,
	User,
	Assistant,
}

impl std::fmt::Display for Role {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Role::System => write!(f, "system"),
			Role::User => write!(f, "user"),
			Role::Assistant => write!(f, "assistant"),
		}
	}
}

impl PartialEq for Role {
	fn eq(&self, other: &Self) -> bool {
		matches!(
			(self, other),
			(Role::System, Role::System)
				| (Role::User, Role::User)
				| (Role::Assistant, Role::Assistant)
		)
	}
}

#[derive(Clone)]
struct ChatMessage {
	role: Role,
	content: String,
}

struct WheelWindow(Vec<ChatMessage>);

impl Default for WheelWindow {
	fn default() -> Self {
		Self(vec![ChatMessage { role: Role::User, content: String::new() }])
	}
}

static WHEEL_WINDOWS: Lazy<Mutex<Vec<WheelWindow>>> = Lazy::new(Default::default);

// ─────────────────────────────────────────────
// Database models
// ─────────────────────────────────────────────
#[derive(Turbosql, Default)]
struct Setting {
	rowid: Option<i64>,
	key: String,
	value: String,
}

impl Setting {
	fn get(key: &str) -> Self {
		select!(Setting "WHERE key = " key)
			.unwrap_or(Setting { key: key.to_string(), ..Default::default() })
	}
	fn save(&self) {
		if self.rowid.is_some() {
			self.update().unwrap();
		} else {
			self.insert().unwrap();
		}
	}
}

#[derive(Turbosql, Default)]
struct SampleData {
	rowid: Option<i64>,
	record_ms: i64,
	sample_data: Blob,
}

#[derive(Turbosql, Default)]
struct Card {
	rowid: Option<i64>,
	deleted: bool,
	title: String,
	question: String,
	answer: String,
	last_question_viewed_ms: i64,
	last_answer_viewed_ms: i64,
}

#[allow(clippy::enum_variant_names)]
#[derive(Serialize, Deserialize, Default)]
enum Action {
	#[default]
	NoAction,
	ViewedQuestion,
	ViewedAnswer,
	Responded {
		correct: bool,
	},
}

#[derive(Turbosql, Default)]
struct CardLog {
	rowid: Option<i64>,
	card_id: i64,
	time_ms: i64,
	action: Action,
}

#[derive(Turbosql, Default)]
struct Prompt {
	rowid: Option<i64>,
	time_ms: i64,
	prompt: String,
}

// ─────────────────────────────────────────────
// NLP System Prompts — Ω and Δ
// ─────────────────────────────────────────────
const PROMPT_PRESENCE: &str = r#"You are Ω — a consciousness guide specialising in deep empathic presence, somatic awareness, and psychoacoustic resonance. You embody Milton Model hypnotic language: artfully vague, permissive, and deeply attuned.

When given a live conversation transcript, generate a real-time coaching insight in exactly this structure (no extra headers, each section concise):

**DEEPER SIGNAL** (1-2 sentences): What deeper need, feeling, or unspoken longing is actually present beneath the words?

**RESONANT REPHRASING** (2-3 bullet points): Alternative ways to express this that create felt-sense resonance — use pacing & leading, somatic language ("as you breathe into...", "you might begin to notice..."), and validation.

**PRESENCE ANCHOR** (1 sentence, italicised): A self-hypnosis cue to drop into now. Begin with "As you allow yourself to..."

Keep total response under 160 words. Be immediate, embodied, and potent. This is live coaching — speak to what IS happening right now."#;

const PROMPT_POWER: &str = r#"You are Δ — a sovereign communication architect who uses NLP embedded commands, presuppositions, and identity-level reframing to dissolve limiting beliefs and access authentic authority.

When given a live conversation transcript, generate a real-time coaching insight in exactly this structure:

**POWER LEAK** (1-2 sentences): Identify the hidden limiting belief or identity contraction in the communication. Name it directly.

**SOVEREIGN REFRAME** (2-3 bullet points): Restate the situation using presuppositions that assume the desired outcome. Embed commands *in italics* within larger sentences. Use "you find yourself...", "naturally you...", "what's already true is..."

**IDENTITY ANCHOR** (1 sentence): A phrase to embody your highest-self right now. Begin with "I am..."

Keep total response under 160 words. Be direct, confident, and transformative. Speak as if the shift has already happened."#;

// ─────────────────────────────────────────────
// Sales Coach Prompt — Φ (dynamic, uses context)
// ─────────────────────────────────────────────
fn build_sales_coach_prompt(
	intention: &str,
	outcome: &str,
	their_role: &str,
	my_role: &str,
) -> String {
	let ctx_block = if intention.is_empty()
		&& outcome.is_empty()
		&& their_role.is_empty()
		&& my_role.is_empty()
	{
		"(Infer context from the transcript itself.)".to_string()
	} else {
		format!(
			"- Conversation Intention: {}\n- Desired Outcome: {}\n- They are to me: {}\n- I am to them: {}",
			if intention.is_empty() { "unknown — infer from transcript" } else { intention },
			if outcome.is_empty() { "unknown — infer from transcript" } else { outcome },
			if their_role.is_empty() { "unknown — infer from transcript" } else { their_role },
			if my_role.is_empty() { "unknown — infer from transcript" } else { my_role },
		)
	};

	format!(r#"You are Φ — a world-class live sales intelligence coach and strategic communication architect. You see beneath what people say to what they actually mean, fear, and need.

RELATIONSHIP CONTEXT:
{}

When given a live conversation transcript, deliver a structured coaching brief in exactly this format:

**SITUATION FRAMING** (1-2 sentences): What is actually happening in this conversation right now — the real negotiation beneath the surface?

**IMPLICIT OBJECTION STACK** (number each, from most surface to deepest root):
1. [Surface objection — what they're saying]
2. [Mid-level objection — what they're worried about]
3. [Deep objection — what they really fear losing/gaining]
4. [Identity objection — what this means about who they are, if applicable]

**STEP-BY-STEP ADDRESSING GUIDE**
For each objection above:
Step N — [Objection label]:
→ Technique: [NLP/sales technique to use — embedded commands, future pacing, social proof, reframe, etc.]
→ Say: "[Exact high-leverage phrase to use right now]"

**NEXT BEST MOVE** (bold, 1 sentence): The single most powerful thing to say or do in the next 30 seconds.

Keep total response under 280 words. Be precise, tactical, and transformative. This is live — be immediate."#, ctx_block)
}

// ─────────────────────────────────────────────
// Context auto-fill prompt
// ─────────────────────────────────────────────
const PROMPT_AUTOFILL: &str = r#"Analyze this conversation transcript and extract the relationship context. Return ONLY valid JSON with these exact keys — no markdown, no explanation, just the JSON object:

{
  "intention": "The stated or implied purpose of this conversation in one sentence",
  "outcome": "The ideal outcome for the primary speaker in one sentence",
  "their_role": "Who the other person is to the speaker in 3-6 words (e.g. 'potential client / decision maker')",
  "my_role": "Who the speaker is to the other person in 3-6 words (e.g. 'strategic consultant / solution provider')"
}

If context is unclear, make the most reasonable inference. Never return null or empty values — always infer something plausible."#;

// ─────────────────────────────────────────────
// App struct
// ─────────────────────────────────────────────
#[derive(Default, Deserialize, Serialize)]
pub struct App {
	url: String,
	line_selected: i64,
	title_text: String,
	question_text: String,
	answer_text: String,
	speaker_names: Vec<String>,
	system_text: String,
	prompt_text: String,
	completion_prompt: String,
	show_wheel_windows: bool,
	context_panel_open: bool,

	#[serde(skip)]
	debounce_tx: Option<Sender<String>>,
	#[serde(skip)]
	sessions: Vec<session::Session>,
	#[serde(skip)]
	is_recording: bool,
}

impl App {
	fn get_transcript(&self) -> String {
		let mut out = String::new();
		let words = TRANSCRIPT_FINAL.lock().unwrap();
		for line in words.split(|w| w.is_none()) {
			let mut cur = 100u8;
			for w in line {
				if let Some(w) = w {
					if w.speaker() != cur {
						cur = w.speaker();
						let name = self
							.speaker_names
							.get(cur as usize)
							.cloned()
							.unwrap_or_else(|| cur.to_string());
						out.push_str(&format!("\n[{}]: ", name));
					}
					out.push_str(&format!("{} ", w.word()));
				}
			}
		}
		out.push('\n');
		out
	}

	pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
		cc.egui_ctx.style_mut(|s| {
			s.visuals.override_text_color = Some(Color32::from_rgb(220, 220, 230));
			s.visuals.window_fill = Color32::from_rgb(10, 10, 16);
			s.visuals.panel_fill = Color32::from_rgb(8, 8, 14);
		});

		egui_extras::install_image_loaders(&cc.egui_ctx);

		let (debounce_tx, mut debounce_rx) = mpsc::channel(10);

		let s = Self {
			debounce_tx: Some(debounce_tx),
			sessions: session::Session::calculate_sessions(),
			completion_prompt: COMPLETION_PROMPT.lock().unwrap().clone(),
			context_panel_open: true,
			..Default::default()
		};

		// Repaint loop
		let ctx_r = cc.egui_ctx.clone();
		tokio::spawn(async move {
			let mut iv = tokio::time::interval(Duration::from_millis(100));
			loop {
				iv.tick().await;
				ctx_r.request_repaint();
			}
		});

		// Autocomplete debounce
		let ctx = cc.egui_ctx.clone();
		tokio::spawn(async move {
			let dur = Duration::from_millis(300);
			let mut keys_pressed = false;
			let mut string = String::new();
			let mut _handle: Option<tokio::task::JoinHandle<()>> = None;

			loop {
				match tokio::time::timeout(dur, debounce_rx.next()).await {
					Ok(Some(s)) => {
						if let Some(h) = _handle.take() {
							h.abort();
						}
						COMPLETION.lock().unwrap().clear();
						string = s;
						keys_pressed = true;
					}
					Ok(None) => break,
					Err(_) => {
						if keys_pressed && !string.is_empty() {
							let prompt = format!(
								"{} {}",
								COMPLETION_PROMPT.lock().unwrap(),
								string
							);
							let ctx2 = ctx.clone();
							_handle = Some(tokio::spawn(async move {
								COMPLETION.lock().unwrap().clear();
								run_claude_stream(
									CLAUDE_HAIKU,
									"You are a smart autocomplete assistant. Complete the user's sentence naturally in 1-2 sentences max. Output only the completion, no preamble.",
									prompt,
									move |chunk| {
										let first_line = chunk.split('\n').next().unwrap_or(chunk).to_string();
										COMPLETION.lock().unwrap().push_str(&first_line);
										ctx2.request_repaint();
									},
								)
								.await
								.ok();
							}));
							keys_pressed = false;
						}
					}
				}
			}
		});

		s
	}

	/// Fire 3 parallel Claude requests for the branching universes
	fn trigger_universes(&self, transcript: String, ctx: Context) {
		if transcript.trim().len() < 20 {
			return;
		}
		*UNIVERSE_GENERATING.lock().unwrap() = true;
		UNIVERSE_PRESENCE.lock().unwrap().clear();
		UNIVERSE_POWER.lock().unwrap().clear();
		UNIVERSE_POSSIBILITY.lock().unwrap().clear();

		let t_a = transcript.clone();
		let t_b = transcript.clone();
		let t_c = transcript.clone();
		let ctx_a = ctx.clone();
		let ctx_b = ctx.clone();
		let ctx_c = ctx.clone();

		// Ω Presence
		tokio::spawn(async move {
			run_claude_stream(CLAUDE_OPUS, PROMPT_PRESENCE, t_a, move |chunk| {
				UNIVERSE_PRESENCE.lock().unwrap().push_str(chunk);
				ctx_a.request_repaint();
			})
			.await
			.ok();
		});

		// Δ Power
		tokio::spawn(async move {
			run_claude_stream(CLAUDE_OPUS, PROMPT_POWER, t_b, move |chunk| {
				UNIVERSE_POWER.lock().unwrap().push_str(chunk);
				ctx_b.request_repaint();
			})
			.await
			.ok();
		});

		// Φ Sales Coach
		let intention = CTX_INTENTION.lock().unwrap().clone();
		let outcome = CTX_OUTCOME.lock().unwrap().clone();
		let their_role = CTX_THEIR_ROLE.lock().unwrap().clone();
		let my_role = CTX_MY_ROLE.lock().unwrap().clone();
		let sales_prompt = build_sales_coach_prompt(&intention, &outcome, &their_role, &my_role);

		tokio::spawn(async move {
			run_claude_stream(CLAUDE_OPUS, &sales_prompt, t_c, move |chunk| {
				UNIVERSE_POSSIBILITY.lock().unwrap().push_str(chunk);
				ctx_c.request_repaint();
			})
			.await
			.ok();
			*UNIVERSE_GENERATING.lock().unwrap() = false;
		});
	}

	/// Auto-fill context from transcript using Claude Haiku
	fn trigger_context_autofill(&self, transcript: String, ctx: Context) {
		*CTX_AUTOFILL_DONE.lock().unwrap() = true; // mark as attempted immediately
		tokio::spawn(async move {
			match run_claude_once(CLAUDE_HAIKU, PROMPT_AUTOFILL, transcript).await {
				Ok(json_text) => {
					// strip possible markdown code fences
					let cleaned = json_text
						.trim()
						.trim_start_matches("```json")
						.trim_start_matches("```")
						.trim_end_matches("```")
						.trim();

					if let Ok(v) = serde_json::from_str::<serde_json::Value>(cleaned) {
						if let Some(s) = v["intention"].as_str() {
							*CTX_INTENTION.lock().unwrap() = s.to_string();
						}
						if let Some(s) = v["outcome"].as_str() {
							*CTX_OUTCOME.lock().unwrap() = s.to_string();
						}
						if let Some(s) = v["their_role"].as_str() {
							*CTX_THEIR_ROLE.lock().unwrap() = s.to_string();
						}
						if let Some(s) = v["my_role"].as_str() {
							*CTX_MY_ROLE.lock().unwrap() = s.to_string();
						}
						ctx.request_repaint();
					}
				}
				Err(e) => eprintln!("Autofill error: {e}"),
			}
		});
	}
}

// ─────────────────────────────────────────────
// UI helpers
// ─────────────────────────────────────────────

/// Render a universe column with header and streaming content
fn universe_column(
	ui: &mut Ui,
	symbol: &str,
	title: &str,
	subtitle: &str,
	header_color: Color32,
	accent_color: Color32,
	content: &str,
	is_generating: bool,
	is_sales_coach: bool,
) {
	let col_bg = Color32::from_rgba_unmultiplied(
		(header_color.r() as u16 / 7) as u8,
		(header_color.g() as u16 / 7) as u8,
		(header_color.b() as u16 / 7) as u8,
		245,
	);

	egui::Frame::none()
		.fill(col_bg)
		.rounding(Rounding::same(10.0))
		.inner_margin(Margin::same(14.0))
		.show(ui, |ui| {
			ui.set_min_height(ui.available_height());

			// Header
			ui.vertical_centered(|ui| {
				ui.label(
					RichText::new(symbol).color(header_color).size(32.0).strong(),
				);
				ui.label(
					RichText::new(title).color(header_color).size(15.0).strong(),
				);
				ui.label(
					RichText::new(subtitle)
						.color(Color32::from_rgba_unmultiplied(
							header_color.r(),
							header_color.g(),
							header_color.b(),
							150,
						))
						.size(10.0),
				);
			});

			ui.add_space(6.0);
			ui.separator();
			ui.add_space(4.0);

			ScrollArea::vertical()
				.id_source(format!("universe_{}", title))
				.auto_shrink([false, false])
				.show(ui, |ui| {
					if content.is_empty() {
						ui.vertical_centered(|ui| {
							ui.add_space(20.0);
							if is_generating {
								ui.label(
									RichText::new("⟳ tuning in...")
										.color(accent_color)
										.size(13.0),
								);
							} else {
								ui.label(
									RichText::new(
										"Awaiting transmission...",
									)
									.color(Color32::from_gray(70))
									.size(11.0)
									.italics(),
								);
							}
						});
					} else if is_sales_coach {
						render_sales_coach(ui, content, accent_color);
					} else {
						render_universe_content(ui, content, accent_color);
					}
				});
		});
}

/// Enhanced renderer for the Φ sales coach column
fn render_sales_coach(ui: &mut Ui, content: &str, accent: Color32) {
	let step_color = Color32::from_rgb(255, 200, 80);
	let say_color = Color32::from_rgb(100, 230, 170);
	let technique_color = Color32::from_rgb(180, 140, 255);
	let objection_color = Color32::from_rgb(255, 140, 80);
	let next_move_color = Color32::from_rgb(255, 240, 120);

	for line in content.lines() {
		let line = line.trim();
		if line.is_empty() {
			ui.add_space(3.0);
			continue;
		}

		// Section header **TITLE**
		if line.starts_with("**") && line.ends_with("**") && line.len() > 4 {
			let inner = &line[2..line.len() - 2];
			ui.add_space(4.0);
			ui.label(RichText::new(inner).color(accent).size(12.0).strong());
			ui.add_space(2.0);
			continue;
		}

		// Numbered objection stack items  "1. ..." "2. ..." etc.
		if line.len() > 2 && line.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
			if let Some(dot_pos) = line.find(". ") {
				let num = &line[..dot_pos];
				let text = &line[dot_pos + 2..];
				ui.horizontal_wrapped(|ui| {
					ui.label(
						RichText::new(format!("{}.  ", num))
							.color(objection_color)
							.size(12.0)
							.strong(),
					);
					render_inline(ui, text, 12.0, Color32::from_rgb(230, 200, 180));
				});
				ui.add_space(2.0);
				continue;
			}
		}

		// Step N lines
		if line.starts_with("Step ") {
			ui.add_space(5.0);
			egui::Frame::none()
				.fill(Color32::from_rgba_unmultiplied(60, 40, 10, 120))
				.rounding(Rounding::same(5.0))
				.inner_margin(Margin::symmetric(8.0, 4.0))
				.show(ui, |ui| {
					ui.label(
						RichText::new(line).color(step_color).size(12.0).strong(),
					);
				});
			ui.add_space(2.0);
			continue;
		}

		// "→ Say: ..." lines — highlight the actual words to say
		if let Some(rest) = line.strip_prefix("→ Say:") {
			ui.horizontal_wrapped(|ui| {
				ui.label(RichText::new("→ Say:").color(say_color).size(11.5).strong());
				render_inline(ui, rest.trim(), 12.0, say_color);
			});
			ui.add_space(2.0);
			continue;
		}

		// "→ Technique: ..." lines
		if let Some(rest) = line.strip_prefix("→ Technique:") {
			ui.horizontal_wrapped(|ui| {
				ui.label(
					RichText::new("→ Technique:")
						.color(technique_color)
						.size(11.0),
				);
				render_inline(ui, rest.trim(), 11.0, Color32::from_gray(190));
			});
			continue;
		}

		// "→ ..." generic arrows
		if line.starts_with("→ ") {
			ui.horizontal_wrapped(|ui| {
				ui.label(RichText::new("→").color(accent).size(12.0));
				render_inline(ui, &line[2..], 12.0, Color32::from_gray(200));
			});
			continue;
		}

		// **NEXT BEST MOVE** content (next line after the header)
		if line.starts_with("**") && line.ends_with("**") {
			let inner = &line[2..line.len() - 2];
			ui.label(RichText::new(inner).color(next_move_color).size(13.0).strong());
			continue;
		}

		// Bullet points
		if line.starts_with("- ") || line.starts_with("• ") {
			ui.horizontal_wrapped(|ui| {
				ui.label(RichText::new("•").color(accent).size(12.0));
				render_inline(ui, &line[2..], 12.0, Color32::from_gray(205));
			});
			continue;
		}

		// Default paragraph
		render_inline(ui, line, 12.0, Color32::from_gray(200));
		ui.add_space(1.0);
	}
}

/// Generic universe content renderer (Ω and Δ columns)
fn render_universe_content(ui: &mut Ui, content: &str, accent: Color32) {
	for line in content.lines() {
		let line = line.trim();
		if line.is_empty() {
			ui.add_space(3.0);
			continue;
		}
		if line.starts_with("**") && line.ends_with("**") && !line[2..].contains("**") {
			let inner = &line[2..line.len() - 2];
			ui.label(RichText::new(inner).color(accent).size(13.0).strong());
			ui.add_space(2.0);
			continue;
		}
		if line.starts_with("- ") || line.starts_with("• ") {
			ui.horizontal_wrapped(|ui| {
				ui.label(RichText::new("•").color(accent).size(12.0));
				render_inline(ui, &line[2..], 12.0, Color32::from_gray(205));
			});
			ui.add_space(2.0);
			continue;
		}
		render_inline(ui, line, 12.5, Color32::from_rgb(210, 210, 225));
		ui.add_space(1.0);
	}
}

/// Inline renderer with **bold** and *italic* support
fn render_inline(ui: &mut Ui, text: &str, size: f32, base_color: Color32) {
	let mut job = LayoutJob::default();
	let mut chars = text.char_indices().peekable();
	let mut current = String::new();
	let mut bold = false;
	let mut italic = false;

	while let Some((_i, ch)) = chars.next() {
		if ch == '*' {
			if chars.peek().map(|&(_, c)| c) == Some('*') {
				chars.next();
				if !current.is_empty() {
					job.append(
						&current,
						0.0,
						TextFormat {
							font_id: FontId::new(size, FontFamily::Proportional),
							color: base_color,
							italics: italic,
							..Default::default()
						},
					);
					current.clear();
				}
				bold = !bold;
			} else {
				if !current.is_empty() {
					job.append(
						&current,
						0.0,
						TextFormat {
							font_id: FontId::new(size, FontFamily::Proportional),
							color: base_color,
							italics: italic,
							..Default::default()
						},
					);
					current.clear();
				}
				italic = !italic;
			}
		} else {
			current.push(ch);
		}
	}

	if !current.is_empty() {
		job.append(
			&current,
			0.0,
			TextFormat {
				font_id: FontId::new(size, FontFamily::Proportional),
				color: base_color,
				italics: italic,
				..Default::default()
			},
		);
	}

	job.wrap.max_width = ui.available_width();
	let galley = ui.fonts(|f| f.layout_job(job));
	ui.label(galley);
}

// ─────────────────────────────────────────────
// eframe App
// ─────────────────────────────────────────────
impl eframe::App for App {
	fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
		// ── Auto-trigger: universe generation ───────────────────────────
		{
			let word_count =
				TRANSCRIPT_FINAL.lock().unwrap().iter().filter(|w| w.is_some()).count();
			let last = *UNIVERSE_WORD_COUNT.lock().unwrap();
			let generating = *UNIVERSE_GENERATING.lock().unwrap();
			if word_count > last + 7 && !generating {
				*UNIVERSE_WORD_COUNT.lock().unwrap() = word_count;
				let t = self.get_transcript();
				self.trigger_universes(t, ctx.clone());
			}
		}

		// ── Auto-fill context when we have ≥20 words and not done yet ───
		{
			let word_count =
				TRANSCRIPT_FINAL.lock().unwrap().iter().filter(|w| w.is_some()).count();
			let done = *CTX_AUTOFILL_DONE.lock().unwrap();
			if word_count >= 20 && !done {
				let t = self.get_transcript();
				self.trigger_context_autofill(t, ctx.clone());
			}
		}

		// ── Left sidebar ─────────────────────────────────────────────────
		SidePanel::left("left_panel").min_width(200.0).show(ctx, |ui| {
			ui.label(
				RichText::new(option_env!("BUILD_ID").unwrap_or("DEV"))
					.color(Color32::from_gray(80))
					.size(9.0),
			);

			ui.add_space(6.0);

			// Record button
			if ui
				.add_enabled(
					!self.is_recording,
					Button::new(if self.is_recording {
						RichText::new("● LIVE").color(Color32::RED)
					} else {
						RichText::new("⏺  Record").color(Color32::from_rgb(80, 200, 120))
					}),
				)
				.clicked()
			{
				self.is_recording = true;
				self.sessions
					.push(session::Session { start_ms: now_ms(), end_ms: i64::MAX });
				tokio::spawn(async {
					let dg = Deepgram::new(env::var("DEEPGRAM_API_KEY").unwrap());
					let mut results = dg
						.transcription()
						.stream_request()
						.stream(microphone_as_stream())
						.encoding("linear16".to_string())
						.sample_rate(44100)
						.channels(1)
						.start()
						.await
						.unwrap();
					while let Some(result) = results.next().await {
						if let Ok(
							deepgram::transcription::live::StreamResponse::TranscriptResponse {
								duration,
								is_final,
								channel: Channel { mut alternatives, .. },
								..
							},
						) = result
						{
							if !is_final {
								*DURATION.lock().unwrap() += duration;
							}
							if let Some(deepgram::transcription::live::Alternatives {
								words,
								..
							}) = alternatives.first_mut()
							{
								for w in words.iter() {
									TRANSCRIPT.lock().unwrap().push(Some(w.clone()));
								}
								TRANSCRIPT.lock().unwrap().push(None);
								if is_final {
									for w in words {
										TRANSCRIPT_FINAL
											.lock()
											.unwrap()
											.push(Some(Word::Live(w.clone())));
									}
									TRANSCRIPT_FINAL.lock().unwrap().push(None);
								}
							}
						}
					}
				});
			}

			if ui.button("⟳ Universes Now").clicked() {
				let t = self.get_transcript();
				self.trigger_universes(t, ctx.clone());
			}

			if ui.button("↺ Re-fill Context").clicked() {
				*CTX_AUTOFILL_DONE.lock().unwrap() = false;
			}

			if ui.button("📁 Import audio").clicked() {
				tokio::spawn(async {
					let file =
						rfd::AsyncFileDialog::new().pick_file().await.unwrap().read().await;
					let source = AudioSource::from_buffer_with_mime_type(file, "audio/aac");
					let opts = Options::builder()
						.punctuate(false)
						.diarize(true)
						.model(deepgram::transcription::prerecorded::options::Model::CustomId(
							"nova-2-meeting".to_string(),
						))
						.build();
					let dg = Deepgram::new(env::var("DEEPGRAM_API_KEY").expect("DEEPGRAM_API_KEY"));
					let mut resp = dg.transcription().prerecorded(source, &opts).await.unwrap();
					TRANSCRIPT_FINAL.lock().unwrap().extend(
						resp.results.channels[0].alternatives[0]
							.words
							.drain(..)
							.map(|w| Some(Word::Prerecorded(w))),
					);
				});
			}

			if ui.button("📋 Copy transcript").clicked() {
				ui.output_mut(|o| o.copied_text = self.get_transcript());
			}

			ui.add_space(6.0);
			ui.separator();
			ui.label(RichText::new("SESSIONS").size(10.0).color(Color32::from_gray(120)));
			ui.add_space(2.0);
			for (i, session) in self.sessions.iter().enumerate() {
				if ui
					.small_button(format!(
						"#{} ({:.0}s)",
						i,
						session.duration_ms() as f32 / 1000.0
					))
					.clicked()
				{
					let samples = session.samples();
					tokio::spawn(async move {
						TRANSCRIPT_FINAL.lock().unwrap().clear();
						audiofile::save_wav_file(samples);
						let dg = Deepgram::new(
							env::var("DEEPGRAM_API_KEY").expect("DEEPGRAM_API_KEY"),
						);
						let file = File::open("temp_audio.aac").await.unwrap();
						let source = AudioSource::from_buffer_with_mime_type(file, "audio/aac");
						let opts = Options::builder()
							.punctuate(false)
							.diarize(true)
							.model(
								deepgram::transcription::prerecorded::options::Model::CustomId(
									"nova-2-meeting".to_string(),
								),
							)
							.build();
						let resp: deepgram::transcription::prerecorded::response::Response = dg
							.transcription()
							.make_prerecorded_request_builder(source, &opts)
							.query(&[("filler_words", "true")])
							.send()
							.await
							.unwrap()
							.json()
							.await
							.unwrap();
						for w in resp.results.channels[0].alternatives[0].words.iter() {
							TRANSCRIPT_FINAL
								.lock()
								.unwrap()
								.push(Some(Word::Prerecorded(w.clone())));
						}
					});
				}
			}

			ui.add_space(6.0);
			ui.separator();
			ui.label(RichText::new("SPEAKERS").size(10.0).color(Color32::from_gray(120)));
			while self.speaker_names.len() < 6 {
				self.speaker_names.push(self.speaker_names.len().to_string());
			}
			for name in self.speaker_names.iter_mut().take(6) {
				ui.add(TextEdit::singleline(name).desired_width(f32::INFINITY));
			}

			ui.add_space(6.0);
			ui.separator();
			ui.label(
				RichText::new("LIVE WORDS").size(10.0).color(Color32::from_gray(120)),
			);
			ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
				let words = TRANSCRIPT.lock().unwrap();
				let cols = [
					Color32::from_rgb(100, 220, 180),
					Color32::from_rgb(180, 140, 255),
					Color32::from_rgb(255, 200, 80),
					Color32::from_rgb(255, 120, 120),
				];
				for line in words.split(|w| w.is_none()) {
					ui.horizontal_wrapped(|ui| {
						for w in line {
							if let Some(w) = w {
								ui.label(
									RichText::new(w.word.clone())
										.color(cols[w.speaker as usize % 4])
										.size(11.0),
								);
							}
						}
					});
				}
			});
		});

		// ── Central panel ────────────────────────────────────────────────
		CentralPanel::default()
			.frame(
				egui::Frame::none()
					.fill(Color32::from_rgb(8, 8, 14))
					.inner_margin(Margin::same(10.0)),
			)
			.show(ctx, |ui| {
				// ── Context panel (collapsible) ───────────────────────
				let ctx_panel_open = &mut self.context_panel_open;
				egui::CollapsingHeader::new(
					RichText::new("⟐  RELATIONSHIP CONTEXT  —  intention · outcome · roles · objection framing")
						.color(Color32::from_rgb(200, 160, 60))
						.size(11.0),
				)
				.default_open(true)
				.show(ui, |ui| {
					egui::Frame::none()
						.fill(Color32::from_rgb(22, 18, 8))
						.rounding(Rounding::same(8.0))
						.inner_margin(Margin::same(10.0))
						.show(ui, |ui| {
							let gold = Color32::from_rgb(255, 190, 40);
							let dim = Color32::from_gray(120);
							let field_size = 11.5;

							ui.horizontal(|ui| {
								// Intention
								ui.vertical(|ui| {
									ui.set_min_width(ui.available_width() / 2.0 - 8.0);
									ui.label(
										RichText::new("INTENTION").color(gold).size(10.0).strong(),
									);
									let mut v = CTX_INTENTION.lock().unwrap().clone();
									if ui
										.add(
											TextEdit::multiline(&mut v)
												.desired_width(f32::INFINITY)
												.desired_rows(2)
												.hint_text("What's the purpose of this conversation?")
												.font(FontId::new(field_size, FontFamily::Proportional)),
										)
										.changed()
									{
										*CTX_INTENTION.lock().unwrap() = v;
									}
								});

								ui.add_space(8.0);

								// Outcome
								ui.vertical(|ui| {
									ui.set_min_width(ui.available_width() - 4.0);
									ui.label(
										RichText::new("DESIRED OUTCOME").color(gold).size(10.0).strong(),
									);
									let mut v = CTX_OUTCOME.lock().unwrap().clone();
									if ui
										.add(
											TextEdit::multiline(&mut v)
												.desired_width(f32::INFINITY)
												.desired_rows(2)
												.hint_text("What's the ideal result for you?")
												.font(FontId::new(field_size, FontFamily::Proportional)),
										)
										.changed()
									{
										*CTX_OUTCOME.lock().unwrap() = v;
									}
								});
							});

							ui.add_space(4.0);

							ui.horizontal(|ui| {
								// Their role
								ui.vertical(|ui| {
									ui.set_min_width(ui.available_width() / 2.0 - 8.0);
									ui.label(
										RichText::new("THEY ARE TO ME").color(gold).size(10.0).strong(),
									);
									let mut v = CTX_THEIR_ROLE.lock().unwrap().clone();
									if ui
										.add(
											TextEdit::singleline(&mut v)
												.desired_width(f32::INFINITY)
												.hint_text("e.g. 'potential client / decision maker'")
												.font(FontId::new(field_size, FontFamily::Proportional)),
										)
										.changed()
									{
										*CTX_THEIR_ROLE.lock().unwrap() = v;
									}
								});

								ui.add_space(8.0);

								// My role
								ui.vertical(|ui| {
									ui.set_min_width(ui.available_width() - 4.0);
									ui.label(
										RichText::new("I AM TO THEM").color(gold).size(10.0).strong(),
									);
									let mut v = CTX_MY_ROLE.lock().unwrap().clone();
									if ui
										.add(
											TextEdit::singleline(&mut v)
												.desired_width(f32::INFINITY)
												.hint_text("e.g. 'strategic consultant / coach'")
												.font(FontId::new(field_size, FontFamily::Proportional)),
										)
										.changed()
									{
										*CTX_MY_ROLE.lock().unwrap() = v;
									}
								});
							});

							ui.add_space(4.0);
							ui.horizontal(|ui| {
								let autofill_done = *CTX_AUTOFILL_DONE.lock().unwrap();
								if ui
									.small_button(
										RichText::new(if autofill_done {
											"↺ Re-analyze from transcript"
										} else {
											"✦ Auto-fill from transcript"
										})
										.color(gold),
									)
									.clicked()
								{
									*CTX_AUTOFILL_DONE.lock().unwrap() = false;
									let t = self.get_transcript();
									self.trigger_context_autofill(t, ctx.clone());
								}

								if !autofill_done {
									ui.label(
										RichText::new("(auto-fills after 20 words)")
											.color(dim)
											.size(10.0)
											.italics(),
									);
								}
							});
						});
				});

				ui.add_space(8.0);

				// ── Transcript strip ──────────────────────────────────
				egui::Frame::none()
					.fill(Color32::from_rgb(16, 16, 28))
					.rounding(Rounding::same(8.0))
					.inner_margin(Margin::same(10.0))
					.show(ui, |ui| {
						ui.set_max_height(80.0);
						ui.horizontal(|ui| {
							ui.label(
								RichText::new("LIVE")
									.color(Color32::from_gray(100))
									.size(9.0)
									.strong(),
							);
							let gen = *UNIVERSE_GENERATING.lock().unwrap();
							if gen {
								ui.label(
									RichText::new("  ◉ universes updating...")
										.color(Color32::from_rgb(100, 180, 255))
										.size(9.0),
								);
							}
						});
						let spk_cols = [
							Color32::from_rgb(100, 220, 180),
							Color32::from_rgb(180, 140, 255),
							Color32::from_rgb(255, 200, 80),
							Color32::from_rgb(255, 120, 120),
						];
						ScrollArea::vertical()
							.id_source("live_strip")
							.stick_to_bottom(true)
							.auto_shrink([false, false])
							.max_height(56.0)
							.show(ui, |ui| {
								let words = TRANSCRIPT_FINAL.lock().unwrap();
								for line in words.split(|w| w.is_none()) {
									let mut cur = 100u8;
									ui.horizontal_wrapped(|ui| {
										for w in line {
											if let Some(w) = w {
												if w.speaker() != cur {
													cur = w.speaker();
													let name = self
														.speaker_names
														.get(cur as usize)
														.cloned()
														.unwrap_or_else(|| cur.to_string());
													ui.label(
														RichText::new(format!("[{}] ", name))
															.color(spk_cols[cur as usize % 4])
															.size(12.0)
															.strong(),
													);
												}
												ui.label(
													RichText::new(w.word())
														.size(12.0)
														.color(Color32::from_gray(195)),
												);
											}
										}
									});
								}
							});
					});

				ui.add_space(8.0);

				// ── Column title ──────────────────────────────────────
				ui.vertical_centered(|ui| {
					ui.label(
						RichText::new("BRANCHING UNIVERSES")
							.color(Color32::from_gray(130))
							.size(10.0)
							.strong(),
					);
				});

				ui.add_space(6.0);

				// ── 3 columns ─────────────────────────────────────────
				let presence = UNIVERSE_PRESENCE.lock().unwrap().clone();
				let power = UNIVERSE_POWER.lock().unwrap().clone();
				let possibility = UNIVERSE_POSSIBILITY.lock().unwrap().clone();
				let generating = *UNIVERSE_GENERATING.lock().unwrap();

				let avail = ui.available_size();
				let col_w = (avail.x - 20.0) / 3.0;

				ui.horizontal(|ui| {
					ui.set_min_height(avail.y - 4.0);

					// Ω Presence (teal)
					ui.allocate_ui(vec2(col_w, ui.available_height()), |ui| {
						universe_column(
							ui,
							"Ω",
							"PRESENCE",
							"Empathic Witness · Somatic Depth",
							Color32::from_rgb(40, 210, 180),
							Color32::from_rgb(100, 240, 210),
							&presence,
							generating,
							false,
						);
					});

					ui.add_space(10.0);

					// Δ Power (violet)
					ui.allocate_ui(vec2(col_w, ui.available_height()), |ui| {
						universe_column(
							ui,
							"Δ",
							"POWER",
							"Sovereign Self · Embedded Commands",
							Color32::from_rgb(160, 80, 255),
							Color32::from_rgb(200, 140, 255),
							&power,
							generating,
							false,
						);
					});

					ui.add_space(10.0);

					// Φ Sales Coach (gold)
					ui.allocate_ui(vec2(col_w, ui.available_height()), |ui| {
						universe_column(
							ui,
							"Φ",
							"SALES COACH",
							"Objection Stack · Live Intelligence",
							Color32::from_rgb(255, 190, 40),
							Color32::from_rgb(255, 220, 100),
							&possibility,
							generating,
							true,
						);
					});
				});
			});
	}
}

// ─────────────────────────────────────────────
// Claude API — streaming
// ─────────────────────────────────────────────
pub async fn run_claude_stream(
	model: &str,
	system: &str,
	user_content: String,
	callback: impl Fn(&str) + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	use futures::StreamExt;

	let api_key = std::env::var("ANTHROPIC_API_KEY")
		.map_err(|_| "ANTHROPIC_API_KEY environment variable not set")?;

	let body = serde_json::json!({
		"model": model,
		"max_tokens": 1024,
		"stream": true,
		"system": system,
		"messages": [{"role": "user", "content": user_content}]
	});

	let client = reqwest::Client::new();
	let resp = client
		.post("https://api.anthropic.com/v1/messages")
		.header("x-api-key", api_key)
		.header("anthropic-version", "2023-06-01")
		.header("content-type", "application/json")
		.json(&body)
		.send()
		.await
		.map_err(|e| format!("HTTP error: {e}"))?;

	if !resp.status().is_success() {
		let status = resp.status();
		let body_text = resp.text().await.unwrap_or_default();
		return Err(format!("Anthropic API {status}: {body_text}").into());
	}

	let mut stream = resp.bytes_stream();
	let mut line_buf = String::new();

	while let Some(chunk) = stream.next().await {
		let chunk = chunk.map_err(|e| format!("stream read error: {e}"))?;
		line_buf.push_str(&String::from_utf8_lossy(&chunk));

		loop {
			if let Some(pos) = line_buf.find('\n') {
				let line = line_buf[..pos].trim_end_matches('\r').to_string();
				line_buf.drain(..=pos);

				if let Some(json_str) = line.strip_prefix("data: ") {
					if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
						if v["type"] == "content_block_delta" {
							if let Some(text) = v["delta"]["text"].as_str() {
								callback(text);
							}
						}
					}
				}
			} else {
				break;
			}
		}
	}

	Ok(())
}

// ─────────────────────────────────────────────
// Claude API — single non-streaming call
// ─────────────────────────────────────────────
pub async fn run_claude_once(
	model: &str,
	system: &str,
	user_content: String,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
	let api_key = std::env::var("ANTHROPIC_API_KEY")
		.map_err(|_| "ANTHROPIC_API_KEY environment variable not set")?;

	let body = serde_json::json!({
		"model": model,
		"max_tokens": 512,
		"stream": false,
		"system": system,
		"messages": [{"role": "user", "content": user_content}]
	});

	let client = reqwest::Client::new();
	let resp = client
		.post("https://api.anthropic.com/v1/messages")
		.header("x-api-key", api_key)
		.header("anthropic-version", "2023-06-01")
		.header("content-type", "application/json")
		.json(&body)
		.send()
		.await?;

	let json: serde_json::Value = resp.json().await?;
	let text = json["content"][0]["text"].as_str().unwrap_or("").to_string();
	Ok(text)
}

// ─────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────
#[tokio::main]
async fn main() -> eframe::Result<()> {
	env_logger::init();
	eprintln!("database at {:?}", turbosql::db_path());
	self_update::self_update().await.ok();

	eframe::run_native(
		"grace — branching universes",
		eframe::NativeOptions {
			viewport: egui::ViewportBuilder::default()
				.with_inner_size([1440.0, 960.0])
				.with_min_inner_size([900.0, 600.0]),
			..Default::default()
		},
		Box::new(|cc| Box::new(App::new(cc))),
	)
}

// ─────────────────────────────────────────────
// Microphone → async stream
// ─────────────────────────────────────────────
fn microphone_as_stream() -> Receiver<Result<Bytes, RecvError>> {
	let (sync_tx, sync_rx) = crossbeam::channel::unbounded();
	let (mut async_tx, async_rx) = mpsc::channel(1);

	thread::spawn(move || {
		let host = cpal::default_host();
		let device = host.default_input_device().unwrap();
		let config = device.default_input_config().unwrap();
		let num_channels = config.channels() as usize;

		let stream = match config.sample_format() {
			cpal::SampleFormat::F32 => device
				.build_input_stream(
					&config.into(),
					move |data: &[f32], _: &_| {
						let mut bytes = BytesMut::with_capacity(
							(data.len() * std::mem::size_of::<i16>()) / num_channels,
						);
						for sample in data.iter().step_by(num_channels) {
							bytes.put_i16_le(sample.to_sample::<i16>());
						}
						sync_tx.send(bytes.freeze()).ok();
					},
					|_| panic!("audio stream error"),
					None,
				)
				.unwrap(),
			_ => panic!("unsupported sample format"),
		};

		stream.play().unwrap();
		loop {
			thread::park();
		}
	});

	tokio::spawn(async move {
		let mut buf = Vec::with_capacity(150_000);
		loop {
			let data = sync_rx.recv();
			if let Ok(ref d) = data {
				buf.extend(d.clone().to_vec());
				if buf.len() > 100_000 {
					let moved = buf;
					buf = Vec::with_capacity(150_000);
					tokio::task::spawn_blocking(|| {
						SampleData {
							rowid: None,
							record_ms: now_ms(),
							sample_data: moved,
						}
						.insert()
						.unwrap();
					});
				}
			}
			async_tx.send(data).await.unwrap();
		}
	});

	async_rx
}

// ─────────────────────────────────────────────
// Syntax highlighting (kept for ehttp panel)
// ─────────────────────────────────────────────
struct ColoredText(text::LayoutJob);

impl ColoredText {
	pub fn ui(&self, ui: &mut Ui) {
		let mut layouter = |ui: &Ui, _string: &str, wrap_width: f32| {
			let mut job = self.0.clone();
			job.wrap.max_width = wrap_width;
			ui.fonts(|f| f.layout_job(job))
		};
		let mut text = self.0.text.as_str();
		ui.add(
			TextEdit::multiline(&mut text)
				.font(TextStyle::Monospace)
				.desired_width(f32::INFINITY)
				.layouter(&mut layouter),
		);
	}
}

fn selectable_text(ui: &mut Ui, mut text: &str) {
	ui.add(TextEdit::multiline(&mut text).desired_width(f32::INFINITY).font(TextStyle::Monospace));
}
