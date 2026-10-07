//! One immutable set of ambient launch inputs for shared compiler selection.

use super::{LocalHostEnvironment, LocalHostVariable};
use std::ffi::OsString;

/// Frozen launch inputs, before installed paths are selected and closed.
///
/// This value is for admission only. It retains PATH and Go cache inputs so every
/// local service surface can use the same selection policy; compiler children
/// receive only the resulting canonical closed snapshot.
#[derive(Clone, Debug)]
pub struct CapturedLocalHostEnvironment {
    values: Vec<(LocalHostVariable, OsString)>,
    search_path: Option<OsString>,
    go_module_cache: Option<OsString>,
    go_path: Option<OsString>,
    cargo_home: Option<OsString>,
    user_profile: Option<OsString>,
}

impl CapturedLocalHostEnvironment {
    /// Reads each typed launch input once without discovery, probing or mutations.
    #[must_use]
    pub fn capture(environment: &impl LocalHostEnvironment) -> Self {
        Self {
            values: LocalHostVariable::ALL
                .into_iter()
                .filter_map(|variable| environment.value(variable).map(|value| (variable, value)))
                .collect(),
            search_path: environment.search_path(),
            go_module_cache: environment.go_module_cache(),
            go_path: environment.go_path(),
            cargo_home: environment.cargo_home(),
            user_profile: environment.user_profile(),
        }
    }
}

