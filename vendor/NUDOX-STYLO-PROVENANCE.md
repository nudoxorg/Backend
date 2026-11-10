# Native Stylo source and observer provenance

`stylo-native` is the complete published Stylo 0.23.0 crate from Servo source
revision `2dca34592ced37c6aa01733e94206a767194cf9d`, registry archive SHA-256
`84fe6be1e185e318b93be8f1e025667c196584a9bc9d5b7aca34bba4a07fb488`.
`selectors-native` is selectors 0.42.0 from the same revision, archive SHA-256
`284ac2879477575f13b22201f7e98aa33aacc765b790536808cf580fd79339f1`.
Both are MPL-2.0; their full license text is present in each directory.

The six locally modified upstream files are Stylo `lib.rs`,
`stylesheets/stylesheet.rs`, `stylesheets/rule_parser.rs`,
`properties/declaration_block.rs`, and selectors `lib.rs` and `parser.rs`.
Local `LICENSE` and `.gitattributes` files were added; Cargo's `.cargo-ok` cache
markers were omitted. The frontend support fingerprint includes the entire
vendored source trees and locked native dependency closure, as well as adapter,
schema, worker, compiler and target identity.

The observer's supported source subset is accepted top-level native style rules,
class-only selector lists and custom-property declaration names. It reports
unknown retained/overridden disposition and explicit partial coverage for
unobserved roles and nested or complex syntax. It does not evaluate cascade or
claim exact authored sites for native serialization. The reproducible original
archive checks, patches and fixture commands are in
`experiments/stylo-observer-proof/README.md`.
