//! Roslyn authority: compiler-resolved methods used as values emit method-group
//! rows distinct from invocations and member-access field reads.

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

const SOURCE: &str = r#"using System;
using System.Linq;
using static demo.Widget;
namespace demo;
public static class Widget {
    public static string Parse(string raw) => raw;
    public static int Length(string raw) => raw.Length;
}
public class Box {
    public string Parse(string raw) => raw;
    public static void Via(string[] items) {
        Func<string, string> bound = Widget.Parse;
        Func<string, int> qualified = Widget.Length;
        _ = items.Select(Widget.Parse);
        var called = Widget.Parse("x");
        var named = nameof(Widget.Parse);
    }
    public void Conditional(string[] items) {
        Box? box = this;
        Func<string, string>? grouped = box is null ? null : box.Parse;
        var called = box?.Parse("x");
        var selected = items?.Select(Parse);
        var wrapped = (box?.Parse("x"));
    }
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
        "nudox-csharp-method-group-publish-{}-{}",
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

fn span_after(source: &[u8], marker: &[u8], token: &[u8]) -> (u32, u32) {
    let base = source
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("marker");
    let slice = &source[base..];
    let at = slice
        .windows(token.len())
        .position(|window| window == token)
        .expect("token");
    let start = base + at;
    (
        u32::try_from(start).expect("start"),
        u32::try_from(start + token.len()).expect("end"),
    )
}

fn matching_method_groups<'a>(
    references: &'a [ResolvedReference<'a>],
    owner: u32,
    spelling: &[u8],
    target: u32,
) -> Vec<&'a ResolvedReference<'a>> {
    references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.owner == owner
                && reference.spelling.bytes == spelling
                && reference.target == Some(target)
        })
        .collect()
}

fn method_groups_at_span<'a>(
    references: &'a [ResolvedReference<'a>],
    owner: u32,
    start: u32,
    end: u32,
) -> Vec<&'a ResolvedReference<'a>> {
    references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.owner == owner
                && reference.start == start
                && reference.end == end
        })
        .collect()
}

fn invocations_owned_by<'a>(
    references: &'a [ResolvedReference<'a>],
    owner: u32,
) -> Vec<&'a ResolvedReference<'a>> {
    references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::Invocation && reference.owner == owner
        })
        .collect()
}

