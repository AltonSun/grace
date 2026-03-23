//! Deepgram pre-recorded transcription API response types.
//!
//! See the [Deepgram API Reference][api] for more info.
//!
//! [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded-responses

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Returned by [`Transcription::prerecorded`](crate::transcription::Transcription::prerecorded).
///
/// See the [Deepgram API Reference][api] for more info.
///
/// [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Response {
    #[allow(missing_docs)]
    pub metadata: ListenMetadata,

    #[allow(missing_docs)]
    pub results: ListenResults,
}

/// Returned by [`Transcription::prerecorded_callback`](crate::transcription::Transcription::prerecorded_callback).
///
/// See the [Deepgram Callback feature docs][docs] for more info.
///
/// [docs]: https://developers.deepgram.com/documentation/features/callback/
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CallbackResponse {
    #[allow(missing_docs)]
    pub request_id: Uuid,
}

/// Metadata about the transcription.
///
/// See the [Deepgram API Reference][api] for more info.
///
/// [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ListenMetadata {
    #[allow(missing_docs)]
    pub request_id: Uuid,

    #[allow(missing_docs)]
    pub transaction_key: String,

    #[allow(missing_docs)]
    pub sha256: String,

    #[allow(missing_docs)]
    pub created: String,

    #[allow(missing_docs)]
    pub duration: f64,

    #[allow(missing_docs)]
    pub channels: usize,
}

/// Transcription results.
///
/// See the [Deepgram API Reference][api] for more info.
///
/// [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ListenResults {
    #[allow(missing_docs)]
    pub channels: Vec<ChannelResult>,

    /// [`None`] unless the [Utterances feature][docs] is set.
    /// Features can be set using an [`OptionsBuilder`](`super::options::OptionsBuilder`).
    ///
    /// [docs]: https://developers.deepgram.com/documentation/features/utterances/
    pub utterances: Option<Vec<Utterance>>,
}

/// Transcription results for a single audio channel.
///
/// See the [Deepgram API Reference][api]
/// and the [Deepgram Multichannel feature docs][docs] for more info.
///
/// [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded
/// [docs]: https://developers.deepgram.com/documentation/features/multichannel/
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChannelResult {
    /// [`None`] unless the [Search feature][docs] is set.
    /// Features can be set using an [`OptionsBuilder`](`super::options::OptionsBuilder`).
    ///
    /// [docs]: https://developers.deepgram.com/documentation/features/search/
    pub search: Option<Vec<SearchResults>>,

    #[allow(missing_docs)]
    pub alternatives: Vec<ResultAlternative>,

    ///  [BCP-47][bcp47] language tag for the dominant language identified in the channel.
    ///
    /// [`None`] unless the [Language Detection feature][docs] is set.
    /// Features can be set using an [`OptionsBuilder`](`super::options::OptionsBuilder`).
    ///
    /// [bcp47]: https://tools.ietf.org/html/bcp47
    /// [docs]: https://developers.deepgram.com/docs/language-detection/
    pub detected_language: Option<String>,
}

/// Transcription results for a single utterance.
///
/// See the [Deepgram Utterance feature docs][docs] for more info.
///
/// [docs]: https://developers.deepgram.com/documentation/features/utterances/
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Utterance {
    #[allow(missing_docs)]
    pub start: f64,

    #[allow(missing_docs)]
    pub end: f64,

    #[allow(missing_docs)]
    pub confidence: f64,

    #[allow(missing_docs)]
    pub channel: usize,

    #[allow(missing_docs)]
    pub transcript: String,

    #[allow(missing_docs)]
    pub words: Vec<Word>,

    /// [`None`] unless the [Diarization feature][docs] is set.
    /// Features can be set using an [`OptionsBuilder`](`super::options::OptionsBuilder`).
    ///
    /// [docs]: https://developers.deepgram.com/documentation/features/diarize/
    pub speaker: Option<usize>,

    #[allow(missing_docs)]
    pub id: Uuid,
}

/// Search results.
///
/// See the [Deepgram API Reference][api]
/// and the [Deepgram Search feature docs][docs] for more info.
///
/// [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded
/// [docs]: https://developers.deepgram.com/documentation/features/search/
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResults {
    #[allow(missing_docs)]
    pub query: String,

    #[allow(missing_docs)]
    pub hits: Vec<Hit>,
}

/// Transcript alternatives.
///
/// See the [Deepgram API Reference][api] for more info.
///
/// [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResultAlternative {
    #[allow(missing_docs)]
    pub transcript: String,

    #[allow(missing_docs)]
    pub confidence: f64,

    #[allow(missing_docs)]
    pub words: Vec<Word>,
}

