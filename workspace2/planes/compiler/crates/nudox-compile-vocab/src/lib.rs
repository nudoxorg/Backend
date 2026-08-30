#![no_std]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    RustSubset,
    TypeScriptSubset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Parse,
    LowerIr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontendError {
    UnsupportedStage { language: Language, stage: Stage },
}
