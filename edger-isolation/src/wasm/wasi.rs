//! WASI capability configuration stub (wasmtime standalone path — see spike.md).

/// WASI sandbox capabilities for Wasm workers (defaults deny-all).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WasiConfig {
    pub allow_env: bool,
    pub allow_stdio: bool,
    pub allow_net: bool,
    pub allow_fs_read: bool,
    pub allow_fs_write: bool,
}

impl WasiConfig {
    pub fn deny_all() -> Self {
        Self::default()
    }

    pub fn is_restricted(&self) -> bool {
        !self.allow_env
            && !self.allow_stdio
            && !self.allow_net
            && !self.allow_fs_read
            && !self.allow_fs_write
    }
}
