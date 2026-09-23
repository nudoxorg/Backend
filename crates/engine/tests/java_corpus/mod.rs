//! Frozen twenty-row Maven corpus (`.codex/evidence/capabilities/java-authority-lifecycle/
//! corpus-table.md`, scout-verified 2026-09-03) plus the fixture pieces the corpus
//! journey needs: a best-effort-removed temp directory and a typed file writer.
//!
//! Entry paths are the exact paths inside each sources jar; dependency coordinates
//! are complete Maven coordinates. Rows must never be substituted, extended, or
//! trimmed. Fetches resolve only from the canonical Central host because the repo1
//! CDN edge served stale cached 404s during scouting.

use std::{
    fs,
    io::{self},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;

/// Canonical Maven Central host; never fetch from any other host.
pub const CENTRAL_HOST: &str = "https://repo.maven.apache.org/maven2/";

/// Zero-based position of the frozen commons-csv row (1-based row 6) whose
/// fragments carry the publication, index, and generation-2 leg.
pub const PUBLICATION_ROW: usize = 5;

/// Zero-based position of the frozen jetbrains-annotations row (1-based row 18).
/// `NotNull` and `Nullable` declare annotation types with no executable, so
/// they carry no call, while `ApiStatus`'s private constructor calls
/// `new AssertionError(...)`. The row asserts exactly that, and that the
/// `NotNull.exception() default Exception.class` type reference survives
/// lowering.
pub const ANNOTATIONS_ROW: usize = 17;

/// One frozen corpus row: release PURL, frozen entry paths inside the sources
/// jar, and the complete classpath dependency coordinates.
pub struct CorpusRow {
    /// Release PURL in the `maven:` spelling.
    pub purl: &'static str,
    /// Frozen entry paths, exactly as they must exist inside the sources jar.
    pub entries: &'static [&'static str],
    /// Complete classpath dependency coordinates in frozen order.
    pub deps: &'static [&'static str],
}

