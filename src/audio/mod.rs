pub mod audio_stream_manager;
pub mod ducking;
pub mod level_monitor;
pub mod recording_machine;

pub use audio_stream_manager::AudioStreamManager;
pub use ducking::AudioDuckingController;
pub use level_monitor::spawn_idle_level_monitor;
pub use recording_machine::{
    BehaviorOptions, CompletedJob, JobOptions, RecordingMachine, RecordingPhase, RecordingStatus,
    RecordingStatusHandle, ToggleResult,
};
