use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub type ProgressCallback = Arc<dyn Fn(&str, f64) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FoundryLocalError {
    NotConfigured,
    NotCached(String),
    Request(String),
}

impl std::fmt::Display for FoundryLocalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => write!(formatter, "Foundry Local model is not configured"),
            Self::NotCached(alias) => write!(
                formatter,
                "Foundry Local model '{alias}' is not cached. Download it from Settings before recording."
            ),
            Self::Request(message) => write!(formatter, "Foundry Local failed: {message}"),
        }
    }
}

impl std::error::Error for FoundryLocalError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FoundryLocalModelCatalogEntry {
    pub alias: String,
    pub display_name: Option<String>,
    pub task: Option<String>,
    pub model_type: String,
    pub input_modalities: Option<String>,
    pub output_modalities: Option<String>,
    pub cached: bool,
    pub file_size_mb: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct FoundryLocalTranscriptionRequest {
    pub model_alias: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct FoundryLocalTranscriptionSegment {
    pub no_speech_prob: Option<f64>,
    pub avg_logprob: Option<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct FoundryLocalTranscription {
    pub text: String,
    pub segments: Vec<FoundryLocalTranscriptionSegment>,
}

impl FoundryLocalTranscription {
    pub(crate) fn accepted_text(self) -> Option<String> {
        let text = self.text.trim().to_string();
        if text.is_empty() || !should_accept_transcription(&self.segments) {
            None
        } else {
            Some(text)
        }
    }
}

struct FoundryLocalService {
    runtime: tokio::runtime::Runtime,
    manager: &'static foundry_local_sdk::FoundryLocalManager,
    eps_ready: Mutex<bool>,
}

impl FoundryLocalService {
    fn new() -> Result<Self, FoundryLocalError> {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|error| FoundryLocalError::Request(error.to_string()))?;
        let manager = foundry_local_sdk::FoundryLocalManager::create(
            foundry_local_sdk::FoundryLocalConfig::new("meetly_lite"),
        )
        .map_err(to_foundry_error)?;
        Ok(Self {
            runtime,
            manager,
            eps_ready: Mutex::new(false),
        })
    }

    fn list_models(&self) -> Result<Vec<FoundryLocalModelCatalogEntry>, FoundryLocalError> {
        self.runtime.block_on(async {
            let models = self
                .manager
                .catalog()
                .get_models()
                .await
                .map_err(to_foundry_error)?;
            let mut entries = models
                .iter()
                .map(|model| {
                    let info = model.info();
                    FoundryLocalModelCatalogEntry {
                        alias: model.alias().to_string(),
                        display_name: info.display_name.clone(),
                        task: info.task.clone(),
                        model_type: info.model_type.clone(),
                        input_modalities: info.input_modalities.clone(),
                        output_modalities: info.output_modalities.clone(),
                        cached: info.cached,
                        file_size_mb: info.file_size_mb,
                    }
                })
                .filter(is_stt_model)
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| left.alias.cmp(&right.alias));
            Ok(entries)
        })
    }

    fn prepare_model(
        &self,
        model_alias: &str,
        on_progress: ProgressCallback,
    ) -> Result<(), FoundryLocalError> {
        self.runtime.block_on(async {
            self.ensure_execution_providers_with_progress(Arc::clone(&on_progress))
                .await?;
            self.loaded_model_with_progress(model_alias, on_progress)
                .await
                .map(|_| ())
        })
    }

    fn ensure_model_ready(&self, model_alias: &str) -> Result<(), FoundryLocalError> {
        self.runtime.block_on(async {
            self.ensure_execution_providers().await?;
            self.loaded_model(model_alias).await.map(|_| ())
        })
    }

    fn transcribe(
        &self,
        request: FoundryLocalTranscriptionRequest,
        wav_bytes: Vec<u8>,
    ) -> Result<FoundryLocalTranscription, FoundryLocalError> {
        let model_alias = request.model_alias.trim();
        if model_alias.is_empty() {
            return Err(FoundryLocalError::NotConfigured);
        }
        let temp_audio = TempAudioFile::write(wav_bytes)?;
        self.runtime.block_on(async {
            self.ensure_execution_providers().await?;
            let model = self.loaded_model(model_alias).await?;
            let mut audio_client = model.create_audio_client().temperature(0.0);
            if let Some(language) = request
                .language
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty() && *value != "auto")
            {
                audio_client = audio_client.language(language);
            }
            let result = audio_client
                .transcribe(&temp_audio.path)
                .await
                .map_err(to_foundry_error)?;
            Ok(FoundryLocalTranscription {
                text: result.text,
                segments: result
                    .segments
                    .unwrap_or_default()
                    .into_iter()
                    .map(|segment| FoundryLocalTranscriptionSegment {
                        no_speech_prob: segment.no_speech_prob,
                        avg_logprob: segment.avg_logprob,
                    })
                    .collect(),
            })
        })
    }

