//! The icon and logo set, embedded in the binary and rendered as alpha masks.
//! Icons are a closed enum: a view names one, it cannot invent a path.
//! Their colour is the text colour, so they inherit whatever ink they sit in.
//!
//! Three vocabularies live here and the split is deliberate. *Icons* are
//! chrome: navigation, window controls, settings. *Logos* are the eight
//! language marks (simple-icons, CC0), drawn wherever a language is named.
//! *Kind marks* are the nineteen declaration shapes a glyph tile holds; they
//! are addressed by [`kind_path`] from the closed
//! [`backend_library::DeclarationKind`] so a kind without a mark cannot exist.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use backend_library::DeclarationKind;
use backend_present::Language;
use gpui::{AssetSource, Hsla, Result, SharedString, Styled, Svg, px, svg};
use std::borrow::Cow;

/// Every chrome icon this application draws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Icon {
    /// The omnibar's search mark.
    Search,
    /// The command-palette mark.
    Command,
    /// Add a project.
    Plus,
    /// Choose a folder.
    Folder,
    /// Close a tab or dismiss a surface.
    Close,
    /// Collapse a panel on the left.
    ChevronLeft,
    /// Collapse a panel on the right.
    ChevronRight,
    /// Open a disclosure.
    ChevronDown,
    /// Copy exact text.
    Copy,
    /// Settings.
    Gear,
    /// The light appearance.
    Sun,
    /// The dark appearance.
    Moon,
    /// Re-read.
    Refresh,
    /// Open outside this window.
    External,
    /// The browse page.
    Home,
    /// Walk history back.
    ArrowLeft,
    /// Walk history forward.
    ArrowRight,
    /// Minimise the window.
    Minimize,
    /// Maximise the window.
    Maximize,
    /// Restore the window from maximised.
    Restore,
    /// A link into another declaration.
    Link,
    /// Source text.
    Code,
    /// An agent connection.
    Spark,
    /// Run.
    Play,
}

impl Icon {
    /// Returns the asset path this icon loads from.
    pub(crate) const fn path(self) -> &'static str {
        match self {
            Self::Search => "icons/search.svg",
            Self::Command => "icons/command.svg",
            Self::Plus => "icons/plus.svg",
            Self::Folder => "icons/folder.svg",
            Self::Close => "icons/close.svg",
            Self::ChevronLeft => "icons/chevron-left.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::Copy => "icons/copy.svg",
            Self::Gear => "icons/gear.svg",
            Self::Sun => "icons/sun.svg",
            Self::Moon => "icons/moon.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::External => "icons/external.svg",
            Self::Home => "icons/home.svg",
            Self::ArrowLeft => "icons/arrow-left.svg",
            Self::ArrowRight => "icons/arrow-right.svg",
            Self::Minimize => "icons/minimize.svg",
            Self::Maximize => "icons/maximize.svg",
            Self::Restore => "icons/restore.svg",
            Self::Link => "icons/link.svg",
            Self::Code => "icons/code.svg",
            Self::Spark => "icons/spark.svg",
            Self::Play => "icons/play.svg",
        }
    }
}

/// Every language logo this application draws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Logo(Language);

impl Logo {
    /// Returns the logo for one language, when the language has one.
    pub(crate) const fn of(language: Language) -> Option<Self> {
        match language {
            Language::Unknown => None,
            _ => Some(Self(language)),
        }
    }

    /// Returns the asset path this logo loads from.
    pub(crate) const fn path(self) -> &'static str {
        match self.0 {
            Language::Rust | Language::Unknown => "logos/rust.svg",
            Language::Python => "logos/python.svg",
            Language::TypeScript => "logos/typescript.svg",
            Language::Go => "logos/go.svg",
            Language::Java => "logos/java.svg",
            Language::CSharp => "logos/csharp.svg",
            Language::C => "logos/c.svg",
            Language::Cxx => "logos/cpp.svg",
        }
    }
}

/// The asset a package or project mark is drawn from.
pub(crate) const PACKAGE_PATH: &str = "kinds/package.svg";

