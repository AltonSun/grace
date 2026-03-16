#![forbid(unsafe_code)]
#![allow(unused_imports, dead_code)]

use async_openai::types::ChatCompletionRequestMessage;
use async_openai::types::Role::{self, *};
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
use stream_cancel::{StreamExt as _, Trigger, Tripwire};
use tokio::fs::File;
use turbosql::{execute, now_ms, select, update, Blob, Turbosql};

mod audiofile;
mod self_update;
mod session;

// ─────────────────────────────────────────────
// Word wrapper for live vs prerecorded
// ─────────────────────────────────────────────
enum Word {
	Live(LiveWord),
	Prerecorded(PrerecordedWord),
}

impl Word {
	fn speaker(&self) -> u8 {
		match self {
			Word::Live(word) => word.speaker,
			Word::Prerecorded(word) => word.speaker.unwrap() as u8,
		}
	}
	fn word(&self) -> &str {
		match self {
			Word::Live(word) => &word.word,
			Word::Prerecorded(word) => &word.word,
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
	Mutex::new(String::from(
		"For the personal conversational transcript above, here is a coaching prompt:",
	))
});

/// Branching Universe content — one per column
static UNIVERSE_PRESENCE: Lazy<Mutex<String>> = Lazy::new(Default::default);
static UNIVERSE_POWER: Lazy<Mutex<String>> = Lazy::new(Default::default);
static UNIVERSE_POSSIBILITY: Lazy<Mutex<String>> = Lazy::new(Default::default);

/// How many words (final) have been processed into universes already
static UNIVERSE_WORD_COUNT: Lazy<Mutex<usize>> = Lazy::new(Default::default);

/// Whether universe generation is currently running
static UNIVERSE_GENERATING: Lazy<Mutex<bool>> = Lazy::new(Default::default);

// ─────────────────────────────────────────────
// Chat / wheel window types
// ─────────────────────────────────────────────
#[derive(Clone)]
struct ChatMessage {
	role: Role,
	content: String,
}

struct WheelWindow(Vec<ChatMessage>);

impl Default for WheelWindow {
	fn default() -> Self {
		Self(vec![ChatMessage { role: User, content: String::new() }])
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
// HTTP resource (for ehttp fetch panel)
// ─────────────────────────────────────────────
struct Resource {
	response: ehttp::Response,
	text: Option<String>,
	image: Option<Image<'static>>,
	colored_text: Option<ColoredText>,
}

impl Resource {
	fn from_response(ctx: &Context, response: ehttp::Response) -> Self {
		let content_type = response.content_type().unwrap_or_default();
		if content_type.starts_with("image/") {
			ctx.include_bytes(response.url.clone(), response.bytes.clone());
			let image = Image::from_uri(response.url.clone());
			Self { response, text: None, colored_text: None, image: Some(image) }
		} else {
			let text = response.text();
			let colored_text = text.and_then(|text| syntax_highlighting(ctx, &response, text));
			let text = text.map(|text| text.to_owned());
			Self { response, text, colored_text, image: None }
		}
	}
}

// ─────────────────────────────────────────────
// NLP Universe system prompts
// ─────────────────────────────────────────────
const PROMPT_PRESENCE: &str = r#"You are Ω — a consciousness guide specialising in deep empathic presence, somatic awareness, and psychoacoustic resonance. You embody Milton Model hypnotic language: artfully vague, permissive, and deeply attuned.

When given a live conversation transcript, generate a real-time coaching insight in exactly this structure (no extra headers, keep each section concise):

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

const PROMPT_POSSIBILITY: &str = r#"You are Φ — a quantum possibility architect and visionary language artist who opens doorways to entirely new realities through metaphor, future-pacing, and generative questions.

When given a live conversation transcript, generate a real-time coaching insight in exactly this structure:

**QUANTUM SIGNAL** (1-2 sentences): What universe is being co-created by this conversation? Describe the energetic pattern and its highest potential trajectory.

**POSSIBILITY PORTALS** (2-3 bullet points): Generative questions or reframes that open entirely new reality tunnels. Use "What becomes possible when...", "Imagine the version of you who...", "In the universe where this resolves perfectly..."

**FUTURE ECHO** (1 sentence, italicised): Speak from the version of reality where this has already unfolded beautifully. Begin with "From here, you can already feel..."

Keep total response under 160 words. Be expansive, poetic, and electric with possibility."#;

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

	#[serde(skip)]
	debounce_tx: Option<Sender<String>>,
	#[serde(skip)]
	gpt_3_trigger: Option<Trigger>,
	#[serde(skip)]
	trigger: Option<Trigger>,
	#[serde(skip)]
	sessions: Vec<session::Session>,
	#[serde(skip)]
	is_recording: bool,
	#[serde(skip)]
	promise: Option<Promise<ehttp::Result<Resource>>>,
}

impl App {
	fn get_transcript(&self) -> String {
		let mut transcript = String::new();
		let words = TRANSCRIPT_FINAL.lock().unwrap();
		let lines = words.split(|word| word.is_none());
		for line in lines {
			let mut current_speaker = 100u8;
			for word in line {
				if let Some(word) = word {
					if word.speaker() != current_speaker {
						current_speaker = word.speaker();
						transcript.push_str(&format!(
							"\n[{}]: ",
							self.speaker_names.get(current_speaker as usize).cloned().unwrap_or_else(|| current_speaker.to_string())
						));
					}
					transcript.push_str(&format!("{} ", word.word()));
				}
			}
		}
		transcript.push('\n');
		transcript
	}

	pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
		// Dark, moody base style
		cc.egui_ctx.style_mut(|s| {
			s.visuals.override_text_color = Some(Color32::from_rgb(220, 220, 230));
			s.visuals.window_fill = Color32::from_rgb(12, 12, 18);
			s.visuals.panel_fill = Color32::from_rgb(8, 8, 14);
		});

		egui_extras::install_image_loaders(&cc.egui_ctx);

		let (debounce_tx, mut debounce_rx) = mpsc::channel(10);

		let s = Self {
			debounce_tx: Some(debounce_tx),
			sessions: session::Session::calculate_sessions(),
			completion_prompt: COMPLETION_PROMPT.lock().unwrap().clone(),
			..Default::default()
		};

		// Repaint loop
		let ctx_repaint = cc.egui_ctx.clone();
		tokio::spawn(async move {
			let mut interval = tokio::time::interval(Duration::from_millis(100));
			loop {
				interval.tick().await;
				ctx_repaint.request_repaint();
			}
		});

		// Completion debounce task
		let ctx = cc.egui_ctx.clone();
		tokio::spawn(async move {
			let duration = Duration::from_millis(300);
			let mut keys_pressed = false;
			let mut string = String::new();
			let mut _trigger = None;

			loop {
				match tokio::time::timeout(duration, debounce_rx.next()).await {
					Ok(Some(s)) => {
						_trigger = None;
						COMPLETION.lock().unwrap().clear();
						string = s;
						keys_pressed = true;
					}
					Ok(None) => break,
					Err(_) => {
						if keys_pressed && !string.is_empty() {
							let (t, tripwire) = Tripwire::new();
							_trigger = Some(t);
							let prompt_string =
								format!("{} {}", COMPLETION_PROMPT.lock().unwrap(), string);
							let ctx2 = ctx.clone();
							tokio::spawn(async move {
								COMPLETION.lock().unwrap().clear();
								run_openai_completion(tripwire, prompt_string, move |content| {
									COMPLETION.lock().unwrap().push_str(&content);
									ctx2.request_repaint();
								})
								.await
								.ok();
							});
							keys_pressed = false;
						}
					}
				}
			}
		});

		s
	}

