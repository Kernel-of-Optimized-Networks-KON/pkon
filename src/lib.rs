//! Nanoscope: a small rig for testing lossless image compression ideas honestly.
pub mod canon;
pub mod codec;
pub mod corpus;
pub mod envinfo;
pub mod ext;
pub mod ellie;
pub mod ulisa;
pub mod lisa;
/// Compatibility alias for the pre-rename module path `nanoscope::praw` (PRAW was the former name of LISA).
pub use lisa as praw;
pub mod pkonfile;
pub mod rangecoder;
pub mod pkon;
pub mod dng;
pub mod integrity;
pub mod verify;
pub mod lisa_cloud;
pub mod lisa_mixer;
pub mod lisa_rp;
pub mod lisa_pcq;
pub mod lisa_or128;
pub mod lisa_or128_table;
pub mod lisa_or128_tree;
pub mod lisa_or128_tree_data;
pub mod lisa_pcq_table;
pub mod lisa_or128_tree_small_data;
pub mod lisa_bayer_plane;
pub mod lisa_frame;