#[test]
fn compiler_resolved_method_groups_emit_distinct_reference_rows() -> Result<(), Box<dyn Error>> {
    let dotnet = dotnet()?;
    let helper = publish_oracle(&dotnet)?;
    let root = std::env::temp_dir().join(format!(
        "nudox-csharp-method-group-root-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    fs::create_dir_all(&root)?;
    let binding = root.join("MethodGroup.cs");
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

    let widget = find_type(&declarations, b"Widget", DeclarationKind::Class)
        .ok_or("Widget class missing")?;
    let parse_method = find_member(&declarations, b"Parse", DeclarationKind::Method, "Widget")
        .ok_or("Widget.Parse method missing")?;
    let length_method =
        find_member(&declarations, b"Length", DeclarationKind::Method, "Widget")
            .ok_or("Widget.Length method missing")?;
    let box_type = find_type(&declarations, b"Box", DeclarationKind::Class).ok_or("Box class missing")?;
    let box_parse_method = find_member(&declarations, b"Parse", DeclarationKind::Method, "Box")
        .ok_or("Box.Parse instance method missing")?;
    let via_method = find_member(&declarations, b"Via", DeclarationKind::Method, "Box")
        .ok_or("Box.Via method missing")?;
    let conditional_method =
        find_member(&declarations, b"Conditional", DeclarationKind::Method, "Box")
            .ok_or("Box.Conditional method missing")?;

    let parse_row = declaration_index(&declarations, parse_method);
    let box_parse_row = declaration_index(&declarations, box_parse_method);
    let length_row = declaration_index(&declarations, length_method);
    let via_row = declaration_index(&declarations, via_method);
    let conditional_row = declaration_index(&declarations, conditional_method);
    let _widget_row = declaration_index(&declarations, widget);
    let _box_row = declaration_index(&declarations, box_type);

    let (bound_parse_start, bound_parse_end) = span_after(source, b"bound = Widget.", b"Parse");
    let bound_groups = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.owner == via_row
                && reference.spelling.bytes == b"Parse"
                && reference.target == Some(parse_row)
                && reference.start == bound_parse_start
                && reference.end == bound_parse_end
        })
        .collect::<Vec<_>>();
    if bound_groups.len() != 1 {
        return Err(format!(
            "expected one MethodGroup for bare Parse in bound assignment, found {}",
            bound_groups.len()
        )
        .into());
    }
    if bound_groups[0].start != bound_parse_start || bound_groups[0].end != bound_parse_end {
        return Err(format!(
            "bare Parse span {:?}..{:?}, expected {:?}..{:?}",
            bound_groups[0].start,
            bound_groups[0].end,
            bound_parse_start,
            bound_parse_end
        )
        .into());
    }

    let (select_parse_start, select_parse_end) = span_after(source, b"Select(Widget.", b"Parse");
    let select_groups = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.owner == via_row
                && reference.spelling.bytes == b"Parse"
                && reference.target == Some(parse_row)
                && reference.start == select_parse_start
                && reference.end == select_parse_end
        })
        .collect::<Vec<_>>();
    if select_groups.len() != 1 {
        return Err(format!(
            "expected one MethodGroup for Parse in Select argument, found {}",
            select_groups.len()
        )
        .into());
    }

    let (length_start, length_end) = span_after(source, b"Widget.", b"Length");
    let length_groups = matching_method_groups(&references, via_row, b"Length", length_row);
    if length_groups.len() != 1 {
        return Err(format!(
            "expected one MethodGroup for Widget.Length, found {}",
            length_groups.len()
        )
        .into());
    }
    if length_groups[0].start != length_start || length_groups[0].end != length_end {
        return Err(format!(
            "qualified Length span {:?}..{:?}, expected {:?}..{:?}",
            length_groups[0].start,
            length_groups[0].end,
            length_start,
            length_end
        )
        .into());
    }

    let member_access_on_length = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MemberAccess
                && reference.owner == via_row
                && reference.start == length_start
                && reference.end == length_end
        })
        .count();
    if member_access_on_length != 0 {
        return Err(format!(
            "Widget.Length must not also be MemberAccess on the Length token (found {})",
            member_access_on_length
        )
        .into());
    }

    let (callee_start, callee_end) = span_after(source, b"called = Widget.", b"Parse");
    let invocations = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::Invocation
                && reference.owner == via_row
                && reference.spelling.bytes.windows(b"Parse".len()).any(|window| window == b"Parse")
                && reference.start <= callee_start
                && reference.end >= callee_end
        })
        .collect::<Vec<_>>();
    if invocations.len() != 1 {
        return Err(format!(
            "expected one Invocation covering Parse callee, found {}",
            invocations.len()
        )
        .into());
    }
    let method_group_on_callee = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.start == callee_start
                && reference.end == callee_end
        })
        .count();
    if method_group_on_callee != 0 {
        return Err(format!(
            "Parse callee must not also be MethodGroup (found {})",
            method_group_on_callee
        )
        .into());
    }

    let (nameof_parse_start, nameof_parse_end) = span_after(source, b"nameof(Widget.", b"Parse");
    let nameof_groups = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.start == nameof_parse_start
                && reference.end == nameof_parse_end
        })
        .count();
    if nameof_groups != 0 {
        return Err(format!(
            "nameof(Parse) must not emit MethodGroup (found {})",
            nameof_groups
        )
        .into());
    }

    let (grouped_parse_start, grouped_parse_end) = span_after(source, b"null : box.", b"Parse");
    let grouped_binding = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.owner == conditional_row
                && reference.spelling.bytes == b"Parse"
                && reference.target == Some(box_parse_row)
                && reference.start == grouped_parse_start
                && reference.end == grouped_parse_end
        })
        .collect::<Vec<_>>();
    if grouped_binding.len() != 1 {
        return Err(format!(
            "expected one MethodGroup for box.Parse value, found {}",
            grouped_binding.len()
        )
        .into());
    }

    let (conditional_callee_start, conditional_callee_end) =
        span_after(source, b"called = box?.", b"Parse");
    if !method_groups_at_span(
        &references,
        conditional_row,
        conditional_callee_start,
        conditional_callee_end,
    )
    .is_empty()
    {
        return Err("box?.Parse(\"x\") must not emit MethodGroup on the Parse token".into());
    }
    let conditional_invocations = invocations_owned_by(&references, conditional_row)
        .into_iter()
        .filter(|reference| reference.spelling.bytes.windows(b"Parse".len()).any(|w| w == b"Parse"))
        .filter(|reference| {
            reference.start <= conditional_callee_start && reference.end >= conditional_callee_end
        })
        .collect::<Vec<_>>();
    if conditional_invocations.len() != 1 {
        return Err(format!(
            "expected one Invocation for box?.Parse(\"x\"), found {}",
            conditional_invocations.len()
        )
        .into());
    }

    let (wrapped_parse_start, wrapped_parse_end) =
        span_after(source, b"(box?.Parse(", b"Parse");
    if !method_groups_at_span(
        &references,
        conditional_row,
        wrapped_parse_start,
        wrapped_parse_end,
    )
    .is_empty()
    {
        return Err("(box?.Parse)(\"x\") must not emit MethodGroup on the Parse token".into());
    }
    let wrapped_invocations = invocations_owned_by(&references, conditional_row)
        .into_iter()
        .filter(|reference| reference.spelling.bytes.windows(b"Parse".len()).any(|w| w == b"Parse"))
        .filter(|reference| {
            reference.start <= wrapped_parse_start && reference.end >= wrapped_parse_end
        })
        .collect::<Vec<_>>();
    if wrapped_invocations.len() != 1 {
        return Err(format!(
            "expected one Invocation for (box?.Parse)(\"x\"), found {}",
            wrapped_invocations.len()
        )
        .into());
    }

    let (select_arg_start, select_arg_end) = span_after(source, b"selected = items?.Select(", b"Parse");
    let select_arg_groups = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.owner == conditional_row
                && reference.spelling.bytes == b"Parse"
                && reference.target == Some(box_parse_row)
                && reference.start == select_arg_start
                && reference.end == select_arg_end
        })
        .collect::<Vec<_>>();
    if select_arg_groups.len() != 1 {
        return Err(format!(
            "expected one MethodGroup for Parse in items?.Select(Parse), found {}",
            select_arg_groups.len()
        )
        .into());
    }
    let (select_name_start, select_name_end) = span_after(source, b"items?.", b"Select");
    let select_method_groups = references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceTag::MethodGroup
                && reference.owner == conditional_row
                && reference.spelling.bytes == b"Select"
                && reference.start == select_name_start
                && reference.end == select_name_end
        })
        .count();
    if select_method_groups != 0 {
        return Err(format!(
            "items?.Select must not emit MethodGroup on Select (found {})",
            select_method_groups
        )
        .into());
    }

    Ok(())
}