impl LocalHostEnvironment for CapturedLocalHostEnvironment {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        self.values
            .iter()
            .find(|(key, _)| *key == variable)
            .map(|(_, value)| value.clone())
    }

    fn search_path(&self) -> Option<OsString> {
        self.search_path.clone()
    }
    fn go_module_cache(&self) -> Option<OsString> {
        self.go_module_cache.clone()
    }
    fn go_path(&self) -> Option<OsString> {
        self.go_path.clone()
    }
    fn cargo_home(&self) -> Option<OsString> {
        self.cargo_home.clone()
    }
    fn user_profile(&self) -> Option<OsString> {
        self.user_profile.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct ChangingEnvironment(Cell<usize>);
    impl LocalHostEnvironment for ChangingEnvironment {
        fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
            if variable != LocalHostVariable::NudoxTypeScriptNode {
                return None;
            }
            let read = self.0.get();
            self.0.set(read + 1);
            Some(format!("/launch/node-{read}").into())
        }
        fn search_path(&self) -> Option<OsString> {
            Some("/launch/bin".into())
        }
    }

    #[test]
    fn repeated_discovery_reads_cannot_change_the_frozen_launch_inputs() {
        let source = ChangingEnvironment(Cell::new(0));
        let captured = CapturedLocalHostEnvironment::capture(&source);
        assert_eq!(source.0.get(), 1);
        assert_eq!(
            captured.value(LocalHostVariable::NudoxTypeScriptNode),
            Some("/launch/node-0".into())
        );
        assert_eq!(
            captured.value(LocalHostVariable::NudoxTypeScriptNode),
            Some("/launch/node-0".into())
        );
        assert_eq!(captured.search_path(), Some("/launch/bin".into()));
        assert_eq!(captured.value(LocalHostVariable::NudoxGoOracle), None);
        assert_eq!(source.0.get(), 1);
    }

    #[test]
    #[ignore = "requires the pinned dependency-installed real TypeScript application and installed host SDK"]
    fn actual_project_sdk_precedes_the_frozen_installed_global_sdk() {
        use super::super::{
            ClosedLocalHostEnvironmentSnapshot, LocalCompilerHost, LocalHostDiscovery,
            ProcessHostEnvironment,
        };
        use crate::application::typescript_host::TypeScriptProjectHost;
        use std::path::{Path, PathBuf};

        struct Closed(ClosedLocalHostEnvironmentSnapshot);
        impl LocalHostEnvironment for Closed {
            fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
                self.0
                    .path(variable)
                    .map(|path| path.as_os_str().to_owned())
            }
        }
        for variable in [
            "NUDOX_TSC",
            "NUDOX_TYPESCRIPT_NODE",
            "NUDOX_TYPESCRIPT_MODULE_ROOT",
        ] {
            assert!(
                std::env::var_os(variable).is_none(),
                "default gate cannot configure {variable}"
            );
        }
        let root = std::fs::canonicalize(PathBuf::from(
            std::env::var_os("NUDOX_SETUP_APPLICATION_ROOT")
                .expect("pinned declared-dependency application"),
        ))
        .expect("application root");
        let selected =
            LocalCompilerHost::new(ProcessHostEnvironment, LocalHostDiscovery::InstalledTools)
                .capture_installed_selection()
                .expect("capture actual installed host");
        let frozen = LocalCompilerHost::new(
            Closed(selected.snapshot().clone()),
            LocalHostDiscovery::ClosedSnapshot,
        );
        let home = selected.snapshot().path(LocalHostVariable::Home);
        let tuple = frozen
            .typescript_host_selection(home)
            .expect("closed canonical host tuple");
        let global_compiler = tuple.compiler.clone().expect("installed global SDK");
        assert!(!tuple.compiler_explicit);
        let node = tuple.node.expect("canonical installed Node");
        let project = TypeScriptProjectHost::new_with_node_origin(
            None,
            Some(node.path.clone()),
            Some(node.origin),
            home.map(Path::to_path_buf),
            None,
            None,
            crate::application::ToolchainProbeLimits::new(
                super::super::VERSION_PROBE_TIMEOUT,
                super::super::nonzero(super::super::VERSION_PROBE_STREAM_BYTES),
            )
            .expect("host probe limits"),
        )
        .with_installed_default(Some(global_compiler.clone()), tuple.module_root);
        let admitted = project
            .admit(&root)
            .expect("actual project SDK admission")
            .expect("project SDK available");
        let inputs = admitted.inputs();
        let local_compiler = std::fs::canonicalize(root.join("node_modules/typescript/bin/tsc"))
            .expect("actual PNPM project SDK compiler");
        assert_eq!(inputs.compiler_path, local_compiler.as_path());
        assert_ne!(
            inputs.compiler_path,
            global_compiler.as_path(),
            "installed global SDK is a fallback"
        );
        assert_eq!(inputs.node_path, node.path.as_path());
        assert_eq!(inputs.package_root, root.as_path());
        assert_eq!(inputs.compiler_origin, crate::application::typescript_host::TypeScriptSelectionOrigin::ProjectLocalInstallation);
        assert_eq!(
            inputs.node_origin,
            crate::application::typescript_host::TypeScriptSelectionOrigin::InstalledHostSelection
        );
        assert!(String::from_utf8_lossy(inputs.compiler_version).contains("5.7.2"));
        assert!(
            !inputs.config_candidates.is_empty(),
            "actual project configuration is bound"
        );
        admitted
            .validate_current()
            .expect("unchanged source/config/SDK authority");
        let frontier = std::fs::read_to_string(
            std::env::var_os("NUDOX_SETUP_SOURCE_FRONTIER")
                .expect("exact original application source frontier"),
        )
        .expect("source frontier");
        let sources = frontier.lines().collect::<Vec<_>>();
        assert_eq!(sources.len(), 33);
        for relative in &sources {
            let path = root.join(relative);
            let captured = inputs
                .load_source(&path)
                .expect("source belongs to actual selected project");
            assert_eq!(captured.path.as_ref(), path.as_path());
            assert_eq!(
                captured.bytes.as_ref(),
                std::fs::read(&path).unwrap().as_slice()
            );
        }
        admitted
            .validate_current()
            .expect("all exact source members remain unchanged");
        let restarted = project
            .admit(&root)
            .expect("repeat project admission")
            .expect("same project SDK");
        assert_eq!(admitted.fingerprint, restarted.fingerprint);
        assert_eq!(
            admitted.resolved_toolchain().unwrap().invocation_identity(),
            restarted
                .resolved_toolchain()
                .unwrap()
                .invocation_identity(),
            "same actual SDK/Node retain the authority recipe"
        );
        assert_eq!(
            admitted
                .resolved_toolchain()
                .unwrap()
                .invocation_location_identity(),
            restarted
                .resolved_toolchain()
                .unwrap()
                .invocation_location_identity()
        );
        println!(
            "actual-project-sdk={} node={} host-fallback={} fingerprint={:?}",
            inputs.compiler_path.display(),
            inputs.node_path.display(),
            global_compiler.display(),
            admitted.fingerprint
        );
    }
}