	/// Fire 3 parallel GPT requests for the branching universes
	fn trigger_universes(&self, transcript: String, ctx: Context) {
		if transcript.trim().is_empty() {
			return;
		}
		*UNIVERSE_GENERATING.lock().unwrap() = true;
		UNIVERSE_PRESENCE.lock().unwrap().clear();
		UNIVERSE_POWER.lock().unwrap().clear();
		UNIVERSE_POSSIBILITY.lock().unwrap().clear();

		let transcript_a = transcript.clone();
		let transcript_b = transcript.clone();
		let transcript_c = transcript.clone();
		let ctx_a = ctx.clone();
		let ctx_b = ctx.clone();
		let ctx_c = ctx.clone();

		// Ω — Presence
		tokio::spawn(async move {
			let messages = vec![
				ChatMessage { role: System, content: PROMPT_PRESENCE.to_string() },
				ChatMessage { role: User, content: transcript_a },
			];
			let (_, tripwire) = Tripwire::new();
			run_openai(GPT_4, tripwire, messages, String::new(), move |chunk| {
				UNIVERSE_PRESENCE.lock().unwrap().push_str(chunk);
				ctx_a.request_repaint();
			})
			.await
			.ok();
		});

		// Δ — Power
		tokio::spawn(async move {
			let messages = vec![
				ChatMessage { role: System, content: PROMPT_POWER.to_string() },
				ChatMessage { role: User, content: transcript_b },
			];
			let (_, tripwire) = Tripwire::new();
			run_openai(GPT_4, tripwire, messages, String::new(), move |chunk| {
				UNIVERSE_POWER.lock().unwrap().push_str(chunk);
				ctx_b.request_repaint();
			})
			.await
			.ok();
		});

		// Φ — Possibility
		tokio::spawn(async move {
			let messages = vec![
				ChatMessage { role: System, content: PROMPT_POSSIBILITY.to_string() },
				ChatMessage { role: User, content: transcript_c },
			];
			let (_, tripwire) = Tripwire::new();
			run_openai(GPT_4, tripwire, messages, String::new(), move |chunk| {
				UNIVERSE_POSSIBILITY.lock().unwrap().push_str(chunk);
				ctx_c.request_repaint();
			})
			.await
			.ok();
		});
	}
}

// ─────────────────────────────────────────────
// UI helpers
// ─────────────────────────────────────────────
trait MyThings {
	fn editable(&mut self, text: &mut dyn TextBuffer) -> bool;
}

impl MyThings for Ui {
	fn editable(&mut self, text: &mut dyn TextBuffer) -> bool {
		self
			.add(
				TextEdit::multiline(text)
					.desired_width(f32::INFINITY)
					.font(FontId::new(30.0, FontFamily::Proportional)),
			)
			.changed()
	}
}

/// Render a universe column with glowing header and markdown-ish content
fn universe_column(
	ui: &mut Ui,
	symbol: &str,
	title: &str,
	subtitle: &str,
	header_color: Color32,
	accent_color: Color32,
	content: &str,
	is_generating: bool,
) {
	let col_bg = Color32::from_rgba_unmultiplied(
		header_color.r() / 6,
		header_color.g() / 6,
		header_color.b() / 6,
		240,
	);

	// Column frame
	egui::Frame::none()
		.fill(col_bg)
		.rounding(Rounding::same(10.0))
		.inner_margin(Margin::same(14.0))
		.show(ui, |ui| {
			ui.set_min_height(ui.available_height());

			// Header symbol
			ui.vertical_centered(|ui| {
				ui.label(
					RichText::new(symbol)
						.color(header_color)
						.size(36.0)
						.strong(),
				);
				ui.label(
					RichText::new(title)
						.color(header_color)
						.size(16.0)
						.strong(),
				);
				ui.label(
					RichText::new(subtitle)
						.color(Color32::from_rgba_unmultiplied(
							header_color.r(),
							header_color.g(),
							header_color.b(),
							160,
						))
						.size(11.0),
				);
			});

			ui.add_space(8.0);
			ui.separator();
			ui.add_space(6.0);

			ScrollArea::vertical()
				.id_source(format!("universe_{}", title))
				.auto_shrink([false, false])
				.stick_to_bottom(false)
				.show(ui, |ui| {
					if content.is_empty() && is_generating {
						// Pulsing dots while generating
						ui.vertical_centered(|ui| {
							ui.add_space(20.0);
							ui.label(RichText::new("⟳ tuning in...").color(accent_color).size(13.0));
						});
					} else if content.is_empty() {
						ui.vertical_centered(|ui| {
							ui.add_space(20.0);
							ui.label(
								RichText::new("Awaiting transmission...")
									.color(Color32::from_gray(80))
									.size(12.0)
									.italics(),
							);
						});
					} else {
						// Render content with simple formatting
						render_universe_content(ui, content, accent_color);
					}
				});
		});
}

/// Light markdown renderer: **bold** → strong, *italic* → italics, bullets
fn render_universe_content(ui: &mut Ui, content: &str, accent: Color32) {
	for line in content.lines() {
		let line = line.trim();
		if line.is_empty() {
			ui.add_space(4.0);
			continue;
		}

		// Section headers: **TITLE** lines
		if line.starts_with("**") && line.ends_with("**") && !line[2..].contains("**") {
			let inner = &line[2..line.len() - 2];
			ui.label(RichText::new(inner).color(accent).size(13.0).strong());
			ui.add_space(2.0);
			continue;
		}

		// Bullet points
		if line.starts_with("- ") || line.starts_with("• ") {
			let text = &line[2..];
			ui.horizontal_wrapped(|ui| {
				ui.label(RichText::new("•").color(accent).size(13.0));
				render_inline(ui, text, 12.0);
			});
			ui.add_space(2.0);
			continue;
		}

		// Regular paragraph
		render_inline(ui, line, 12.5);
		ui.add_space(1.0);
	}
}

/// Inline renderer: handles **bold** and *italic* spans
fn render_inline(ui: &mut Ui, text: &str, size: f32) {
	// Simple approach: split on ** and * markers to apply formatting
	// For a full parser this would be more complex; this handles the common cases
	let mut job = LayoutJob::default();
	let mut chars = text.char_indices().peekable();
	let mut current = String::new();
	let mut bold = false;
	let mut italic = false;

	let base_color = Color32::from_rgb(210, 210, 225);

	while let Some((_i, ch)) = chars.next() {
		if ch == '*' {
			if chars.peek().map(|&(_, c)| c) == Some('*') {
				// double star — flush, toggle bold
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
				// single star — flush, toggle italic
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
// eframe App impl
// ─────────────────────────────────────────────
impl eframe::App for App {
	fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
		// ── Check if we should trigger universe generation ──────────────
		{
			let current_word_count = TRANSCRIPT_FINAL
				.lock()
				.unwrap()
				.iter()
				.filter(|w| w.is_some())
				.count();
			let last_count = *UNIVERSE_WORD_COUNT.lock().unwrap();
			let generating = *UNIVERSE_GENERATING.lock().unwrap();

			// Trigger every 8 new words, and not while already generating
			if current_word_count > last_count + 7 && !generating {
				*UNIVERSE_WORD_COUNT.lock().unwrap() = current_word_count;
				let transcript = self.get_transcript();
				self.trigger_universes(transcript, ctx.clone());
			}
		}

		// ── Left sidebar ─────────────────────────────────────────────────
		let _do_all = false;

		SidePanel::left("left_panel").min_width(200.0).show(ctx, |ui| {
			ui.label(
				RichText::new(option_env!("BUILD_ID").unwrap_or("DEV"))
					.color(Color32::from_gray(100))
					.size(10.0),
			);

			ui.add_space(4.0);

			// Recording controls
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
				self.sessions.push(session::Session { start_ms: now_ms(), end_ms: i64::MAX });
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
						if let Ok(deepgram::transcription::live::StreamResponse::TranscriptResponse {
							duration,
							is_final,
							channel: Channel { mut alternatives, .. },
							..
						}) = result
						{
							if !is_final {
								*DURATION.lock().unwrap() += duration;
							}
							if let Some(deepgram::transcription::live::Alternatives { words, .. }) =
								alternatives.first_mut()
							{
								for word in words.iter() {
									TRANSCRIPT.lock().unwrap().push(Some(word.clone()));
								}
								TRANSCRIPT.lock().unwrap().push(None);
								if is_final {
									for word in words {
										TRANSCRIPT_FINAL
											.lock()
											.unwrap()
											.push(Some(Word::Live(word.clone())));
									}
									TRANSCRIPT_FINAL.lock().unwrap().push(None);
								}
							}
						}
					}
				});
			}

			if ui.button("⟳ Generate Universes Now").clicked() {
				let t = self.get_transcript();
				self.trigger_universes(t, ctx.clone());
			}

			if ui.button("⬆  Import audio").clicked() {
				tokio::spawn(async {
					let file = rfd::AsyncFileDialog::new().pick_file().await.unwrap().read().await;
					let source = AudioSource::from_buffer_with_mime_type(file, "audio/aac");
					let options = Options::builder()
						.punctuate(false)
						.diarize(true)
						.model(deepgram::transcription::prerecorded::options::Model::CustomId(
							"nova-2-meeting".to_string(),
						))
						.build();
					let dg_client =
						Deepgram::new(env::var("DEEPGRAM_API_KEY").expect("DEEPGRAM_API_KEY"));
					let mut response =
						dg_client.transcription().prerecorded(source, &options).await.unwrap();
					TRANSCRIPT_FINAL.lock().unwrap().extend(
						response.results.channels[0].alternatives[0]
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
			ui.label(RichText::new("Sessions").size(11.0).color(Color32::from_gray(140)));
			for (i, session) in self.sessions.iter().enumerate() {
				if ui
					.small_button(format!("#{} ({:.0}s)", i, session.duration_ms() as f32 / 1000.0))
					.clicked()
				{
					let samples = session.samples();
					tokio::spawn(async move {
						TRANSCRIPT_FINAL.lock().unwrap().clear();
						audiofile::save_wav_file(samples);
						let dg_client = Deepgram::new(
							env::var("DEEPGRAM_API_KEY").expect("DEEPGRAM_API_KEY"),
						);
						let file = File::open("temp_audio.aac").await.unwrap();
						let source =
							AudioSource::from_buffer_with_mime_type(file, "audio/aac");
						let options = Options::builder()
							.punctuate(false)
							.diarize(true)
							.model(deepgram::transcription::prerecorded::options::Model::CustomId(
								"nova-2-meeting".to_string(),
							))
							.build();
						let response: deepgram::transcription::prerecorded::response::Response =
							dg_client
								.transcription()
								.make_prerecorded_request_builder(source, &options)
								.query(&[("filler_words", "true")])
								.send()
								.await
								.unwrap()
								.json()
								.await
								.unwrap();
						for word in response.results.channels[0].alternatives[0].words.iter() {
							TRANSCRIPT_FINAL
								.lock()
								.unwrap()
								.push(Some(Word::Prerecorded(word.clone())));
						}
					});
				}
			}

			ui.add_space(6.0);
			ui.separator();
			ui.label(RichText::new("Speakers").size(11.0).color(Color32::from_gray(140)));

			while self.speaker_names.len() < 20 {
				self.speaker_names.push(format!("{}", self.speaker_names.len()));
			}
			for name in self.speaker_names.iter_mut().take(6) {
				ui.add(TextEdit::singleline(name).desired_width(f32::INFINITY));
			}

			ui.add_space(6.0);
			ui.separator();
			ui.label(
				RichText::new("Live words").size(11.0).color(Color32::from_gray(140)),
			);
			ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
				let words = TRANSCRIPT.lock().unwrap();
				let lines = words.split(|word| word.is_none());
				for line in lines {
					ui.horizontal_wrapped(|ui| {
						for word in line {
							if let Some(word) = word {
								let color = match word.speaker {
									0 => Color32::from_rgb(100, 220, 180),
									1 => Color32::from_rgb(180, 140, 255),
									2 => Color32::from_rgb(255, 200, 80),
									3 => Color32::from_rgb(255, 120, 120),
									_ => Color32::from_gray(180),
								};
								ui.label(
									RichText::new(word.word.clone()).color(color).size(12.0),
								);
							}
						}
					});
				}
			});
		});

		// ── Optional wheel windows (advanced chat) ───────────────────────
		if self.show_wheel_windows {
			let len = WHEEL_WINDOWS.lock().unwrap().len();
			for i in 0..len {
				egui::Window::new(format!("Channel {}", i)).show(ctx, |ui| {
					ScrollArea::vertical()
						.stick_to_bottom(true)
						.auto_shrink([false, false])
						.show(ui, |ui| {
							let mut wheel_windows = WHEEL_WINDOWS.lock().unwrap();
							let mut do_it = false;
							let mut do_it_j = 9999;
							for (j, entry) in
								wheel_windows.get_mut(i).unwrap().0.iter_mut().enumerate()
							{
								let id = Id::new(i * 1000 + j);
								let editor_has_focus =
									ui.ctx().memory(|m| m.has_focus(id));
								if editor_has_focus
									&& ui.input_mut(|inp| {
										inp.consume_key(
											Modifiers { command: true, ..Default::default() },
											Key::Enter,
										)
									}) {
									do_it = true;
									do_it_j = j;
								}
								ui.horizontal(|ui| {
									ui.radio_value(&mut entry.role, User, "user");
									ui.radio_value(&mut entry.role, System, "system");
									ui.radio_value(&mut entry.role, Assistant, "assistant");
									ui.add(
										TextEdit::multiline(&mut entry.content)
											.id(id)
											.desired_width(f32::INFINITY),
									);
								});
							}
							if do_it {
								let messages =
									&mut wheel_windows.get_mut(i).unwrap().0;
								messages.truncate(do_it_j + 1);
								let orig = messages.clone();
								messages.push(ChatMessage {
									role: Assistant,
									content: String::new(),
								});
								messages.push(ChatMessage {
									role: User,
									content: String::new(),
								});
								let id = messages.len() - 2;
								let transcript = self.get_transcript();
								let ctx2 = ctx.clone();
								let (trigger, tripwire) = Tripwire::new();
								self.trigger = Some(trigger);
								tokio::spawn(async move {
									run_openai(GPT_4, tripwire, orig, transcript, move |c| {
										WHEEL_WINDOWS
											.lock()
											.unwrap()
											.get_mut(i)
											.unwrap()
											.0
											.get_mut(id)
											.unwrap()
											.content
											.push_str(c);
										ctx2.request_repaint();
									})
									.await
									.unwrap();
								});
							}
						});
				});
			}
		}

		// ── Central panel: transcript strip + 3-column universes ─────────
		CentralPanel::default()
			.frame(
				egui::Frame::none()
					.fill(Color32::from_rgb(8, 8, 14))
					.inner_margin(Margin::same(10.0)),
			)
			.show(ctx, |ui| {
				// ── Top strip: live transcript ──────────────────────────
				let transcript_height = 100.0;
				let frame_color = Color32::from_rgb(20, 20, 32);

				egui::Frame::none()
					.fill(frame_color)
					.rounding(Rounding::same(8.0))
					.inner_margin(Margin::same(10.0))
					.show(ui, |ui| {
						ui.set_min_height(transcript_height);
						ui.set_max_height(transcript_height);

						ui.horizontal(|ui| {
							ui.label(
								RichText::new("LIVE TRANSCRIPT")
									.color(Color32::from_gray(120))
									.size(10.0)
									.strong(),
							);
							let generating = *UNIVERSE_GENERATING.lock().unwrap();
							if generating {
								ui.label(
									RichText::new("  ◉ universes updating...")
										.color(Color32::from_rgb(120, 200, 255))
										.size(10.0),
								);
							}
						});

						ScrollArea::vertical()
							.id_source("transcript_top")
							.stick_to_bottom(true)
							.auto_shrink([false, false])
							.max_height(transcript_height - 30.0)
							.show(ui, |ui| {
								let words = TRANSCRIPT_FINAL.lock().unwrap();
								let lines = words.split(|w| w.is_none());
								let speaker_colors = [
									Color32::from_rgb(100, 220, 180),
									Color32::from_rgb(180, 140, 255),
									Color32::from_rgb(255, 200, 80),
									Color32::from_rgb(255, 120, 120),
								];
								for line in lines {
									let mut cur_speaker = 100u8;
									ui.horizontal_wrapped(|ui| {
										for word in line {
											if let Some(word) = word {
												if word.speaker() != cur_speaker {
													cur_speaker = word.speaker();
													let name = self
														.speaker_names
														.get(cur_speaker as usize)
														.cloned()
														.unwrap_or_else(|| {
															cur_speaker.to_string()
														});
													let col = speaker_colors
														[(cur_speaker as usize) % 4];
													ui.label(
														RichText::new(format!(
															"[{}] ",
															name
														))
														.color(col)
														.size(13.0)
														.strong(),
													);
												}
												ui.label(
													RichText::new(word.word())
														.size(13.0)
														.color(Color32::from_gray(200)),
												);
											}
										}
									});
								}
							});
					});

				ui.add_space(10.0);

				// ── Title bar ────────────────────────────────────────────
				ui.vertical_centered(|ui| {
					ui.label(
						RichText::new("BRANCHING UNIVERSES")
							.color(Color32::from_gray(160))
							.size(11.0)
							.strong(),
					);
					ui.label(
						RichText::new(
							"Three paths. Three selves. Three realities.",
						)
						.color(Color32::from_gray(80))
						.size(10.0)
						.italics(),
					);
				});

				ui.add_space(8.0);

				// ── 3 Columns ─────────────────────────────────────────────
				let presence = UNIVERSE_PRESENCE.lock().unwrap().clone();
				let power = UNIVERSE_POWER.lock().unwrap().clone();
				let possibility = UNIVERSE_POSSIBILITY.lock().unwrap().clone();
				let generating = *UNIVERSE_GENERATING.lock().unwrap();

				// Update generating flag: done when all 3 have content
				if generating
					&& !presence.is_empty()
					&& !power.is_empty()
					&& !possibility.is_empty()
				{
					drop(presence);
					drop(power);
					drop(possibility);
					*UNIVERSE_GENERATING.lock().unwrap() = false;
					ctx.request_repaint();
					return;
				}
				let presence = UNIVERSE_PRESENCE.lock().unwrap().clone();
				let power = UNIVERSE_POWER.lock().unwrap().clone();
				let possibility = UNIVERSE_POSSIBILITY.lock().unwrap().clone();

				let available = ui.available_size();
				let col_width = (available.x - 20.0) / 3.0;

				ui.horizontal(|ui| {
					ui.set_min_height(available.y - 10.0);

					// Ω — Presence (teal)
					ui.allocate_ui(vec2(col_width, ui.available_height()), |ui| {
						universe_column(
							ui,
							"Ω",
							"PRESENCE",
							"Empathic Witness · Somatic Depth",
							Color32::from_rgb(40, 210, 180),
							Color32::from_rgb(100, 240, 210),
							&presence,
							generating,
						);
					});

					ui.add_space(10.0);

					// Δ — Power (violet)
					ui.allocate_ui(vec2(col_width, ui.available_height()), |ui| {
						universe_column(
							ui,
							"Δ",
							"POWER",
							"Sovereign Self · Embedded Commands",
							Color32::from_rgb(160, 80, 255),
							Color32::from_rgb(200, 140, 255),
							&power,
							generating,
						);
					});

					ui.add_space(10.0);

					// Φ — Possibility (gold)
					ui.allocate_ui(vec2(col_width, ui.available_height()), |ui| {
						universe_column(
							ui,
							"Φ",
							"POSSIBILITY",
							"Quantum Creator · Future Pacing",
							Color32::from_rgb(255, 190, 40),
							Color32::from_rgb(255, 220, 100),
							&possibility,
							generating,
						);
					});
				});
			});
	}
}

// ─────────────────────────────────────────────
// Main entry point
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
				.with_inner_size([1400.0, 900.0])
				.with_min_inner_size([900.0, 600.0]),
			..Default::default()
		},
		Box::new(|cc| Box::new(App::new(cc))),
	)
}