/// Returns the asset the mark for one declaration kind is drawn from.
///
/// Kinds are a closed set and so are their marks: a new kind without a mark
/// is a compile error here, not a blank tile in a list.
pub(crate) const fn kind_path(kind: Option<DeclarationKind>) -> &'static str {
    match kind {
        Some(DeclarationKind::Module) => "kinds/module.svg",
        Some(DeclarationKind::Struct) => "kinds/struct.svg",
        Some(DeclarationKind::Class) => "kinds/class.svg",
        Some(DeclarationKind::Enum) => "kinds/enum.svg",
        Some(DeclarationKind::Variant) => "kinds/variant.svg",
        Some(DeclarationKind::Union) => "kinds/union.svg",
        Some(DeclarationKind::Interface) => "kinds/interface.svg",
        Some(DeclarationKind::Trait) => "kinds/trait.svg",
        Some(DeclarationKind::Type) => "kinds/type.svg",
        Some(DeclarationKind::Function) => "kinds/function.svg",
        Some(DeclarationKind::Method) => "kinds/method.svg",
        Some(DeclarationKind::Constructor) => "kinds/constructor.svg",
        Some(DeclarationKind::Macro) => "kinds/macro.svg",
        Some(DeclarationKind::Constant) => "kinds/constant.svg",
        Some(DeclarationKind::Field) => "kinds/field.svg",
        Some(DeclarationKind::Property) => "kinds/property.svg",
        Some(DeclarationKind::Variable) => "kinds/variable.svg",
        Some(DeclarationKind::Import) => "kinds/import.svg",
        Some(DeclarationKind::Unknown) | None => "kinds/unknown.svg",
    }
}

/// Returns one icon at an explicit size and paint role.
pub(crate) fn sized(theme: &Theme, mark: Icon, side: f32, role: Paint) -> Svg {
    inked(mark, side, theme.paint(role))
}

/// Returns one icon at an explicit size in an exact ink.
pub(crate) fn inked(mark: Icon, side: f32, ink: Hsla) -> Svg {
    svg()
        .path(mark.path())
        .w(px(side))
        .h(px(side))
        .flex_none()
        .text_color(ink)
}

/// Returns one language logo at an explicit size in an exact ink.
pub(crate) fn logo(mark: Logo, side: f32, ink: Hsla) -> Svg {
    svg()
        .path(mark.path())
        .w(px(side))
        .h(px(side))
        .flex_none()
        .text_color(ink)
}

/// The embedded asset source.
///
/// Icons are compiled into the binary rather than read from a bundle so the
/// application draws identically whether it is launched from a build tree, a
/// `.app`, or a test harness.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(bytes_for(path).map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        let mut all: Vec<SharedString> = ALL
            .iter()
            .map(|icon| SharedString::new_static(icon.path()))
            .collect();
        all.extend(
            LOGOS
                .iter()
                .map(|logo| SharedString::new_static(logo.path())),
        );
        all.extend(
            crate::theme::kind::ALL_KINDS
                .iter()
                .map(|kind| SharedString::new_static(kind_path(Some(*kind)))),
        );
        all.push(SharedString::new_static(PACKAGE_PATH));
        Ok(all)
    }
}

/// Every icon, for the asset listing and the preview fixtures.
pub(crate) const ALL: [Icon; 24] = [
    Icon::Search,
    Icon::Command,
    Icon::Plus,
    Icon::Folder,
    Icon::Close,
    Icon::ChevronLeft,
    Icon::ChevronRight,
    Icon::ChevronDown,
    Icon::Copy,
    Icon::Gear,
    Icon::Sun,
    Icon::Moon,
    Icon::Refresh,
    Icon::External,
    Icon::Home,
    Icon::ArrowLeft,
    Icon::ArrowRight,
    Icon::Minimize,
    Icon::Maximize,
    Icon::Restore,
    Icon::Link,
    Icon::Code,
    Icon::Spark,
    Icon::Play,
];

/// Every logo, for the asset listing.
pub(crate) const LOGOS: [Logo; 8] = [
    Logo(Language::Rust),
    Logo(Language::Python),
    Logo(Language::TypeScript),
    Logo(Language::Go),
    Logo(Language::Java),
    Logo(Language::CSharp),
    Logo(Language::C),
    Logo(Language::Cxx),
];

