//! Explicit selected-generation semantic segment hydration command.

use crate::options::{Format, Options};
use backend_client::{ClientError, LocalSemanticIndexClient};
use backend_engine::{FileStore, PackageReference};
use backend_present::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
use backend_replication::{
    FileSemanticRangeStore, HydrationCredits, IrHydrationPoll, SemanticRangeClientCheckpoint,
    SemanticRangeClientProgress, SemanticTargetKey, TransportLimits,
};
use backend_semantic::ir::{
    EmbeddingNormalization, EmbeddingPlaneIdentity, SemanticIrPlane, SemanticPlaneKind,
};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const FILESTORE_PACK_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_TRANSFER_BUDGET: u64 = 512 * 1024 * 1024;
const DEFAULT_RANGE_BUDGET: usize = 65_536;
const MAX_RANGE_BUDGET: usize = 1_000_000;
const MAX_CHECKPOINT_BYTES: u64 = 5 * 1024 * 1024;

struct Arguments {
    package: String,
    coordinate: String,
    profile: LanguageProfile,
    image_ordinal: u32,
    plane: SemanticPlaneKind,
    store: PathBuf,
    checkpoint: PathBuf,
    max_bytes: u64,
    max_ranges: usize,
}

/// Runs the explicit local selected-semantic hydration command.
pub(super) fn run(words: &[String], options: &Options) -> Result<String, Fault> {
    if words
        .first()
        .is_some_and(|word| matches!(word.as_str(), "help" | "--help" | "-h"))
        && words.len() == 1
    {
        return Ok(help_text().to_owned());
    }
    let args = parse(words)?;
    let target = target(&args)?;
    let workspace = backend_runtime::WorkspacePaths::discover(
        options.project().cloned(),
        options.workspace().cloned(),
        options.endpoint().cloned(),
    )
    .map_err(|error| endpoint_fault(&workspace_operand(options), error.to_string()))?;
    let endpoint = backend_runtime::ensure_locald(&workspace).map_err(|error| {
        endpoint_fault(&workspace.endpoint().to_string_lossy(), error.to_string())
    })?;
    let mut client = LocalSemanticIndexClient::connect(&endpoint, target)
        .map_err(|error| client_fault(&error, &args.package))?;
    let snapshot = client
        .fetch_selected_catalog()
        .map_err(|error| client_fault(&error, &args.package))?;
    let entry = snapshot
        .catalog()
        .entries()
        .iter()
        .find(|entry| entry.image().artifact_ordinal() == args.image_ordinal)
        .ok_or_else(|| {
            usage(
                "--image-ordinal",
                "choose an image ordinal present in the current selected catalog",
            )
        })?;
    let image = entry.image();
    let manifest = client
        .fetch_selected_manifest(image)
        .map_err(|error| client_fault(&error, &args.package))?;

    let store = FileStore::open(&args.store, FILESTORE_PACK_BYTES)
        .map_err(|error| endpoint_fault(&args.store.to_string_lossy(), format!("{error:?}")))?;
    let mut store = FileSemanticRangeStore::open(store, transport_limits())
        .map_err(|error| endpoint_fault(&args.store.to_string_lossy(), error))?;
    let full_image = client
        .fetch_selected_image(image, &store, args.max_bytes, args.max_ranges)
        .map_err(|error| client_fault(&error, &args.package))?;
    let have_ids = client
        .verified_local_segments(&manifest, image, args.plane, &mut store)
        .map_err(|error| client_fault(&error, &args.package))?;

    let checkpoint_bytes = read_checkpoint(&args.checkpoint)?;
    let (mut cursor, mut partial, mut poll) = match checkpoint_bytes {
        Some(bytes) => {
            let checkpoint = SemanticRangeClientCheckpoint::decode(&bytes).map_err(|error| {
                endpoint_fault(&args.checkpoint.to_string_lossy(), error.to_string())
            })?;
            let (cursor, coverage, poll) = client
                .resume_semantic_range(
                    &checkpoint,
                    &manifest,
                    &have_ids,
                    transport_limits(),
                    &mut store,
                )
                .map_err(|error| client_fault(&error, &args.package))?;
            (cursor, Some(coverage), poll)
        }
        None => {
            let mut cursor = client
                .new_cursor(&manifest, image, args.plane, &have_ids, transport_limits())
                .map_err(|error| client_fault(&error, &args.package))?;
            let poll = client
                .next_request(&mut cursor, None, HydrationCredits::new(1, 16 * 1024))
                .map_err(|error| client_fault(&error, &args.package))?;
            (cursor, None, poll)
        }
    };

    let mut transferred_bytes = full_image.transferred_bytes();
    let mut range_count = full_image.page_requests();
    let mut completed_segments = have_ids.len();
    loop {
        match poll {
            IrHydrationPoll::Request(request) => {
                let bytes = client
                    .requested_bytes(&request)
                    .map_err(|error| client_fault(&error, &args.package))?;
                range_count = range_count
                    .checked_add(1)
                    .ok_or_else(|| usage("--max-ranges", "range accounting overflowed"))?;
                transferred_bytes = transferred_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| usage("--max-bytes", "byte accounting overflowed"))?;
                if range_count > args.max_ranges || transferred_bytes > args.max_bytes {
                    return Err(usage(
                        "--max-bytes/--max-ranges",
                        "the selected image or plane exceeded this command's bounded transfer budget; rerun with larger explicit limits",
                    ));
                }
                match client
                    .request_and_accept(&mut cursor, &request, &mut store, transport_limits())
                    .map_err(|error| client_fault(&error, &args.package))?
                {
                    SemanticRangeClientProgress::Staged {
                        coverage,
                        checkpoint,
                    } => {
                        write_checkpoint(
                            &args.checkpoint,
                            &checkpoint.encode().map_err(|error| {
                                endpoint_fault(
                                    &args.checkpoint.to_string_lossy(),
                                    error.to_string(),
                                )
                            })?,
                        )?;
                        partial = Some(coverage);
                        poll = client
                            .next_request(
                                &mut cursor,
                                partial.as_ref(),
                                HydrationCredits::new(1, 16 * 1024),
                            )
                            .map_err(|error| client_fault(&error, &args.package))?;
                    }
                    SemanticRangeClientProgress::Complete(_) => {
                        completed_segments = completed_segments.saturating_add(1);
                        partial = None;
                        clear_checkpoint(&args.checkpoint)?;
                        poll = client
                            .next_request(&mut cursor, None, HydrationCredits::new(1, 16 * 1024))
                            .map_err(|error| client_fault(&error, &args.package))?;
                    }
                }
            }
            IrHydrationPoll::VerifyLocal(request) => {
                client
                    .verify_local_segment(&mut cursor, &request, &mut store)
                    .map_err(|error| client_fault(&error, &args.package))?;
                completed_segments = completed_segments.saturating_add(1);
                partial = None;
                clear_checkpoint(&args.checkpoint)?;
                poll = client
                    .next_request(&mut cursor, None, HydrationCredits::new(1, 16 * 1024))
                    .map_err(|error| client_fault(&error, &args.package))?;
            }
            IrHydrationPoll::NoCredits => {
                return Err(Fault::new(
                    FaultSlug::Transport,
                    Operand::Path(args.store.to_string_lossy().into_owned()),
                    Cause::new(
                        CauseSlug::Unproven,
                        "hydration was paused without a resumable range",
                    ),
                    Affordance::Retry,
                ));
            }
            IrHydrationPoll::Exhausted => break,
        }
    }

    let total_segments = manifest
        .plane(args.plane)
        .map_or(0, |plane| plane.segments().len());
    let local_generation = client
        .commit_local_generation(image, &manifest, args.plane, &mut store)
        .map_err(|error| client_fault(&error, &args.package))?;
    let local_ir_vcs_diff = match local_generation.previous_image() {
        None => serde_json::json!({ "state": "noBase" }),
        Some(base_image) => match store
            .semantic_diff_summary(client.target(), base_image, image)
            .map_err(|error| endpoint_fault(&args.store.to_string_lossy(), error))?
        {
            None => serde_json::json!({
                "state": "baseImageMissing",
                "baseGeneration": hex(base_image.semantic_generation().as_bytes()),
                "targetGeneration": hex(image.semantic_generation().as_bytes()),
            }),
            Some(diff) => serde_json::json!({
                "state": "computed",
                "beforeGeneration": hex(diff.before.as_bytes()),
                "afterGeneration": hex(diff.after.as_bytes()),
                "introducedEntities": diff.introduced_entities,
                "deletedEntities": diff.deleted_entities,
                "changedEntities": diff.changed_entities,
                "addedLinks": diff.added_links,
                "removedLinks": diff.removed_links,
                "changedLinks": diff.changed_links,
                "provenanceComparison": format!("{:?}", diff.provenance.comparison),
            }),
        },
    };
    let result = serde_json::json!({
        "target": &args.package,
        "coordinate": &args.coordinate,
        "profile": format!("{:?}", args.profile),
        "selectedRoot": hex(snapshot.selected_root()),
        "catalogRoot": hex(snapshot.selected_stamp().catalog_root().as_bytes()),
        "imageOrdinal": image.artifact_ordinal(),
        "fullImageIdentity": hex(full_image.image().identity().as_ref()),
        "fullImageBytes": full_image.image().view().as_ref().len(),
        "fullImageTransferBytes": full_image.transferred_bytes(),
        "fullImagePages": full_image.page_requests(),
        "manifestRoot": hex(image.manifest_root().as_bytes()),
        "semanticGeneration": hex(local_generation.semantic_generation().as_bytes()),
        "localGeneration": hex(local_generation.identity().as_bytes()),
        "previousLocalGeneration": local_generation.previous_identity().map(|id| hex(id.as_bytes())),
        "localIrVcsDiff": local_ir_vcs_diff,
        "plane": plane_label(args.plane),
        "totalSegments": total_segments,
        "verifiedSegments": completed_segments,
        "transferredBytes": transferred_bytes,
        "rangeRequests": range_count,
        "checkpoint": args.checkpoint.display().to_string(),
    });
    match options.format() {
        Format::Json => serde_json::to_string_pretty(&result)
            .map(|mut output| {
                output.push('\n');
                output
            })
            .map_err(|error| endpoint_fault("semantic-hydrate result", error.to_string())),
        Format::Human | Format::Markdown => Ok(format!(
            "Hydrated selected semantic plane {} for {}.\nSelected root: {}\nSemantic generation: {}\nLocal generation identity: {}\nFull NXFI image: {} bytes, {} fetched bytes in {} pages\nLocal IR-VCS comparison: {}\nSegments: {}/{} verified\nTransferred: {} bytes in {} ranges\nLocal store: {}\n",
            plane_label(args.plane),
            args.package,
            hex(snapshot.selected_root()),
            hex(local_generation.semantic_generation().as_bytes()),
            hex(local_generation.identity().as_bytes()),
            full_image.image().view().as_ref().len(),
            full_image.transferred_bytes(),
            full_image.page_requests(),
            match &local_ir_vcs_diff["state"] {
                serde_json::Value::String(state) if state == "computed" =>
                    "computed against prior local image",
                serde_json::Value::String(state) if state == "baseImageMissing" =>
                    "prior local image missing",
                _ => "no prior local image",
            },
            completed_segments,
            total_segments,
            transferred_bytes,
            range_count,
            args.store.display(),
        )),
    }
}

