use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::AppHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservedCallState {
    Other,
    Talking { muted: bool },
    Held { muted: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExternalMuteState {
    NoActiveCalls,
    Unanimous(bool),
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PublishedMuteState {
    external_muted: Option<bool>,
    published_muted: bool,
}

fn select_external_mute_state(calls: &[ObservedCallState]) -> ExternalMuteState {
    let mut active_states = calls.iter().filter_map(|call| match call {
        ObservedCallState::Talking { muted } => Some(*muted),
        ObservedCallState::Held { .. } | ObservedCallState::Other => None,
    });
    let Some(selected) = active_states.next() else {
        return ExternalMuteState::NoActiveCalls;
    };
    if active_states.all(|muted| muted == selected) {
        ExternalMuteState::Unanimous(selected)
    } else {
        ExternalMuteState::Ambiguous
    }
}

fn reconcile_external_mute_state(
    state: &mut PublishedMuteState,
    update: ExternalMuteState,
) -> Option<bool> {
    match update {
        ExternalMuteState::NoActiveCalls => state.external_muted = None,
        ExternalMuteState::Unanimous(muted) => state.external_muted = Some(muted),
        ExternalMuteState::Ambiguous => return None,
    }

    let effective = state.external_muted.unwrap_or(false);
    if effective == state.published_muted {
        None
    } else {
        state.published_muted = effective;
        Some(effective)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::{Condvar, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use tauri::Emitter;
    use windows::core::{IInspectable, GUID};
    use windows::ApplicationModel::Calls::{
        PhoneCallManager, PhoneCallStatus, PhoneLine, PhoneLineOperationStatus, PhoneLineWatcher,
        PhoneLineWatcherEventArgs, PhoneLineWatcherStatus,
    };
    use windows::Foundation::TypedEventHandler;
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

    const EXTERNAL_CALL_POLL_INTERVAL: Duration = Duration::from_millis(200);
    const WATCHER_RETRY_INTERVAL: Duration = Duration::from_secs(5);

    pub(crate) struct TeamsMuteMonitor {
        monitor_stop: Arc<StopSignal>,
        monitor_thread: Option<JoinHandle<()>>,
    }

    struct MuteStateController {
        state: Mutex<PublishedMuteState>,
        microphone_muted: Arc<AtomicBool>,
        app: AppHandle,
        meeting_id: String,
    }

    impl MuteStateController {
        fn new(app: AppHandle, meeting_id: String, microphone_muted: Arc<AtomicBool>) -> Self {
            Self {
                state: Mutex::new(PublishedMuteState::default()),
                microphone_muted,
                app,
                meeting_id,
            }
        }

        fn set_external(&self, update: ExternalMuteState) {
            let Ok(mut state) = self.state.lock() else {
                emit_warning(
                    &self.app,
                    &self.meeting_id,
                    "Microphone mute state could not be synchronized.".to_string(),
                );
                return;
            };
            if let Some(muted) = reconcile_external_mute_state(&mut state, update) {
                self.microphone_muted.store(muted, Ordering::Release);
                emit_mute_changed(&self.app, &self.meeting_id, muted);
            }
        }
    }

    struct WinRtApartment;

    impl WinRtApartment {
        fn initialize() -> windows::core::Result<Self> {
            unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
            Ok(Self)
        }
    }

    impl Drop for WinRtApartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }

    #[derive(Default)]
    struct StopSignal {
        stopped: Mutex<bool>,
        wake: Condvar,
    }

    impl StopSignal {
        fn stop(&self) {
            if let Ok(mut stopped) = self.stopped.lock() {
                *stopped = true;
            }
            self.wake.notify_all();
        }

        fn is_stopped(&self) -> bool {
            self.stopped.lock().map(|stopped| *stopped).unwrap_or(true)
        }

        fn wait(&self, duration: Duration) -> bool {
            let Ok(stopped) = self.stopped.lock() else {
                return true;
            };
            if *stopped {
                return true;
            }
            self.wake
                .wait_timeout(stopped, duration)
                .map(|(stopped, _)| *stopped)
                .unwrap_or(true)
        }
    }

    impl TeamsMuteMonitor {
        pub(crate) fn start(
            app: AppHandle,
            meeting_id: String,
            microphone_muted: Arc<AtomicBool>,
        ) -> Result<Self, String> {
            let mute_state = Arc::new(MuteStateController::new(
                app.clone(),
                meeting_id.clone(),
                microphone_muted,
            ));
            let monitor_stop = Arc::new(StopSignal::default());
            let thread_stop = monitor_stop.clone();
            let thread_app = app;
            let thread_meeting_id = meeting_id;
            let monitor_thread = thread::Builder::new()
                .name("external-call-mute".to_string())
                .spawn(move || {
                    let apartment = match WinRtApartment::initialize() {
                        Ok(apartment) => apartment,
                        Err(error) => {
                            emit_warning(
                                &thread_app,
                                &thread_meeting_id,
                                format!(
                                    "External Call Mute monitoring could not initialize: {error}"
                                ),
                            );
                            return;
                        }
                    };
                    monitor_external_calls(thread_stop, mute_state);
                    drop(apartment);
                })
                .map_err(|error| format!("Teams mute monitor could not start: {error}"))?;

            Ok(Self {
                monitor_stop,
                monitor_thread: Some(monitor_thread),
            })
        }
    }

    impl Drop for TeamsMuteMonitor {
        fn drop(&mut self) {
            self.monitor_stop.stop();
            if let Some(thread) = self.monitor_thread.take() {
                if thread.is_finished() {
                    let _ = thread.join();
                }
            }
        }
    }

    fn monitor_external_calls(stop: Arc<StopSignal>, mute_state: Arc<MuteStateController>) {
        while !stop.is_stopped() {
            match monitor_phone_lines(&stop, &mute_state) {
                Ok(false) => break,
                Ok(true) | Err(_) => {}
            }
            if stop.wait(WATCHER_RETRY_INTERVAL) {
                break;
            }
        }
    }

    fn monitor_phone_lines(
        stop: &StopSignal,
        mute_state: &MuteStateController,
    ) -> windows::core::Result<bool> {
        let store = PhoneCallManager::RequestStoreAsync()?.get()?;
        let watcher = store.RequestLineWatcher()?;
        let line_ids = Arc::new(Mutex::new(Vec::<GUID>::new()));

        let added_line_ids = line_ids.clone();
        let line_added = TypedEventHandler::<PhoneLineWatcher, PhoneLineWatcherEventArgs>::new(
            move |_, args| {
                if let Some(args) = args.as_ref() {
                    if let Ok(line_id) = args.LineId() {
                        if let Ok(mut ids) = added_line_ids.lock() {
                            if !ids.contains(&line_id) {
                                ids.push(line_id);
                            }
                        }
                    }
                }
                Ok(())
            },
        );
        let line_added_token = watcher.LineAdded(&line_added)?;

        let removed_line_ids = line_ids.clone();
        let line_removed = TypedEventHandler::<PhoneLineWatcher, PhoneLineWatcherEventArgs>::new(
            move |_, args| {
                if let Some(args) = args.as_ref() {
                    if let Ok(line_id) = args.LineId() {
                        if let Ok(mut ids) = removed_line_ids.lock() {
                            ids.retain(|id| id != &line_id);
                        }
                    }
                }
                Ok(())
            },
        );
        let line_removed_token = match watcher.LineRemoved(&line_removed) {
            Ok(token) => token,
            Err(error) => {
                let _ = watcher.RemoveLineAdded(line_added_token);
                return Err(error);
            }
        };

        let enumeration_complete = Arc::new(AtomicBool::new(false));
        let event_enumeration_complete = enumeration_complete.clone();
        let enumeration_completed =
            TypedEventHandler::<PhoneLineWatcher, IInspectable>::new(move |_, _| {
                event_enumeration_complete.store(true, Ordering::Release);
                Ok(())
            });
        let enumeration_completed_token = match watcher.EnumerationCompleted(&enumeration_completed)
        {
            Ok(token) => token,
            Err(error) => {
                let _ = watcher.RemoveLineRemoved(line_removed_token);
                let _ = watcher.RemoveLineAdded(line_added_token);
                return Err(error);
            }
        };

        if let Err(error) = watcher.Start() {
            let _ = watcher.RemoveEnumerationCompleted(enumeration_completed_token);
            let _ = watcher.RemoveLineRemoved(line_removed_token);
            let _ = watcher.RemoveLineAdded(line_added_token);
            return Err(error);
        }

        let mut lines = Vec::<(GUID, PhoneLine)>::new();
        let mut watcher_stopped = false;
        while !stop.is_stopped() {
            match watcher.Status() {
                Ok(status) if status == PhoneLineWatcherStatus::Stopped => {
                    watcher_stopped = true;
                    break;
                }
                Err(_) => {
                    watcher_stopped = true;
                    break;
                }
                _ => {}
            }
            if enumeration_complete.load(Ordering::Acquire)
                && synchronize_phone_lines(&line_ids, &mut lines)
            {
                if let Some(calls) = observe_phone_calls(&lines) {
                    mute_state.set_external(select_external_mute_state(&calls));
                }
            }

            if stop.wait(EXTERNAL_CALL_POLL_INTERVAL) {
                break;
            }
        }

        let _ = watcher.Stop();
        let _ = watcher.RemoveEnumerationCompleted(enumeration_completed_token);
        let _ = watcher.RemoveLineRemoved(line_removed_token);
        let _ = watcher.RemoveLineAdded(line_added_token);
        Ok(watcher_stopped)
    }

    fn synchronize_phone_lines(
        line_ids: &Mutex<Vec<GUID>>,
        lines: &mut Vec<(GUID, PhoneLine)>,
    ) -> bool {
        let Ok(ids) = line_ids.lock().map(|ids| ids.clone()) else {
            return false;
        };
        lines.retain(|(id, _)| ids.contains(id));

        let mut complete = true;
        for id in ids {
            if lines.iter().any(|(known_id, _)| known_id == &id) {
                continue;
            }
            match PhoneLine::FromIdAsync(id).and_then(|operation| operation.get()) {
                Ok(line) => lines.push((id, line)),
                Err(_) => complete = false,
            }
        }
        complete
    }

    fn observe_phone_calls(lines: &[(GUID, PhoneLine)]) -> Option<Vec<ObservedCallState>> {
        let mut observed = Vec::new();
        for (_, line) in lines {
            let is_teams = is_teams_line(line)?;
            if !is_teams {
                continue;
            }
            let result = line.GetAllActivePhoneCalls().ok()?;
            if result.OperationStatus().ok()? != PhoneLineOperationStatus::Succeeded {
                return None;
            }
            let calls = result.AllActivePhoneCalls().ok()?;
            for index in 0..calls.Size().ok()? {
                let call = calls.GetAt(index).ok()?;
                let status = call.Status().ok()?;
                let state = if status == PhoneCallStatus::Talking {
                    ObservedCallState::Talking {
                        muted: call.IsMuted().ok()?,
                    }
                } else if status == PhoneCallStatus::Held {
                    ObservedCallState::Held {
                        muted: call.IsMuted().ok()?,
                    }
                } else {
                    ObservedCallState::Other
                };
                observed.push(state);
            }
        }
        Some(observed)
    }

    fn is_teams_line(line: &PhoneLine) -> Option<bool> {
        let display_name = line.DisplayName().ok()?;
        let network_name = line.NetworkName().ok()?;
        Some(
            [display_name, network_name]
                .iter()
                .any(|name| is_teams_name(&name.to_string_lossy())),
        )
    }

    pub(super) fn is_teams_name(name: &str) -> bool {
        name.split(|character: char| !character.is_ascii_alphanumeric())
            .any(|word| word.eq_ignore_ascii_case("teams"))
    }

    fn emit_mute_changed(app: &AppHandle, meeting_id: &str, muted: bool) {
        let _ = app.emit(
            "microphone-mute-changed",
            serde_json::json!({
                "meetingId": meeting_id,
                "muted": muted,
            }),
        );
    }

    fn emit_warning(app: &AppHandle, meeting_id: &str, message: String) {
        let _ = app.emit(
            "call-mute-warning",
            serde_json::json!({
                "meetingId": meeting_id,
                "message": message,
            }),
        );
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(crate) struct TeamsMuteMonitor;

    impl TeamsMuteMonitor {
        pub(crate) fn start(
            _app: AppHandle,
            _meeting_id: String,
            _microphone_muted: Arc<AtomicBool>,
        ) -> Result<Self, String> {
            Err("Windows Call Mute is only available on Windows.".to_string())
        }
    }
}

pub(crate) use platform::TeamsMuteMonitor;

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::platform::is_teams_name;
    use super::{
        reconcile_external_mute_state, select_external_mute_state, ExternalMuteState,
        ObservedCallState, PublishedMuteState,
    };

    #[test]
    fn selects_only_talking_calls() {
        assert_eq!(
            select_external_mute_state(&[
                ObservedCallState::Other,
                ObservedCallState::Talking { muted: true },
            ]),
            ExternalMuteState::Unanimous(true)
        );
        assert_eq!(
            select_external_mute_state(&[ObservedCallState::Held { muted: false }]),
            ExternalMuteState::NoActiveCalls
        );
        assert_eq!(
            select_external_mute_state(&[
                ObservedCallState::Talking { muted: true },
                ObservedCallState::Held { muted: false },
            ]),
            ExternalMuteState::Unanimous(true)
        );
    }

    #[test]
    fn selects_only_a_unanimous_multiple_call_state() {
        assert_eq!(
            select_external_mute_state(&[
                ObservedCallState::Talking { muted: true },
                ObservedCallState::Held { muted: true },
            ]),
            ExternalMuteState::Unanimous(true)
        );
        assert_eq!(
            select_external_mute_state(&[
                ObservedCallState::Talking { muted: true },
                ObservedCallState::Talking { muted: false },
            ]),
            ExternalMuteState::Ambiguous
        );
        assert_eq!(
            select_external_mute_state(&[ObservedCallState::Other]),
            ExternalMuteState::NoActiveCalls
        );
    }

    #[test]
    fn external_state_is_cleared_after_the_call() {
        let mut state = PublishedMuteState::default();
        assert_eq!(
            reconcile_external_mute_state(&mut state, ExternalMuteState::Unanimous(true)),
            Some(true)
        );
        assert_eq!(
            reconcile_external_mute_state(&mut state, ExternalMuteState::NoActiveCalls),
            Some(false)
        );
    }

    #[test]
    fn ambiguous_external_state_keeps_the_last_effective_state() {
        let mut state = PublishedMuteState::default();
        assert_eq!(
            reconcile_external_mute_state(&mut state, ExternalMuteState::Unanimous(true)),
            Some(true)
        );
        assert_eq!(
            reconcile_external_mute_state(&mut state, ExternalMuteState::Ambiguous),
            None
        );
        assert!(state.published_muted);
    }

    #[cfg(windows)]
    #[test]
    fn identifies_teams_as_a_complete_line_name_word() {
        assert!(is_teams_name("Microsoft Teams"));
        assert!(is_teams_name("Teams (work or school)"));
        assert!(!is_teams_name("Contoso Teamspace"));
    }
}
