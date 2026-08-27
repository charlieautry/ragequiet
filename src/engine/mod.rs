pub mod features;
#[allow(dead_code)] // used from Task 7 on
pub mod baseline;
#[allow(dead_code)] // used from Task 7 on
pub mod vad;

pub const FRAME_SIZE: usize = 512;
pub const SAMPLE_RATE: u32 = 16_000;