pub(crate) const fn help_text() -> &'static str {
    "Usage: backend [OPTIONS] semantic-hydrate --package REF --coordinate PKGURL --profile PROFILE --image-ordinal N --plane core|types|relations|occurrences|documentation|source-provenance|language-extensions|embeddings --store PATH [--checkpoint PATH] [--max-bytes N] [--max-ranges N]\n\n\
     Embeddings additionally require --model HEX --model-version HEX --tokenizer HEX --dimension N --normalization none|l2|mean-centered-l2|custom:HEX --toolchain HEX --recipe HEX.\n\
     The command binds to the current selected generation, reuses verified content-addressed segments already in the local CAS, and commits an atomic local generation head after the selected plane is complete. A checkpoint is exact-generation bound."
}

fn parse(words: &[String]) -> Result<Arguments, Fault> {
    let mut flags = BTreeMap::new();
    let mut at = 0;
    while at < words.len() {
        let flag = words[at]
            .strip_prefix("--")
            .ok_or_else(|| usage("semantic-hydrate", "expected named --option value pairs"))?;
        at += 1;
        let value = words
            .get(at)
            .ok_or_else(|| usage(flag, "requires a value"))?;
        if flags.insert(flag.to_owned(), value.clone()).is_some() {
            return Err(usage(flag, "may be specified only once"));
        }
        at += 1;
    }
    let known = [
        "package",
        "coordinate",
        "profile",
        "image-ordinal",
        "plane",
        "store",
        "checkpoint",
        "max-bytes",
        "max-ranges",
        "model",
        "model-version",
        "tokenizer",
        "dimension",
        "normalization",
        "toolchain",
        "recipe",
    ];
    if let Some(unknown) = flags.keys().find(|key| !known.contains(&key.as_str())) {
        return Err(usage(unknown.as_str(), "is not a semantic-hydrate option"));
    }
    let package = required(&flags, "package")?.to_owned();
    let coordinate = required(&flags, "coordinate")?.to_owned();
    let profile = LanguageProfile::try_from(required(&flags, "profile")?)
        .map_err(|_| usage("--profile", "use a canonical language profile spelling"))?;
    let image_ordinal = parse_u32(required(&flags, "image-ordinal")?, "--image-ordinal")?;
    let plane = parse_plane(&flags, profile)?;
    let store = PathBuf::from(required(&flags, "store")?);
    let checkpoint = flags.get("checkpoint").map_or_else(
        || store.with_extension("semantic-checkpoint"),
        |value| PathBuf::from(value.as_str()),
    );
    let max_bytes = flags
        .get("max-bytes")
        .map_or(Ok(DEFAULT_TRANSFER_BUDGET), |value| {
            parse_u64(value, "--max-bytes")
        })?;
    let max_ranges = flags
        .get("max-ranges")
        .map_or(Ok(DEFAULT_RANGE_BUDGET), |value| {
            parse_usize(value, "--max-ranges")
        })?;
    if store.as_os_str().is_empty() || checkpoint.as_os_str().is_empty() {
        return Err(usage("--store/--checkpoint", "paths must not be empty"));
    }
    if max_bytes == 0 || max_ranges == 0 || max_ranges > MAX_RANGE_BUDGET {
        return Err(usage(
            "--max-bytes/--max-ranges",
            "use a positive byte budget and a range budget no greater than one million",
        ));
    }
    Ok(Arguments {
        package,
        coordinate,
        profile,
        image_ordinal,
        plane,
        store,
        checkpoint,
        max_bytes,
        max_ranges,
    })
}

