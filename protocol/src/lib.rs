//! The GameStream wire formats that the Sandom GameStream server and client
//! share: the ciphers, the pairing identities and signatures, the control
//! messages, the input events and the video packets. The formats come from
//! Moonshine (BSD 2-Clause); each module names its source.

pub mod control;
pub mod crypto;
pub mod input;
pub mod pairing;
pub mod video;
