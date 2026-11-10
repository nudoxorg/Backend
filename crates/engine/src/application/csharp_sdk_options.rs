//! Conservative captured-project options for a private SDK observation.
//! This is not MSBuild evaluation; unsupported constructs refuse the slice.
use backend_frontend_csharp::legacy::{CSharpSdkKind, CSharpSdkProjectProperties};
use quick_xml::{events::Event, reader::Reader};
use std::io::Cursor;

pub(super) struct CSharpSdkOptions {
    pub kind: CSharpSdkKind,
    pub target_framework: String,
    pub implicit_usings: bool,
    pub properties: CSharpSdkProjectProperties,
}

/// Properties that change what the compiler binds or how it treats the
/// project's entry point. Each is forwarded to the SDK observation.
const FORWARDED: [&str; 5] = [
    "TargetFramework",
    "ImplicitUsings",
    "OutputType",
    "Nullable",
    "AllowUnsafeBlocks",
];

/// Properties with no effect on name binding or the entry point. They are read
/// and ignored; every other property refuses the slice.
const BENIGN: [&str; 7] = [
    "RootNamespace",
    "InvariantGlobalization",
    "UserSecretsId",
    "GenerateDocumentationFile",
    "TreatWarningsAsErrors",
    "NoWarn",
    "Description",
];

