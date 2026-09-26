//! Roslyn authority: bare identifiers that resolve to properties emit field-read
//! and field-write rows the same way bare field identifiers do.

use backend_frontend_csharp::legacy::{
    CSharpImage, Declaration, DeclarationKind, ReferenceTag, ResolvedReference, probe_dotnet,
    probe_dotnet_path,
};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = r#"namespace demo;

public record Widget(int Count)
{
    public int Double() => Count;
}

public class Box
{
    public int Count { get; set; }
    public int Read() => Count;
    public void Write() { Count = 1; }
    public int Local() { int Count = 2; return Count; }
    public int Qualified() => this.Count;
}
"#;

fn image(bytes: &[u8]) -> Result<CSharpImage<'_>, Box<dyn Error>> {
    CSharpImage::open(bytes).map_err(|error| format!("image rejected: {error:?}").into())
}

fn dotnet() -> Result<PathBuf, Box<dyn Error>> {
    match probe_dotnet() {
        Ok(tool) => Ok(tool),
        Err(_) => probe_dotnet_path(PathBuf::from("/home/ubuntu/.dotnet/dotnet"))
            .map_err(|error| format!("dotnet unavailable: {error:?}").into()),
    }
}

fn publish_oracle(dotnet: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let helper_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/legacy/helper");
    let publish = std::env::temp_dir().join(format!(
        "nudox-csharp-property-read-publish-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    fs::create_dir_all(&publish)?;
    let intermediate = publish.join("obj");
    let mut intermediate_arg = OsString::from("-p:BaseIntermediateOutputPath=");
    intermediate_arg.push(&intermediate);
    intermediate_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let output_base = publish.join("bin");
    let mut output_base_arg = OsString::from("-p:BaseOutputPath=");
    output_base_arg.push(&output_base);
    output_base_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let restored = Command::new(dotnet)
        .args(["restore", "oracle.csproj", "--locked-mode", "--nologo"])
        .arg(&intermediate_arg)
        .current_dir(&helper_dir)
        .status()?;
    assert!(restored.success(), "locked oracle restore failed: {restored}");
    let published = Command::new(dotnet)
        .args([
            "publish",
            "oracle.csproj",
            "-c",
            "Release",
            "--nologo",
            "--no-restore",
            "-p:UseSharedCompilation=false",
        ])
        .arg(&intermediate_arg)
        .arg(&output_base_arg)
        .arg("-o")
        .arg(&publish)
        .current_dir(&helper_dir)
        .status()?;
    assert!(published.success(), "oracle publish failed: {published}");
    Ok(publish.join("oracle.dll"))
}

fn run_oracle(
    dotnet: &Path,
    helper: &Path,
    root: &Path,
    binding: &Path,
    out: &Path,
) -> Result<(), Box<dyn Error>> {
    let status = Command::new(dotnet)
        .args([
            helper.to_str().ok_or("helper path")?,
            "--mode",
            "source",
            "--root",
            root.to_str().ok_or("root path")?,
            "--authority-image",
            "--source-binding",
            binding.to_str().ok_or("source path")?,
            "--out",
            out.to_str().ok_or("output path")?,
        ])
        .status()?;
    assert!(status.success(), "oracle run failed: {status}");
    Ok(())
}

fn owner_type_name<'a>(
    declarations: &'a [Declaration<'a>],
    declaration: &Declaration<'a>,
) -> Option<&'a [u8]> {
    declaration
        .owner
        .and_then(|owner| declarations.get(owner as usize))
        .map(|row| row.name.bytes)
}

fn find_type<'a>(
    declarations: &'a [Declaration<'a>],
    name: &[u8],
    kind: DeclarationKind,
) -> Option<&'a Declaration<'a>> {
    declarations
        .iter()
        .find(|declaration| declaration.kind == kind && declaration.name.bytes == name)
}

fn find_member<'a>(
    declarations: &'a [Declaration<'a>],
    name: &[u8],
    kind: DeclarationKind,
    owner_name: &str,
) -> Option<&'a Declaration<'a>> {
    declarations.iter().find(|declaration| {
        declaration.kind == kind
            && declaration.name.bytes == name
            && owner_type_name(declarations, declaration) == Some(owner_name.as_bytes())
    })
}