fn target(args: &Arguments) -> Result<SemanticTargetKey, Fault> {
    PackageReference::parse(args.package.clone())
        .map_err(|_| usage("--package", "use a canonical package reference"))?;
    let parsed_coordinate = PackageUrl::parse(args.coordinate.clone())
        .map_err(|_| usage("--coordinate", "use a canonical compiler package URL"))?;
    if parsed_coordinate.as_str() != args.coordinate {
        return Err(usage("--coordinate", "the package URL must be canonical"));
    }
    SemanticTargetKey::new(args.package.clone(), args.coordinate.clone(), args.profile).map_err(
        |_| {
            usage(
                "semantic-hydrate target",
                "the target fields exceed protocol bounds",
            )
        },
    )
}

fn parse_plane(
    flags: &BTreeMap<String, String>,
    profile: LanguageProfile,
) -> Result<SemanticPlaneKind, Fault> {
    let plane = required(flags, "plane")?;
    let kind = match plane {
        "core" => SemanticPlaneKind::Ir(SemanticIrPlane::Core),
        "types" => SemanticPlaneKind::Ir(SemanticIrPlane::Types),
        "relations" => SemanticPlaneKind::Ir(SemanticIrPlane::Relations),
        "occurrences" => SemanticPlaneKind::Ir(SemanticIrPlane::Occurrences),
        "documentation" => SemanticPlaneKind::Ir(SemanticIrPlane::Documentation),
        "source-provenance" => SemanticPlaneKind::Ir(SemanticIrPlane::SourceProvenance),
        "language-extensions" => {
            SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(profile))
        }
        "embeddings" => SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                fixed_hex(required(flags, "model")?, "--model")?,
                fixed_hex(required(flags, "model-version")?, "--model-version")?,
                fixed_hex(required(flags, "tokenizer")?, "--tokenizer")?,
                parse_u32(required(flags, "dimension")?, "--dimension")?,
                parse_normalization(required(flags, "normalization")?)?,
                fixed_hex(required(flags, "toolchain")?, "--toolchain")?,
                fixed_hex(required(flags, "recipe")?, "--recipe")?,
            )
            .map_err(|_| usage("--dimension", "embedding dimension must be positive"))?,
        ),
        _ => {
            return Err(usage("--plane", "choose an IR plane name or embeddings"));
        }
    };
    Ok(kind)
}

