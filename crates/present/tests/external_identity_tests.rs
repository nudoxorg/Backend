use super::*;

#[test]
fn external_operands_keep_captured_titles_and_distinct_exact_coordinates() {
    for display in ["malloc", "外部::χ", "external semantic target"] {
        let identities = [
            symbol_key("first foreign endpoint"),
            symbol_key("second foreign endpoint"),
        ];
        let mut coordinates = Vec::new();
        for symbol in identities {
            let coordinate = format!(
                "/abs/polyglot::external::symbol:{}::{display}",
                backend_library::encode_id(symbol.as_bytes())
            );
            let row = backend_library::Row::new(RowId::Symbol(symbol), basis(), &coordinate);
            let record = crate::record::Record::from_row(&row);
            let identity = record.identity();
            assert_eq!(identity.shape(), IdentityShape::External);
            assert_eq!(identity.coordinate().as_str(), coordinate);
            assert_eq!(identity.trail().joined(), display);
            assert_eq!(identity.trail_within(None), format!("polyglot › {display}"));
            assert_eq!(identity.language(), Language::Unknown);
            assert_eq!(identity.path(), None);
            assert_eq!(identity.line(), None);
            assert_eq!(record.kind(), None);
            assert_eq!(record.signature(), None);
            let dto = crate::dto::IdentityDto::new(identity);
            assert_eq!(dto.coordinate, coordinate);
            assert_eq!(dto.trail, format!("polyglot › {display}"));
            assert_eq!(dto.shape, "external");
            coordinates.push(coordinate);
        }
        assert_ne!(coordinates[0], coordinates[1]);
    }
}

#[test]
fn external_display_parser_preserves_legacy_and_unrecognized_named_trails() {
    for tail in ["libc::malloc", "symbol:abc::malloc", "symbol:ZZZZ::malloc"] {
        let coordinate = format!("/abs/polyglot::external::{tail}");
        let identity = Identity::parse(&coordinate);
        assert_eq!(identity.coordinate().as_str(), coordinate);
        assert_eq!(identity.trail().joined(), tail);
    }
}
