//! Real worker framing through the existing reaping process supervisor.
use backend_compile::{
    Cancellation, ExecutableIdentity, ProcessEnvironment, ProcessLimits, ProcessStdin,
    ProcessSupervisor, ProcessTerminal, SupervisedCommand,
};
use backend_frontend_stylo::{
    support_fingerprint,
    worker_protocol::{MAX_INPUT, MAX_OUTPUT, decode_response, encode_request},
};
use backend_semantic::stylo_analysis::{
    StyloLimits, StyloParseConfig, StyloSourceKind, StyloSourceRole,
};
use std::{path::PathBuf, time::Duration};

fn config() -> StyloParseConfig {
    StyloParseConfig {
        source_kind: StyloSourceKind::StandaloneUtf8,
        base_url: "https://example.invalid/x.css".into(),
        author_origin: true,
        no_quirks: true,
        allow_imports: false,
        limits: StyloLimits {
            input_bytes: 4096,
            rules: 128,
            depth: 16,
            records: 256,
            diagnostics: 32,
            text_bytes: 65_536,
        },
    }
}
fn make_command(input: Vec<u8>) -> (SupervisedCommand, ExecutableIdentity) {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_backend-stylo-worker"));
    let identity = ExecutableIdentity::from_path(&path).unwrap();
    let limits = ProcessLimits::new(
        MAX_OUTPUT + 12,
        64 * 1024,
        Duration::from_secs(30),
        MAX_OUTPUT + 12 + 64 * 1024,
    )
    .unwrap()
    .with_input_bytes_limit(MAX_INPUT)
    .unwrap();
    let command = SupervisedCommand::new(
        path,
        Vec::new(),
        std::env::current_dir().unwrap(),
        ProcessEnvironment::new(Vec::new()).unwrap(),
        limits,
    )
    .unwrap()
    .with_stdin(ProcessStdin::bytes(input))
    .unwrap();
    assert_eq!(command.executable_identity(), Some(&identity));
    (command, identity)
}
#[test]
fn worker_roundtrip_is_reaped_and_content_bound() {
    let source = "\u{feff}/* π😀 */\r\n./**/hud\\:menu { --x: 1; --x: 2; }".as_bytes();
    let config = config();
    let input = encode_request(source, &config).unwrap();
    let (command, _) = make_command(input.clone());
    let receipt = ProcessSupervisor::new(command).run().unwrap();
    assert_eq!(receipt.terminal(), ProcessTerminal::Success);
    assert!(receipt.reaped());
    let facts = decode_response(receipt.stdout(), source, &config, support_fingerprint()).unwrap();
    assert!(
        facts
            .occurrences
            .iter()
            .any(|s| s.role == StyloSourceRole::ClassName && s.raw == r"hud\:menu")
    );
    assert_eq!(
        facts
            .occurrences
            .iter()
            .filter(|s| s.role == StyloSourceRole::CustomPropertyDeclarationName && s.raw == "--x")
            .count(),
        2
    );
    assert!(decode_response(receipt.stdout(), b"changed", &config, support_fingerprint()).is_err());
    let (cancelled, handle) = Cancellation::new();
    handle.cancel();
    let (command, _) = make_command(input);
    assert!(
        ProcessSupervisor::new(command)
            .run_with_cancellation(&cancelled)
            .is_err()
    );
}

#[test]
fn embedded_fragment_worker_reaps_and_maps_to_original_host() {
    use backend_compile::VerifiedSourceText;
    use backend_frontend_stylo::worker_protocol::{
        decode_fragment_response, encode_fragment_request,
    };
    let text = "\u{feff}<!-- π😀 -->\r\n<style>\u{feff} {} .menu { --x: 1; }</style>";
    let verified = VerifiedSourceText::capture(text.as_bytes()).unwrap();
    let host = verified.extent(0..text.len()).unwrap();
    let start = text.find("\u{feff} {} .menu").unwrap();
    let end = start + "\u{feff} {} .menu { --x: 1; }".len();
    let mut config = config();
    config.source_kind = StyloSourceKind::EmbeddedUnicodeFragment;
    let input = encode_fragment_request(&host, start..end, &config).unwrap();
    let (command, _) = make_command(input);
    let receipt = ProcessSupervisor::new(command).run().unwrap();
    assert_eq!(receipt.terminal(), ProcessTerminal::Success);
    assert!(receipt.reaped());
    let output = decode_fragment_response(
        receipt.stdout(),
        &host,
        start..end,
        &config,
        support_fingerprint(),
    )
    .unwrap();
    assert!(!output.analysis().utf8_bom_removed);
    let index = output
        .analysis()
        .occurrences
        .iter()
        .position(|o| o.raw == "menu")
        .unwrap();
    let extent = output.host_occurrence(index).unwrap();
    assert_eq!(&text[extent.byte_range()], "menu");
}
