package com.example;

/// A markdown-documented class (JEP 467 `///` doc comments).
///
/// This uses **bold** text and a `[java.util.List]` element reference, to
/// probe whether the running JDK's javadoc recognizes `///` as a doc comment
/// at all. See `nudox_producer_java::producer` module doc for what this
/// currently proves on JDK 21 (nothing — `///` is not a doc comment before
/// JDK 23) versus what it will prove once run against a JDK 23+ toolchain.
public class MarkdownDocProbe {
    /// The markdown-documented field.
    public int x;
}
