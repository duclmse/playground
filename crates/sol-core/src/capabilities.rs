//! Host authority is runtime configuration, never a source-language mode.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub package: bool,
    pub filesystem: bool,
    pub process: bool,
    pub environment: bool,
    pub clock: bool,
    pub locale: bool,
    pub native_modules: bool,
    pub debug: bool,
}

impl Capabilities {
    pub const SANDBOX: Self = Self {
        package: false,
        filesystem: false,
        process: false,
        environment: false,
        clock: false,
        locale: false,
        native_modules: false,
        debug: false,
    };

    pub const NATIVE_CLI: Self = Self {
        package: true,
        filesystem: true,
        process: true,
        environment: true,
        clock: true,
        locale: true,
        native_modules: true,
        debug: true,
    };
}
