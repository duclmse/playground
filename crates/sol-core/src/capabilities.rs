//! Host authority is runtime configuration, never a source-language mode.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub package: bool,
    pub filesystem: bool,
    pub process: bool,
    pub environment: bool,
    pub clock: bool,
    pub locale: bool,
    pub stdin: bool,
    pub stdout: bool,
    pub native_modules: bool,
    pub debug: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::SANDBOX
    }
}

impl Capabilities {
    pub const SANDBOX: Self = Self {
        package: false,
        filesystem: false,
        process: false,
        environment: false,
        clock: false,
        locale: false,
        stdin: false,
        stdout: false,
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
        stdin: true,
        stdout: true,
        native_modules: true,
        debug: true,
    };
}

#[cfg(test)]
mod tests {
    use super::Capabilities;

    #[test]
    fn sandbox_profile_denies_every_host_authority() {
        let capabilities = Capabilities::SANDBOX;
        assert_eq!(Capabilities::default(), capabilities);
        assert!(!capabilities.package);
        assert!(!capabilities.filesystem);
        assert!(!capabilities.process);
        assert!(!capabilities.environment);
        assert!(!capabilities.clock);
        assert!(!capabilities.locale);
        assert!(!capabilities.stdin);
        assert!(!capabilities.stdout);
        assert!(!capabilities.native_modules);
        assert!(!capabilities.debug);
    }

    #[test]
    fn native_cli_profile_enables_every_host_authority() {
        let capabilities = Capabilities::NATIVE_CLI;
        assert!(capabilities.package);
        assert!(capabilities.filesystem);
        assert!(capabilities.process);
        assert!(capabilities.environment);
        assert!(capabilities.clock);
        assert!(capabilities.locale);
        assert!(capabilities.stdin);
        assert!(capabilities.stdout);
        assert!(capabilities.native_modules);
        assert!(capabilities.debug);
    }
}