// ─────────────────────────────────────────────
// Audio: microphone → async stream
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
		let mut buffer = Vec::with_capacity(150000);
		loop {
			let data = sync_rx.recv();
			if let Ok(data) = &data {
				buffer.extend(data.clone().to_vec());
				if buffer.len() > 100000 {
					let moved_buffer = buffer;
					buffer = Vec::with_capacity(150000);
					tokio::task::spawn_blocking(|| {
						SampleData {
							rowid: None,
							record_ms: now_ms(),
							sample_data: moved_buffer,
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
// OpenAI helpers
// ─────────────────────────────────────────────
const GPT_3_5: &str = "gpt-3.5-turbo-0125";
const GPT_4: &str = "gpt-4-0125-preview";

pub(crate) async fn run_openai(
	model: &str,
	tripwire: Tripwire,
	messages: Vec<ChatMessage>,
	transcript: String,
	callback: impl Fn(&String) + Send + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
	use async_openai::{types::CreateChatCompletionRequestArgs, Client};
	use futures::StreamExt;

	let client = Client::new();

	let mut req_messages = messages
		.into_iter()
		.map(|m| match m.role {
			System => async_openai::types::ChatCompletionRequestSystemMessageArgs::default()
				.content(m.content)
				.build()
				.unwrap()
				.into(),
			User => async_openai::types::ChatCompletionRequestUserMessageArgs::default()
				.content(m.content)
				.build()
				.unwrap()
				.into(),
			Assistant => async_openai::types::ChatCompletionRequestAssistantMessageArgs::default()
				.content(m.content)
				.build()
				.unwrap()
				.into(),
			_ => panic!("invalid role"),
		})
		.collect::<Vec<ChatCompletionRequestMessage>>();

	if !transcript.is_empty() {
		req_messages.insert(
			0,
			async_openai::types::ChatCompletionRequestUserMessageArgs::default()
				.content(transcript)
				.build()
				.unwrap()
				.into(),
		);
	}

	let request = CreateChatCompletionRequestArgs::default()
		.model(model)
		.max_tokens(4096u16)
		.messages(req_messages)
		.build()?;

	let mut stream = client.chat().create_stream(request).await?.take_until_if(tripwire);

	while let Some(result) = stream.next().await {
		match result {
			Ok(response) => {
				response.choices.iter().for_each(|chat_choice| {
					if let Some(ref content) = chat_choice.delta.content {
						callback(content);
					}
				});
			}
			Err(err) => {
				eprintln!("OpenAI error: {err}");
				break;
			}
		}
	}

	Ok(())
}

pub(crate) async fn run_openai_completion(
	tripwire: Tripwire,
	prompt: String,
	callback: impl Fn(&String) + Send + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
	use async_openai::{types::CreateCompletionRequestArgs, Client};
	use futures::StreamExt;

	let client = Client::new();

	let mut logit_bias: HashMap<String, serde_json::Value> = HashMap::new();
	["198", "271", "1432", "4815", "1980", "382", "720", "627"].iter().for_each(|s| {
		logit_bias.insert(
			s.to_string(),
			serde_json::Value::Number(serde_json::Number::from_f64(-100.).unwrap()),
		);
	});

	let request = CreateCompletionRequestArgs::default()
		.model("gpt-3.5-turbo-instruct")
		.max_tokens(100u16)
		.logit_bias(logit_bias)
		.prompt(prompt)
		.n(1)
		.stream(true)
		.build()?;

	let mut stream = client.completions().create_stream(request).await?.take_until_if(tripwire);

	while let Some(result) = stream.next().await {
		match result {
			Ok(response) => {
				response.choices.iter().for_each(|c| {
					callback(&c.text);
				});
			}
			Err(err) => {
				eprintln!("completion error: {err}");
				break;
			}
		}
	}

	Ok(())
}

// ─────────────────────────────────────────────
// HTTP / syntax highlighting (kept for ehttp panel)
// ─────────────────────────────────────────────
fn ui_url(ui: &mut Ui, _frame: &mut eframe::Frame, url: &mut String) -> bool {
	let mut trigger_fetch = false;
	ui.horizontal(|ui| {
		ui.label("URL:");
		trigger_fetch |= ui.add(TextEdit::singleline(url).desired_width(f32::INFINITY)).lost_focus();
	});
	trigger_fetch
}

fn ui_resource(ui: &mut Ui, resource: &Resource) {
	let Resource { response, text, image, colored_text } = resource;
	ui.monospace(format!("url:          {}", response.url));
	ui.monospace(format!("status:       {} ({})", response.status, response.status_text));
	ui.monospace(format!(
		"content-type: {}",
		response.content_type().unwrap_or_default()
	));
	ui.monospace(format!("size:         {:.1} kB", response.bytes.len() as f32 / 1000.0));
	ui.separator();
	ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
		if let Some(image) = image {
			ui.add(image.clone());
		} else if let Some(colored_text) = colored_text {
			colored_text.ui(ui);
		} else if let Some(text) = &text {
			selectable_text(ui, text);
		} else {
			ui.monospace("[binary]");
		}
	});
}

fn selectable_text(ui: &mut Ui, mut text: &str) {
	ui.add(TextEdit::multiline(&mut text).desired_width(f32::INFINITY).font(TextStyle::Monospace));
}

fn syntax_highlighting(
	ctx: &Context,
	response: &ehttp::Response,
	text: &str,
) -> Option<ColoredText> {
	let extension_and_rest: Vec<&str> = response.url.rsplitn(2, '.').collect();
	let extension = extension_and_rest.first()?;
	let theme = egui_extras::syntax_highlighting::CodeTheme::from_style(&ctx.style());
	Some(ColoredText(egui_extras::syntax_highlighting::highlight(ctx, &theme, text, extension)))
}

struct ColoredText(text::LayoutJob);

impl ColoredText {
	pub fn ui(&self, ui: &mut Ui) {
		let mut layouter = |ui: &Ui, _string: &str, wrap_width: f32| {
			let mut layout_job = self.0.clone();
			layout_job.wrap.max_width = wrap_width;
			ui.fonts(|f| f.layout_job(layout_job))
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