fn declaration_index(declarations: &[Declaration<'_>], declaration: &Declaration<'_>) -> u32 {
    declarations
        .iter()
        .position(|row| std::ptr::eq(row, declaration))
        .expect("declaration index") as u32
}

#[test]
fn bare_property_identifiers_emit_field_read_and_write_rows() -> Result<(), Box<dyn Error>> {
    let dotnet = dotnet()?;
    let helper = publish_oracle(&dotnet)?;
    let root = std::env::temp_dir().join(format!(
        "nudox-csharp-property-read-root-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    fs::create_dir_all(&root)?;
    let binding = root.join("PropertyRead.cs");
    fs::write(&binding, SOURCE)?;
    let output = root.join("authority.ncaimg");
    run_oracle(&dotnet, &helper, &root, &binding, &output)?;
    let bytes = fs::read(&output)?;
    let authority = image(&bytes)?;
    let source = SOURCE.as_bytes();
    let expected_source: [u8; 32] = Sha256::digest(source).into();
    assert_eq!(authority.source_digest(), expected_source);

    let declarations: Vec<_> = authority.declarations().collect::<Result<_, _>>()?;
    let references: Vec<_> = authority.references().collect::<Result<_, _>>()?;

    let widget = find_type(&declarations, b"Widget", DeclarationKind::Record)
        .ok_or("Widget record missing")?;
    let widget_row = declaration_index(&declarations, widget);
    let widget_count = find_member(&declarations, b"Count", DeclarationKind::Property, "Widget")
        .ok_or("record property Count on Widget missing")?;
    let box_count = find_member(&declarations, b"Count", DeclarationKind::Property, "Box")
        .ok_or("auto-property Count on Box missing")?;
    let double_method = find_member(&declarations, b"Double", DeclarationKind::Method, "Widget")
        .ok_or("method Double missing")?;
    let read_method = find_member(&declarations, b"Read", DeclarationKind::Method, "Box")
        .ok_or("method Read missing")?;
    let write_method = find_member(&declarations, b"Write", DeclarationKind::Method, "Box")
        .ok_or("method Write missing")?;
    let local_method = find_member(&declarations, b"Local", DeclarationKind::Method, "Box")
        .ok_or("method Local missing")?;
    let qualified_method =
        find_member(&declarations, b"Qualified", DeclarationKind::Method, "Box")
            .ok_or("method Qualified missing")?;

    let widget_count_row = declaration_index(&declarations, widget_count);
    let box_count_row = declaration_index(&declarations, box_count);
    let double_row = declaration_index(&declarations, double_method);
    let read_row = declaration_index(&declarations, read_method);
    let write_row = declaration_index(&declarations, write_method);
    let local_row = declaration_index(&declarations, local_method);
    let qualified_row = declaration_index(&declarations, qualified_method);

    let record_reads = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::FieldRead
                && reference.spelling.bytes == b"Count"
                && reference.owner == double_row
                && reference.target == Some(widget_count_row)
        })
        .collect::<Vec<_>>();
    if record_reads.len() != 1 {
        return Err(format!(
            "expected one record FieldRead (owner Double, target Widget.Count property), found {}: {}",
            record_reads.len(),
            describe_matching(&references, ReferenceTag::FieldRead, double_row)
        )
        .into());
    }
    let record_target = authority.declaration(widget_count_row)?;
    if record_target.kind != DeclarationKind::Property {
        return Err(format!(
            "record Count target kind {:?}, expected Property",
            record_target.kind
        )
        .into());
    }
    if record_target.owner != Some(widget_row) {
        return Err("record Count target owner is not Widget".into());
    }

    let auto_reads = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::FieldRead
                && reference.spelling.bytes == b"Count"
                && reference.owner == read_row
                && reference.target == Some(box_count_row)
        })
        .collect::<Vec<_>>();
    if auto_reads.len() != 1 {
        return Err(format!(
            "expected one auto-property FieldRead (owner Read, target Box.Count), found {}: {}",
            auto_reads.len(),
            describe_matching(&references, ReferenceTag::FieldRead, read_row)
        )
        .into());
    }

    let writes = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::FieldWrite
                && reference.spelling.bytes == b"Count"
                && reference.owner == write_row
                && reference.target == Some(box_count_row)
        })
        .collect::<Vec<_>>();
    if writes.len() != 1 {
        return Err(format!(
            "expected one FieldWrite (owner Write, target Box.Count), found {}: {}",
            writes.len(),
            describe_matching(&references, ReferenceTag::FieldWrite, write_row)
        )
        .into());
    }

    let local_field_refs = references
        .iter()
        .filter(|reference| {
            reference.owner == local_row
                && (reference.kind == ReferenceTag::FieldRead
                    || reference.kind == ReferenceTag::FieldWrite)
        })
        .collect::<Vec<_>>();
    if !local_field_refs.is_empty() {
        return Err(format!(
            "local Count must not emit field references (found {}): {}",
            local_field_refs.len(),
            describe_refs(&local_field_refs)
        )
        .into());
    }

    let qualified_member_access = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MemberAccess
                && reference.owner == qualified_row
                && reference.spelling.bytes == b"Count"
        })
        .collect::<Vec<_>>();
    if qualified_member_access.len() != 1 {
        return Err(format!(
            "expected one MemberAccess for this.Count (owner Qualified), found {}",
            qualified_member_access.len()
        )
        .into());
    }
    let qualified_field_reads = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::FieldRead && reference.owner == qualified_row
        })
        .collect::<Vec<_>>();
    if !qualified_field_reads.is_empty() {
        return Err(format!(
            "this.Count must not also emit FieldRead (found {}): {}",
            qualified_field_reads.len(),
            describe_refs(&qualified_field_reads)
        )
        .into());
    }

    let field_reads = references
        .iter()
        .filter(|reference| reference.kind == ReferenceTag::FieldRead)
        .count();
    if field_reads != 2 {
        return Err(format!(
            "expected exactly two FieldRead rows, found {}",
            field_reads
        )
        .into());
    }

    Ok(())
}

fn describe_matching(
    references: &[ResolvedReference<'_>],
    tag: ReferenceTag,
    owner: u32,
) -> String {
    references
        .iter()
        .filter(|reference| reference.kind == tag && reference.owner == owner)
        .map(|reference| describe_ref(reference))
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_refs(references: &[&ResolvedReference<'_>]) -> String {
    references
        .iter()
        .map(|reference| describe_ref(reference))
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_ref(reference: &ResolvedReference<'_>) -> String {
    format!(
        "tag={:?} owner={} target={} spelling={}",
        reference.kind,
        reference.owner,
        reference
            .target
            .map(|target| target.to_string())
            .unwrap_or_else(|| "absent".to_owned()),
        String::from_utf8_lossy(reference.spelling.bytes)
    )
}
