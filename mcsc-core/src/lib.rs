//! mcsc-core: layout detection, conversion, and modded-content scanning.
//! Pure filesystem + NBT logic, no GUI dependency.

pub mod backup;
pub mod convert;
pub mod detect;
pub mod level;
pub mod players;
pub mod progress;
pub mod region;
pub mod scan;

pub use backup::backup_world_dir;
pub use convert::{
    convert, convert_many, default_output_base, ConvertAction, ConvertManyReport, ConvertOptions,
    ConvertReport, TargetLayout,
};
pub use detect::{detect_layout, find_worlds, Detection, FoundWorld, Layout};
pub use players::{is_uuid, offline_uuid, read_usercache_names, UuidResolver};
pub use progress::{Progress, ProgressReporter};
pub use scan::{scan_modded, ScanOptions, ScanReport};