    async fn ensure_execution_providers(&self) -> Result<(), FoundryLocalError> {
        {
            let ready = self.eps_ready.lock().map_err(|_| {
                FoundryLocalError::Request("failed to lock Foundry Local state".to_string())
            })?;
            if *ready {
                return Ok(());
            }
        }

        self.manager
            .download_and_register_eps_with_progress(None, |_ep_name, _percent| {})
            .await
            .map_err(to_foundry_error)?;

        let mut ready = self.eps_ready.lock().map_err(|_| {
            FoundryLocalError::Request("failed to lock Foundry Local state".to_string())
        })?;
        *ready = true;
        Ok(())
    }

    async fn ensure_execution_providers_with_progress(
        &self,
        on_progress: ProgressCallback,
    ) -> Result<(), FoundryLocalError> {
        {
            let ready = self.eps_ready.lock().map_err(|_| {
                FoundryLocalError::Request("failed to lock Foundry Local state".to_string())
            })?;
            if *ready {
                return Ok(());
            }
        }

        let cb = Arc::clone(&on_progress);
        self.manager
            .download_and_register_eps_with_progress(None, move |_ep_name, percent| {
                cb("eps", percent);
            })
            .await
            .map_err(to_foundry_error)?;

        let mut ready = self.eps_ready.lock().map_err(|_| {
            FoundryLocalError::Request("failed to lock Foundry Local state".to_string())
        })?;
        *ready = true;
        Ok(())
    }

    async fn loaded_model(
        &self,
        model_alias: &str,
    ) -> Result<Arc<foundry_local_sdk::Model>, FoundryLocalError> {
        let model = self.cached_model(model_alias).await?;
        if !model.is_loaded().await.map_err(to_foundry_error)? {
            if let Err(error) = model.load().await {
                if should_retry_after_model_cache_error(&error) {
                    model.remove_from_cache().await.map_err(to_foundry_error)?;
                    model
                        .download(None::<fn(f64)>)
                        .await
                        .map_err(to_foundry_error)?;
                    model.load().await.map_err(to_foundry_error)?;
                } else {
                    return Err(to_foundry_error(error));
                }
            }
        }
        Ok(model)
    }

    async fn loaded_model_with_progress(
        &self,
        model_alias: &str,
        on_progress: ProgressCallback,
    ) -> Result<Arc<foundry_local_sdk::Model>, FoundryLocalError> {
        let model = self
            .cached_model_with_progress(model_alias, Arc::clone(&on_progress))
            .await?;
        if !model.is_loaded().await.map_err(to_foundry_error)? {
            if let Err(error) = model.load().await {
                if should_retry_after_model_cache_error(&error) {
                    model.remove_from_cache().await.map_err(to_foundry_error)?;
                    let cb = Arc::clone(&on_progress);
                    model
                        .download(Some(move |percent: f64| {
                            cb("model", percent);
                        }))
                        .await
                        .map_err(to_foundry_error)?;
                    model.load().await.map_err(to_foundry_error)?;
                } else {
                    return Err(to_foundry_error(error));
                }
            }
        }
        Ok(model)
    }

    async fn cached_model(
        &self,
        model_alias: &str,
    ) -> Result<Arc<foundry_local_sdk::Model>, FoundryLocalError> {
        let model = self
            .manager
            .catalog()
            .get_model(model_alias)
            .await
            .map_err(to_foundry_error)?;
        if !model.is_cached().await.map_err(to_foundry_error)? {
            return Err(FoundryLocalError::NotCached(model_alias.to_string()));
        }
        Ok(model)
    }

    async fn cached_model_with_progress(
        &self,
        model_alias: &str,
        on_progress: ProgressCallback,
    ) -> Result<Arc<foundry_local_sdk::Model>, FoundryLocalError> {
        let model = self
            .manager
            .catalog()
            .get_model(model_alias)
            .await
            .map_err(to_foundry_error)?;
        if !model.is_cached().await.map_err(to_foundry_error)? {
            let cb = Arc::clone(&on_progress);
            model
                .download(Some(move |percent: f64| {
                    cb("model", percent);
                }))
                .await
                .map_err(to_foundry_error)?;
        }
        Ok(model)
    }
}

struct TempAudioFile {
    path: PathBuf,
}

impl TempAudioFile {
    fn write(bytes: Vec<u8>) -> Result<Self, FoundryLocalError> {
        let path = std::env::temp_dir().join(format!(
            "meetly-lite-foundry-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| FoundryLocalError::Request(error.to_string()))?
                .as_nanos()
        ));
        fs::write(&path, bytes).map_err(|error| {
            FoundryLocalError::Request(format!("failed to write temporary audio file: {error}"))
        })?;
        Ok(Self { path })
    }
}

