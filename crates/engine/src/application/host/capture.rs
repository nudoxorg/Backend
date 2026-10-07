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
}