pub const CORPUS: &[CorpusRow] = &[
    CorpusRow {
        purl: "maven:org.apache.commons:commons-lang3@3.14.0",
        entries: &[
            "org/apache/commons/lang3/StringUtils.java",
            "org/apache/commons/lang3/CharUtils.java",
            "org/apache/commons/lang3/BooleanUtils.java",
            "org/apache/commons/lang3/ArchUtils.java",
            "org/apache/commons/lang3/tuple/Pair.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:com.google.guava:guava@33.0.0-jre",
        entries: &[
            "com/google/common/base/Strings.java",
            "com/google/common/base/Joiner.java",
            "com/google/common/collect/ImmutableList.java",
            "com/google/common/collect/HashBiMap.java",
            "com/google/common/primitives/Ints.java",
        ],
        deps: &[
            "maven:com.google.guava:failureaccess@1.0.2",
            "maven:com.google.errorprone:error_prone_annotations@2.18.0",
            "maven:org.checkerframework:checker-qual@3.36.0",
            "maven:com.google.code.findbugs:jsr305@3.0.2",
            "maven:com.google.j2objc:j2objc-annotations@2.8",
        ],
    },
    CorpusRow {
        purl: "maven:com.fasterxml.jackson.core:jackson-databind@2.16.1",
        entries: &[
            "com/fasterxml/jackson/databind/JsonNode.java",
            "com/fasterxml/jackson/databind/node/ObjectNode.java",
            "com/fasterxml/jackson/databind/JsonMappingException.java",
        ],
        deps: &[
            "maven:com.fasterxml.jackson.core:jackson-core@2.16.1",
            "maven:com.fasterxml.jackson.core:jackson-annotations@2.16.1",
        ],
    },
    CorpusRow {
        purl: "maven:org.junit.jupiter:junit-jupiter-api@5.10.1",
        entries: &[
            "org/junit/jupiter/api/Test.java",
            "org/junit/jupiter/api/Assertions.java",
            "org/junit/jupiter/api/BeforeEach.java",
        ],
        deps: &[
            "maven:org.junit.platform:junit-platform-commons@1.10.1",
            "maven:org.apiguardian:apiguardian-api@1.1.2",
            "maven:org.opentest4j:opentest4j@1.3.0",
        ],
    },
    CorpusRow {
        purl: "maven:io.projectreactor:reactor-core@3.6.2",
        entries: &[
            "reactor/core/publisher/Mono.java",
            "reactor/core/scheduler/Schedulers.java",
        ],
        deps: &[
            "maven:org.reactivestreams:reactive-streams@1.0.4",
            "maven:io.micrometer:micrometer-core@1.12.1",
            "maven:io.micrometer:micrometer-commons@1.12.1",
        ],
    },
    CorpusRow {
        purl: "maven:org.apache.commons:commons-csv@1.10.0",
        entries: &[
            "org/apache/commons/csv/CSVFormat.java",
            "org/apache/commons/csv/CSVParser.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:org.jsoup:jsoup@1.17.2",
        entries: &[
            "org/jsoup/Jsoup.java",
            "org/jsoup/nodes/Node.java",
            "org/jsoup/nodes/Element.java",
        ],
        deps: &["maven:org.jspecify:jspecify@1.0.0"],
    },
    CorpusRow {
        purl: "maven:org.ow2.asm:asm@9.6",
        entries: &[
            "org/objectweb/asm/ClassVisitor.java",
            "org/objectweb/asm/ClassReader.java",
            "org/objectweb/asm/MethodVisitor.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:com.google.code.gson:gson@2.10.1",
        entries: &[
            "com/google/gson/Gson.java",
            "com/google/gson/reflect/TypeToken.java",
            "com/google/gson/JsonObject.java",
        ],
        deps: &["maven:com.google.errorprone:error_prone_annotations@2.18.0"],
    },
    CorpusRow {
        purl: "maven:io.vavr:vavr@0.10.4",
        entries: &[
            "io/vavr/Lazy.java",
            "io/vavr/Tuple.java",
            "io/vavr/control/Either.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:org.jctools:jctools-core@4.0.2",
        entries: &[
            "org/jctools/queues/MpscArrayQueue.java",
            "org/jctools/queues/SpscArrayQueue.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:org.apache.commons:commons-math3@3.6.1",
        entries: &[
            "org/apache/commons/math3/fraction/Fraction.java",
            "org/apache/commons/math3/util/ArithmeticUtils.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:org.eclipse.collections:eclipse-collections@11.1.0",
        entries: &[
            "org/eclipse/collections/impl/list/mutable/FastList.java",
            "org/eclipse/collections/impl/tuple/Tuples.java",
        ],
        deps: &["maven:org.eclipse.collections:eclipse-collections-api@11.1.0"],
    },
    CorpusRow {
        purl: "maven:org.bitbucket.b_c:jose4j@0.9.6",
        entries: &[
            "org/jose4j/jws/JsonWebSignature.java",
            "org/jose4j/jwt/JwtClaims.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:org.tukaani:xz@1.9",
        entries: &[
            "org/tukaani/xz/XZInputStream.java",
            "org/tukaani/xz/check/CRC32.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:org.apache.commons:commons-compress@1.26.0",
        entries: &["org/apache/commons/compress/archivers/zip/ZipArchiveEntry.java"],
        deps: &[
            "maven:commons-io:commons-io@2.15.1",
            "maven:commons-codec:commons-codec@1.16.0",
            "maven:org.apache.commons:commons-lang3@3.14.0",
        ],
    },
    CorpusRow {
        purl: "maven:org.apache.commons:commons-pool2@2.12.0",
        entries: &[
            "org/apache/commons/pool2/impl/GenericObjectPool.java",
            "org/apache/commons/pool2/BaseObject.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:org.jetbrains:annotations@24.0.1",
        entries: &[
            "org/jetbrains/annotations/NotNull.java",
            "org/jetbrains/annotations/ApiStatus.java",
            "org/jetbrains/annotations/Nullable.java",
        ],
        deps: &[],
    },
    CorpusRow {
        purl: "maven:com.zaxxer:HikariCP@5.1.0",
        entries: &[
            "com/zaxxer/hikari/HikariDataSource.java",
            "com/zaxxer/hikari/HikariConfig.java",
        ],
        deps: &[
            "maven:org.slf4j:slf4j-api@2.0.10",
            "maven:io.dropwizard.metrics:metrics-healthchecks@4.2.25",
            "maven:io.dropwizard.metrics:metrics-core@4.2.25",
        ],
    },
    CorpusRow {
        purl: "maven:org.apache.commons:commons-text@1.11.0",
        entries: &[
            "org/apache/commons/text/StringEscapeUtils.java",
            "org/apache/commons/text/similarity/JaroWinklerSimilarity.java",
        ],
        deps: &["maven:org.apache.commons:commons-lang3@3.14.0"],
    },
];

/// The class simple name prefixed by its package, derived from one frozen
/// entry path (`org/apache/commons/csv/CSVFormat.java` becomes
/// `org.apache.commons.csv.CSVFormat`).
pub fn qualified_name(entry: &str) -> String {
    let trimmed = entry.strip_suffix(".java").unwrap_or(entry);
    trimmed.replace('/', ".")
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("fixture filesystem operation at {path:?} failed: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_owned(),
        source,
    }
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct TempDir {
    pub path: PathBuf,
}
impl TempDir {
    pub fn new(label: &str) -> Result<Self, Error> {
        let path = std::env::temp_dir().join(format!(
            "nudox-java-corpus-{}-{}-{}-{label}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| io(Path::new("clock"), io::Error::other(e)))?
                .as_nanos()
        ));
        fs::create_dir(&path).map_err(|e| io(&path, e))?;
        Ok(Self { path })
    }
    pub fn remove(self) -> Result<(), Error> {
        fs::remove_dir_all(&self.path).map_err(|e| io(&self.path, e))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    }
    fs::write(path, bytes).map_err(|e| io(path, e))
}