fn parse_normalization(value: &str) -> Result<EmbeddingNormalization, Fault> {
    match value {
        "none" => Ok(EmbeddingNormalization::None),
        "l2" => Ok(EmbeddingNormalization::L2),
        "mean-centered-l2" => Ok(EmbeddingNormalization::MeanCenteredL2),
        custom if custom.starts_with("custom:") => Ok(EmbeddingNormalization::Custom(fixed_hex(
            &custom[7..],
            "--normalization",
        )?)),
        _ => Err(usage(
            "--normalization",
            "choose none, l2, mean-centered-l2, or custom: followed by 64 hex digits",
        )),
    }
}

fn required<'a>(flags: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, Fault> {
    flags
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| usage(format!("--{name}"), "is required"))
}

fn parse_u32(value: &str, flag: &str) -> Result<u32, Fault> {
    value
        .parse()
        .map_err(|_| usage(flag, "use a positive integer"))
}

fn parse_u64(value: &str, flag: &str) -> Result<u64, Fault> {
    value
        .parse()
        .map_err(|_| usage(flag, "use a positive integer"))
}

fn parse_usize(value: &str, flag: &str) -> Result<usize, Fault> {
    value
        .parse()
        .map_err(|_| usage(flag, "use a positive integer"))
}

fn fixed_hex<const N: usize>(value: &str, flag: &str) -> Result<[u8; N], Fault> {
    if value.len() != N.saturating_mul(2) {
        return Err(usage(flag, "use the exact number of hex digits"));
    }
    let mut result = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_digit(pair[0]).ok_or_else(|| usage(flag, "use hexadecimal digits"))?;
        let low = hex_digit(pair[1]).ok_or_else(|| usage(flag, "use hexadecimal digits"))?;
        result[index] = high << 4 | low;
    }
    Ok(result)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn transport_limits() -> TransportLimits {
    TransportLimits {
        max_frame: 256 * 1024,
        max_chunk: 16 * 1024,
        ..TransportLimits::default()
    }
}