/// A single transcribed word.
///
/// See the [Deepgram API Reference][api] for more info.
///
/// [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Word {
    #[allow(missing_docs)]
    pub word: String,

    #[allow(missing_docs)]
    pub start: f64,

    #[allow(missing_docs)]
    pub end: f64,

    #[allow(missing_docs)]
    pub confidence: f64,

    /// [`None`] unless the [Diarization feature][docs] is set.
    /// Features can be set using an [`OptionsBuilder`](`super::options::OptionsBuilder`).
    ///
    /// [docs]: https://developers.deepgram.com/documentation/features/diarize/
    pub speaker: Option<usize>,
    pub speaker_confidence: Option<f64>,

    /// [`None`] unless the [Punctuation feature][docs] is set.
    /// Features can be set using an [`OptionsBuilder`](`super::options::OptionsBuilder`).
    ///
    /// [docs]: https://developers.deepgram.com/documentation/features/punctuate/
    pub punctuated_word: Option<String>,
}

/// Search result.
///
/// See the [Deepgram API Reference][api]
/// and the [Deepgram Search feature docs][docs] for more info.
///
/// [api]: https://developers.deepgram.com/api-reference/#transcription-prerecorded
/// [docs]: https://developers.deepgram.com/documentation/features/search/
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Hit {
    #[allow(missing_docs)]
    pub confidence: f64,

    #[allow(missing_docs)]
    pub start: f64,

    #[allow(missing_docs)]
    pub end: f64,

    #[allow(missing_docs)]
    pub snippet: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_word_without_optional_fields() {
        let json = r#"{
            "word": "hello",
            "start": 0.5,
            "end": 1.0,
            "confidence": 0.99
        }"#;
        let word: Word = serde_json::from_str(json).unwrap();
        assert_eq!(word.word, "hello");
        assert_eq!(word.start, 0.5);
        assert_eq!(word.end, 1.0);
        assert_eq!(word.confidence, 0.99);
        assert!(word.speaker.is_none());
        assert!(word.punctuated_word.is_none());
    }

    #[test]
    fn deserialize_word_with_diarization() {
        let json = r#"{
            "word": "hello",
            "start": 0.5,
            "end": 1.0,
            "confidence": 0.99,
            "speaker": 1,
            "speaker_confidence": 0.95,
            "punctuated_word": "Hello,"
        }"#;
        let word: Word = serde_json::from_str(json).unwrap();
        assert_eq!(word.speaker, Some(1));
        assert_eq!(word.speaker_confidence, Some(0.95));
        assert_eq!(word.punctuated_word, Some("Hello,".to_string()));
    }

    #[test]
    fn deserialize_result_alternative() {
        let json = r#"{
            "transcript": "hello world",
            "confidence": 0.98,
            "words": [
                {"word": "hello", "start": 0.0, "end": 0.5, "confidence": 0.99},
                {"word": "world", "start": 0.6, "end": 1.2, "confidence": 0.97}
            ]
        }"#;
        let alt: ResultAlternative = serde_json::from_str(json).unwrap();
        assert_eq!(alt.transcript, "hello world");
        assert_eq!(alt.confidence, 0.98);
        assert_eq!(alt.words.len(), 2);
        assert_eq!(alt.words[0].word, "hello");
        assert_eq!(alt.words[1].word, "world");
    }

    #[test]
    fn deserialize_channel_result_without_search() {
        let json = r#"{
            "alternatives": [
                {
                    "transcript": "test transcript",
                    "confidence": 0.95,
                    "words": []
                }
            ]
        }"#;
        let channel: ChannelResult = serde_json::from_str(json).unwrap();
        assert_eq!(channel.alternatives.len(), 1);
        assert_eq!(channel.alternatives[0].transcript, "test transcript");
        assert!(channel.search.is_none());
        assert!(channel.detected_language.is_none());
    }

    #[test]
    fn deserialize_channel_result_with_detected_language() {
        let json = r#"{
            "alternatives": [],
            "detected_language": "en"
        }"#;
        let channel: ChannelResult = serde_json::from_str(json).unwrap();
        assert_eq!(channel.detected_language, Some("en".to_string()));
    }

    #[test]
    fn deserialize_listen_results_without_utterances() {
        let json = r#"{
            "channels": [
                {
                    "alternatives": [
                        {"transcript": "hi", "confidence": 0.9, "words": []}
                    ]
                }
            ]
        }"#;
        let results: ListenResults = serde_json::from_str(json).unwrap();
        assert_eq!(results.channels.len(), 1);
        assert!(results.utterances.is_none());
    }

    #[test]
    fn roundtrip_word_serialization() {
        let word = Word {
            word: "test".to_string(),
            start: 1.0,
            end: 2.0,
            confidence: 0.88,
            speaker: Some(0),
            speaker_confidence: Some(0.77),
            punctuated_word: Some("Test.".to_string()),
        };
        let json = serde_json::to_string(&word).unwrap();
        let deserialized: Word = serde_json::from_str(&json).unwrap();
        assert_eq!(word, deserialized);
    }
}