/// Only a simple, unconditioned SDK project with absent ancestor build imports
/// can supply options. All remaining compiler-input closure remains unknown.
pub(super) fn simple_csharp_sdk_options(
    project: &str,
    ancestor_imports_absent: bool,
) -> Option<CSharpSdkOptions> {
    if !ancestor_imports_absent || project.len() > 2 * 1024 * 1024 {
        return None;
    }
    let mut reader = Reader::from_reader(Cursor::new(project.as_bytes()));
    reader.config_mut().trim_text(true);
    reader.config_mut().expand_empty_elements = true;
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut kind = None;
    let mut forwarded: [Option<String>; 5] = Default::default();
    let mut active: Option<(Option<usize>, String)> = None;
    let mut nodes = 0usize;
    loop {
        let event = reader.read_event_into(&mut buffer).ok()?;
        match event {
            Event::Start(start) => {
                let name = std::str::from_utf8(start.name().as_ref()).ok()?.to_owned();
                if stack.len() >= 32 || name.len() > 256 {
                    return None;
                }
                nodes = nodes.checked_add(1)?;
                if nodes > 8192 {
                    return None;
                }
                if active.is_some() {
                    return None;
                }
                // Only a deliberately closed project grammar can be represented
                // by the generated SDK observation project. In particular, no
                // user-defined items, targets, imports or extra properties are
                // silently discarded.
                match stack.as_slice() {
                    [] if name == "Project" && kind.is_none() => {}
                    [root]
                        if root == "Project"
                            && matches!(name.as_str(), "PropertyGroup" | "ItemGroup") => {}
                    [root, group]
                        if root == "Project"
                            && group == "PropertyGroup"
                            && (FORWARDED.contains(&name.as_str())
                                || BENIGN.contains(&name.as_str())) => {}
                    _ => return None,
                }
                let mut sdk = None;
                for attribute in start.attributes().with_checks(true) {
                    let attribute = attribute.ok()?;
                    let key = std::str::from_utf8(attribute.key.as_ref()).ok()?;
                    let value = std::str::from_utf8(attribute.value.as_ref()).ok()?;
                    if stack.is_empty() && key == "Sdk" {
                        sdk = Some(value.to_owned());
                    } else if stack.is_empty()
                        && key == "xmlns"
                        && value == "http://schemas.microsoft.com/developer/msbuild/2003"
                    {
                        // Conventional MSBuild namespace has no option effect.
                    } else {
                        return None;
                    }
                }
                if stack.is_empty() {
                    kind = Some(match sdk.as_deref()? {
                        "Microsoft.NET.Sdk" => CSharpSdkKind::Standard,
                        "Microsoft.NET.Sdk.Web" => CSharpSdkKind::Web,
                        _ => return None,
                    });
                }
                if stack.len() == 2 {
                    let slot = FORWARDED.iter().position(|candidate| *candidate == name);
                    if slot.is_some_and(|slot| forwarded[slot].is_some()) {
                        return None;
                    }
                    active = Some((slot, String::new()));
                }
                stack.push(name);
            }
            Event::Text(text) => {
                let decoded = text.decode().ok()?;
                if let Some((_, value)) = &mut active {
                    value.push_str(&decoded);
                    if value.len() > 128 {
                        return None;
                    }
                } else if !decoded.trim().is_empty() {
                    return None;
                }
            }
            Event::End(_) => {
                stack.pop()?;
                if stack.len() == 2
                    && let Some((slot, value)) = active.take()
                    && let Some(slot) = slot
                {
                    forwarded[slot] = Some(value.trim().to_owned());
                }
                if stack.len() < 2 {
                    active = None;
                }
            }
            Event::Decl(_) | Event::Comment(_) => {}
            Event::Eof => break,
            _ => return None,
        }
        buffer.clear();
    }
    if !stack.is_empty() || active.is_some() {
        return None;
    }
    let [target, implicit, output_type, nullable, allow_unsafe] = forwarded;
    let target_framework = target?;
    if !target_framework.starts_with("net")
        || target_framework.len() <= 3
        || target_framework.len() > 32
        || !target_framework[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
    {
        return None;
    }
    let (major, minor) = target_framework[3..].split_once('.')?;
    if major.parse::<u16>().ok().is_none_or(|major| major < 5) || minor.parse::<u16>().is_err() {
        return None;
    }
    let implicit_usings = match implicit.as_deref() {
        None | Some("disable" | "false") => false,
        Some("enable" | "true") => true,
        _ => return None,
    };
    let closed = |value: &Option<String>, allowed: &[&str]| {
        value.as_deref().is_none_or(|value| allowed.contains(&value))
    };
    if !closed(&output_type, &["Exe", "Library", "WinExe"])
        || !closed(&nullable, &["enable", "disable", "warnings", "annotations"])
        || !closed(&allow_unsafe, &["true", "false"])
    {
        return None;
    }
    Some(CSharpSdkOptions {
        kind: kind?,
        target_framework,
        implicit_usings,
        properties: CSharpSdkProjectProperties {
            output_type,
            nullable,
            allow_unsafe_blocks: allow_unsafe,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simple_project_refuses_ambiguous_sdk_options() {
        let good = r#"<Project Sdk="Microsoft.NET.Sdk.Web"><PropertyGroup><TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings></PropertyGroup></Project>"#;
        let options = simple_csharp_sdk_options(good, true).unwrap();
        assert_eq!(options.target_framework, "net10.0");
        assert!(options.implicit_usings);
        assert!(simple_csharp_sdk_options(good, false).is_none());
        assert!(
            simple_csharp_sdk_options(
                &good.replace("<Project Sdk=", "<Project InitialTargets=\"Custom\" Sdk="),
                true
            )
            .is_none()
        );
        assert!(
            simple_csharp_sdk_options(
                &good.replace(
                    "</Project>",
                    "<ItemGroup><UnknownSdkItem Include=\"X\"/></ItemGroup></Project>"
                ),
                true
            )
            .is_none()
        );

        assert!(simple_csharp_sdk_options(&good.replace("net10.0", "net4.8"), true).is_none());
        assert!(
            simple_csharp_sdk_options(
                &good.replace("<PropertyGroup>", "<PropertyGroup Condition=\"true\">"),
                true
            )
            .is_none()
        );
        assert!(
            simple_csharp_sdk_options(
                &good.replace("</Project>", "<Import Project=\"elsewhere\"/></Project>"),
                true
            )
            .is_none()
        );
        assert!(
            simple_csharp_sdk_options(&good.replace("net10.0", "$(TargetFramework)"), true)
                .is_none()
        );
        for mutation in [
            r#"<Using Include="X"/>"#,
            r#"<Using Remove="System"/>"#,
            r#"<Compile Remove="Generated.cs"/>"#,
            r#"<PackageReference Include="Example"/>"#,
            r#"<ProjectReference Include="Other.csproj"/>"#,
            r#"<Analyzer Include="Plugin.dll"/>"#,
        ] {
            let changed = good.replace(
                "</Project>",
                &format!("<ItemGroup>{mutation}</ItemGroup></Project>"),
            );
            assert!(simple_csharp_sdk_options(&changed, true).is_none());
        }
    }

    #[test]
    fn closed_compiler_properties_are_forwarded_and_unknown_ones_refuse() {
        let base = |body: &str| format!(
            r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework>{body}</PropertyGroup></Project>"#
        );
        let options = simple_csharp_sdk_options(
            &base("<OutputType>Exe</OutputType><Nullable>enable</Nullable><AllowUnsafeBlocks>true</AllowUnsafeBlocks><RootNamespace>X</RootNamespace>"),
            true,
        )
        .unwrap();
        assert_eq!(options.properties.output_type.as_deref(), Some("Exe"));
        assert_eq!(options.properties.nullable.as_deref(), Some("enable"));
        assert_eq!(options.properties.allow_unsafe_blocks.as_deref(), Some("true"));
        assert!(simple_csharp_sdk_options(&base("<OutputType>Dll</OutputType>"), true).is_none());
        assert!(simple_csharp_sdk_options(&base("<Nullable>sometimes</Nullable>"), true).is_none());
        assert!(simple_csharp_sdk_options(&base("<Nullable>enable</Nullable><Nullable>disable</Nullable>"), true).is_none());
        assert!(simple_csharp_sdk_options(&base("<DefineConstants>X</DefineConstants>"), true).is_none());
        assert!(simple_csharp_sdk_options(&base("<LangVersion>latest</LangVersion>"), true).is_none());
    }
}
