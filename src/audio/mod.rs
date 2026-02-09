pub mod audio_stream_manager;
pub mod ducking;
pub mod input_device;
pub mod level_monitor;
pub mod recording_machine;

pub use audio_stream_manager::AudioStreamManager;
pub use ducking::AudioDuckingController;
pub use input_device::{
    best_available_input_device_name, preferred_input_device_name, select_input_device,
    select_input_device_any_host,
};
pub use level_monitor::spawn_idle_level_monitor;
pub use recording_machine::{
    BehaviorOptions, CompletedJob, JobOptions, RecordingMachine, RecordingPhase, RecordingStatus,
    RecordingStatusHandle, ToggleResult,
};
