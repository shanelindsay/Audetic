pub mod audio_stream_manager;
pub mod ducking;
pub mod input_device;
pub mod level_monitor;
pub mod recording_machine;

pub use audio_stream_manager::AudioStreamManager;
pub use ducking::AudioDuckingController;
pub use input_device::{
    available_input_device_names, best_available_input_device_name,
    best_available_input_device_name_with_preference, preferred_input_device_name,
    select_input_device, select_input_device_any_host, select_input_device_any_host_with_preference,
    selected_input_device_name,
};
pub use level_monitor::spawn_idle_level_monitor;
pub use recording_machine::{
    BehaviorOptions, CompletedJob, JobOptions, RecordingMachine, RecordingPhase, RecordingStatus,
    RecordingStatusHandle, ToggleResult,
};
