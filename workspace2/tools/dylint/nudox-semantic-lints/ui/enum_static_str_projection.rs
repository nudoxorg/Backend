#![allow(dead_code)]

enum Stage {
    Parse,
    Lower,
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Parse => "parse",
        Stage::Lower => "lower",
    }
}

impl Stage {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Lower => "lower",
        }
    }
}

enum ExternalStage {
    Parse,
}

trait ExternalName {
    fn wire_name(self) -> &'static str;
}

impl ExternalName for ExternalStage {
    fn wire_name(self) -> &'static str {
        "parse"
    }
}

fn dynamic_label(stage: Stage, suffix: &str) -> String {
    format!("{}-{suffix}", stage.wire_name())
}

fn main() {}