impl Drop for TempAudioFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn foundry_service() -> Result<&'static FoundryLocalService, FoundryLocalError> {
    static SERVICE: OnceLock<Result<FoundryLocalService, String>> = OnceLock::new();
    SERVICE
        .get_or_init(|| FoundryLocalService::new().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| FoundryLocalError::Request(error.clone()))
}

pub(crate) fn list_foundry_local_models(
) -> Result<Vec<FoundryLocalModelCatalogEntry>, FoundryLocalError> {
    foundry_service()?.list_models()
}

pub(crate) fn prepare_foundry_local_model(
    model_alias: &str,
    on_progress: ProgressCallback,
) -> Result<(), FoundryLocalError> {
    foundry_service()?.prepare_model(model_alias, on_progress)
}

pub(crate) fn ensure_foundry_local_model_ready(model_alias: &str) -> Result<(), FoundryLocalError> {
    foundry_service()?.ensure_model_ready(model_alias)
}

pub(crate) fn transcribe_foundry_local_audio(
    request: FoundryLocalTranscriptionRequest,
    wav_bytes: Vec<u8>,
) -> Result<FoundryLocalTranscription, FoundryLocalError> {
    foundry_service()?.transcribe(request, wav_bytes)
}

pub(crate) fn is_stt_model(model: &FoundryLocalModelCatalogEntry) -> bool {
    let task = model.task.as_deref().unwrap_or_default().to_lowercase();
    let model_type = model.model_type.to_lowercase();
    let input = model
        .input_modalities
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let output = model
        .output_modalities
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();

    task.contains("speech")
        || task.contains("transcrib")
        || model_type.contains("whisper")
        || (input.contains("audio") && output.contains("text"))
}

fn should_accept_transcription(segments: &[FoundryLocalTranscriptionSegment]) -> bool {
    const NO_SPEECH_THRESHOLD: f64 = 0.6;
    const LOGPROB_THRESHOLD: f64 = -1.0;

    segments.is_empty()
        || segments.iter().any(|segment| {
            !matches!(
                (segment.no_speech_prob, segment.avg_logprob),
                (Some(no_speech), Some(avg_logprob))
                    if no_speech > NO_SPEECH_THRESHOLD && avg_logprob < LOGPROB_THRESHOLD
            )
        })
}

fn to_foundry_error(error: impl std::fmt::Display) -> FoundryLocalError {
    FoundryLocalError::Request(error.to_string())
}

fn should_retry_after_model_cache_error(error: &impl std::fmt::Display) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("parse error")
        || message.contains("invalid string")
        || message.contains("control character")
        || message.contains("not valid json")
        || message.contains("unexpected end of json")
}

#[cfg(test)]
mod tests {
    use super::{
        is_stt_model, should_accept_transcription, FoundryLocalModelCatalogEntry,
        FoundryLocalTranscriptionSegment,
    };

    fn model(
        task: Option<&str>,
        model_type: &str,
        input_modalities: Option<&str>,
        output_modalities: Option<&str>,
    ) -> FoundryLocalModelCatalogEntry {
        FoundryLocalModelCatalogEntry {
            alias: "model".to_string(),
            display_name: None,
            task: task.map(ToString::to_string),
            model_type: model_type.to_string(),
            input_modalities: input_modalities.map(ToString::to_string),
            output_modalities: output_modalities.map(ToString::to_string),
            cached: false,
            file_size_mb: None,
        }
    }

    #[test]
    fn accepts_speech_models_from_catalog_metadata() {
        assert!(is_stt_model(&model(
            Some("speech-to-text"),
            "onnx",
            None,
            None
        )));
        assert!(is_stt_model(&model(None, "whisper", None, None)));
        assert!(is_stt_model(&model(
            None,
            "onnx",
            Some("audio"),
            Some("text")
        )));
    }

    #[test]
    fn rejects_chat_only_models_from_catalog_metadata() {
        assert!(!is_stt_model(&model(
            Some("text-generation"),
            "phi",
            Some("text"),
            Some("text")
        )));
    }

    fn segment(
        no_speech_prob: Option<f64>,
        avg_logprob: Option<f64>,
    ) -> FoundryLocalTranscriptionSegment {
        FoundryLocalTranscriptionSegment {
            no_speech_prob,
            avg_logprob,
        }
    }

    #[test]
    fn rejects_only_when_every_segment_is_confidently_silent() {
        let silent = vec![
            segment(Some(0.91), Some(-1.4)),
            segment(Some(0.75), Some(-1.1)),
        ];
        assert!(!should_accept_transcription(&silent));

        let contains_speech = vec![
            segment(Some(0.91), Some(-1.4)),
            segment(Some(0.2), Some(-0.4)),
        ];
        assert!(should_accept_transcription(&contains_speech));
    }

    #[test]
    fn accepts_results_with_missing_confidence_metadata() {
        assert!(should_accept_transcription(&[]));
        assert!(should_accept_transcription(&[segment(None, None)]));
    }
}
