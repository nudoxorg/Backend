# Frozen Java corpus (J10-E)

This is the scout-frozen order.  No row, entry, or dependency may be changed by
the corpus journey.  Profiles below are filled from the single-threaded
`java_corpus` run (`CORPUS|...` output); `pending` means the measurement has not
yet completed in this worktree.

| row | purl | entries | dependencies | scout verdict | measured profile |
|---:|---|---:|---|---|---|
| 1 | maven:org.apache.commons:commons-lang3@3.14.0 | whole sources jar | none | frozen; whole-artifact law | pending |
| 2 | maven:com.google.guava:guava@33.0.0-jre | 5 | failureaccess 1.0.2; error_prone_annotations 2.18.0; checker-qual 3.36.0; jsr305 3.0.2; j2objc-annotations 2.8 | frozen; selected entries lower | pending |
| 3 | maven:com.fasterxml.jackson.core:jackson-databind@2.16.1 | 3 | jackson-core 2.16.1; jackson-annotations 2.16.1 | frozen; selected entries lower | pending |
| 4 | maven:org.junit.jupiter:junit-jupiter-api@5.10.1 | 3 | junit-platform-commons 1.10.1; apiguardian-api 1.1.2; opentest4j 1.3.0 | frozen; selected entries lower | pending |
| 5 | maven:io.projectreactor:reactor-core@3.6.2 | 2 | reactive-streams 1.0.4; micrometer-core 1.12.1; micrometer-commons 1.12.1 | frozen; selected entries lower | pending |
| 6 | maven:org.apache.commons:commons-csv@1.10.0 | 2 | none | frozen; publication seed row | pending |
| 7 | maven:org.jsoup:jsoup@1.17.2 | 3 | jspecify 1.0.0 | frozen; selected entries lower | pending |
| 8 | maven:org.ow2.asm:asm@9.6 | 3 | none | frozen; selected entries lower | pending |
| 9 | maven:com.google.code.gson:gson@2.10.1 | 3 | error_prone_annotations 2.18.0 | frozen; selected entries lower | pending |
| 10 | maven:io.vavr:vavr@0.10.4 | 3 | none | frozen; selected entries lower | pending |
| 11 | maven:org.jctools:jctools-core@4.0.2 | 2 | none | frozen; selected entries lower | pending |
| 12 | maven:org.apache.commons:commons-math3@3.6.1 | 2 | none | frozen; selected entries lower | pending |
| 13 | maven:org.eclipse.collections:eclipse-collections@11.1.0 | 2 | eclipse-collections-api 11.1.0 | frozen; selected entries lower | pending |
| 14 | maven:org.bitbucket.b_c:jose4j@0.9.6 | 2 | none | frozen; selected entries lower | pending |
| 15 | maven:org.tukaani:xz@1.9 | 2 | none | frozen; selected entries lower | pending |
| 16 | maven:org.apache.commons:commons-compress@1.26.0 | 1 | commons-io 2.15.1; commons-codec 1.16.0; commons-lang3 3.14.0 | frozen; selected entry lower | pending |
| 17 | maven:org.apache.commons:commons-pool2@2.12.0 | 2 | none | frozen; selected entries lower | pending |
| 18 | maven:org.jetbrains:annotations@24.0.1 | 3 | none | frozen; annotation occurrence absence | pending |
| 19 | maven:com.zaxxer:HikariCP@5.1.0 | 2 | slf4j-api 2.0.10; metrics-healthchecks 4.2.25; metrics-core 4.2.25 | frozen; selected entries lower | pending |
| 20 | maven:org.apache.commons:commons-text@1.11.0 | 2 | commons-lang3 3.14.0 | frozen; selected entries lower | pending |