fn read_checkpoint(path: &Path) -> Result<Option<Vec<u8>>, Fault> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(endpoint_fault(&path.to_string_lossy(), error.to_string())),
    };
    let length = file
        .metadata()
        .map_err(|error| endpoint_fault(&path.to_string_lossy(), error.to_string()))?
        .len();
    if length > MAX_CHECKPOINT_BYTES {
        return Err(endpoint_fault(
            &path.to_string_lossy(),
            "checkpoint exceeds the 5 MiB admission bound".to_owned(),
        ));
    }
    let capacity = usize::try_from(length).map_err(|_| {
        endpoint_fault(
            &path.to_string_lossy(),
            "checkpoint length exceeds address space".to_owned(),
        )
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|_| {
        endpoint_fault(
            &path.to_string_lossy(),
            "checkpoint allocation failed".to_owned(),
        )
    })?;
    Read::take(&mut file, MAX_CHECKPOINT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| endpoint_fault(&path.to_string_lossy(), error.to_string()))?;
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err(endpoint_fault(
            &path.to_string_lossy(),
            "checkpoint exceeds the 5 MiB admission bound".to_owned(),
        ));
    }
    Ok(Some(bytes))
}

fn write_checkpoint(path: &Path, bytes: &[u8]) -> Result<(), Fault> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)
            .map_err(|error| endpoint_fault(&parent.to_string_lossy(), error.to_string()))?;
    }
    let temporary = path.with_extension("checkpoint.tmp");
    let mut file = File::create(&temporary)
        .map_err(|error| endpoint_fault(&temporary.to_string_lossy(), error.to_string()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| endpoint_fault(&temporary.to_string_lossy(), error.to_string()))?;
    fs::rename(&temporary, path)
        .map_err(|error| endpoint_fault(&path.to_string_lossy(), error.to_string()))
}

fn clear_checkpoint(path: &Path) -> Result<(), Fault> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(endpoint_fault(&path.to_string_lossy(), error.to_string())),
    }
}

fn client_fault(error: &ClientError, package: &str) -> Fault {
    Fault::from_client_error(error, Operand::Path(package.to_owned()))
}

fn endpoint_fault(path: &str, message: String) -> Fault {
    Fault::new(
        FaultSlug::Endpoint,
        Operand::Path(path.to_owned()),
        Cause::new(CauseSlug::Unreachable, message),
        Affordance::Retry,
    )
}

fn workspace_operand(options: &Options) -> String {
    options
        .workspace()
        .or_else(|| options.project())
        .map_or_else(
            || ".".to_owned(),
            |path| path.to_string_lossy().into_owned(),
        )
}

fn usage(argument: impl Into<String>, message: impl Into<String>) -> Fault {
    Fault::usage(argument, message)
}

fn plane_label(kind: SemanticPlaneKind) -> String {
    match kind {
        SemanticPlaneKind::Ir(plane) => format!("ir:{plane:?}"),
        SemanticPlaneKind::Embeddings(identity) => {
            format!("embeddings:{}", hex(identity.recipe()))
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
