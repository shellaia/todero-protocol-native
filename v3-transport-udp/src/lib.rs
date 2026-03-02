//! v3-transport-udp
//!
//! Scaffold crate for Todero V3 protocol implementation.

pub const CRATE_NAME: &str = "v3-transport-udp";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_constant_is_set() {
        assert_eq!(CRATE_NAME, "v3-transport-udp");
    }
}