fn bytes_for(path: &str) -> Option<&'static [u8]> {
    match path {
        "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
        "icons/command.svg" => Some(include_bytes!("../assets/icons/command.svg")),
        "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
        "icons/folder.svg" => Some(include_bytes!("../assets/icons/folder.svg")),
        "icons/close.svg" => Some(include_bytes!("../assets/icons/close.svg")),
        "icons/chevron-left.svg" => Some(include_bytes!("../assets/icons/chevron-left.svg")),
        "icons/chevron-right.svg" => Some(include_bytes!("../assets/icons/chevron-right.svg")),
        "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
        "icons/copy.svg" => Some(include_bytes!("../assets/icons/copy.svg")),
        "icons/gear.svg" => Some(include_bytes!("../assets/icons/gear.svg")),
        "icons/sun.svg" => Some(include_bytes!("../assets/icons/sun.svg")),
        "icons/moon.svg" => Some(include_bytes!("../assets/icons/moon.svg")),
        "icons/refresh.svg" => Some(include_bytes!("../assets/icons/refresh.svg")),
        "icons/external.svg" => Some(include_bytes!("../assets/icons/external.svg")),
        "icons/home.svg" => Some(include_bytes!("../assets/icons/home.svg")),
        "icons/arrow-left.svg" => Some(include_bytes!("../assets/icons/arrow-left.svg")),
        "icons/arrow-right.svg" => Some(include_bytes!("../assets/icons/arrow-right.svg")),
        "icons/minimize.svg" => Some(include_bytes!("../assets/icons/minimize.svg")),
        "icons/maximize.svg" => Some(include_bytes!("../assets/icons/maximize.svg")),
        "icons/restore.svg" => Some(include_bytes!("../assets/icons/restore.svg")),
        "icons/link.svg" => Some(include_bytes!("../assets/icons/link.svg")),
        "icons/code.svg" => Some(include_bytes!("../assets/icons/code.svg")),
        "icons/spark.svg" => Some(include_bytes!("../assets/icons/spark.svg")),
        "icons/play.svg" => Some(include_bytes!("../assets/icons/play.svg")),
        _ => logo_bytes(path).or_else(|| kind_bytes(path)),
    }
}

fn kind_bytes(path: &str) -> Option<&'static [u8]> {
    match path {
        "kinds/module.svg" => Some(include_bytes!("../assets/kinds/module.svg")),
        "kinds/struct.svg" => Some(include_bytes!("../assets/kinds/struct.svg")),
        "kinds/class.svg" => Some(include_bytes!("../assets/kinds/class.svg")),
        "kinds/enum.svg" => Some(include_bytes!("../assets/kinds/enum.svg")),
        "kinds/variant.svg" => Some(include_bytes!("../assets/kinds/variant.svg")),
        "kinds/union.svg" => Some(include_bytes!("../assets/kinds/union.svg")),
        "kinds/interface.svg" => Some(include_bytes!("../assets/kinds/interface.svg")),
        "kinds/trait.svg" => Some(include_bytes!("../assets/kinds/trait.svg")),
        "kinds/type.svg" => Some(include_bytes!("../assets/kinds/type.svg")),
        "kinds/function.svg" => Some(include_bytes!("../assets/kinds/function.svg")),
        "kinds/method.svg" => Some(include_bytes!("../assets/kinds/method.svg")),
        "kinds/constructor.svg" => Some(include_bytes!("../assets/kinds/constructor.svg")),
        "kinds/macro.svg" => Some(include_bytes!("../assets/kinds/macro.svg")),
        "kinds/constant.svg" => Some(include_bytes!("../assets/kinds/constant.svg")),
        "kinds/field.svg" => Some(include_bytes!("../assets/kinds/field.svg")),
        "kinds/property.svg" => Some(include_bytes!("../assets/kinds/property.svg")),
        "kinds/variable.svg" => Some(include_bytes!("../assets/kinds/variable.svg")),
        "kinds/import.svg" => Some(include_bytes!("../assets/kinds/import.svg")),
        "kinds/unknown.svg" => Some(include_bytes!("../assets/kinds/unknown.svg")),
        "kinds/package.svg" => Some(include_bytes!("../assets/kinds/package.svg")),
        _ => None,
    }
}

fn logo_bytes(path: &str) -> Option<&'static [u8]> {
    match path {
        "logos/rust.svg" => Some(include_bytes!("../assets/logos/rust.svg")),
        "logos/python.svg" => Some(include_bytes!("../assets/logos/python.svg")),
        "logos/typescript.svg" => Some(include_bytes!("../assets/logos/typescript.svg")),
        "logos/go.svg" => Some(include_bytes!("../assets/logos/go.svg")),
        "logos/java.svg" => Some(include_bytes!("../assets/logos/java.svg")),
        "logos/csharp.svg" => Some(include_bytes!("../assets/logos/csharp.svg")),
        "logos/c.svg" => Some(include_bytes!("../assets/logos/c.svg")),
        "logos/cpp.svg" => Some(include_bytes!("../assets/logos/cpp.svg")),
        _ => None,
    }
}
