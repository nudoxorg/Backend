use std::{env, fs, path::Path};

use compiler_languages_java::{
    JavaAuthorityImage, JavaRelease,
    harness::{Harness, HarnessError, HarnessRequest, JavaSource, JdkToolchain},
};
use sha2::{Digest, Sha256};

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/authority/src")
            .join(name),
    )
    .unwrap()
}

fn toolchain() -> JdkToolchain<'static> {
    JdkToolchain::from_env().unwrap()
}

#[test]
fn prepare_reuses_digest_marker() {
    let mut harness = Harness::new().unwrap();
    harness.prepare(&toolchain()).unwrap();
    let marker = harness.marker_path();
    let first = fs::metadata(&marker).unwrap().modified().unwrap();
    harness.prepare(&toolchain()).unwrap();
    assert_eq!(first, fs::metadata(marker).unwrap().modified().unwrap());
}

#[test]
fn image_binds_the_first_source_bytes() {
    let mut harness = Harness::new().unwrap();
    let jdk = toolchain();
    harness.prepare(&jdk).unwrap();
    let cafe = fixture("demo/Cafe.java");
    let module = fixture("module-info.java");
    let helper = fixture("demo/Helper.java");
    let sources = [
        JavaSource {
            name: Path::new("demo/Cafe.java"),
            bytes: &cafe,
        },
        JavaSource {
            name: Path::new("module-info.java"),
            bytes: &module,
        },
        JavaSource {
            name: Path::new("demo/Helper.java"),
            bytes: &helper,
        },
    ];
    let mut output = Vec::new();
    harness
        .image(
            &jdk,
            HarnessRequest {
                sources: &sources,
                classpath: &[],
                release: JavaRelease::Java21,
            },
            &mut output,
        )
        .unwrap();
    let image = JavaAuthorityImage::open(&output).unwrap();
    let mut digest = Sha256::new();
    digest.update(&cafe);
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(image.source_digest(), expected);
}

#[test]
fn syntax_failure_retains_bounded_stderr() {
    let mut harness = Harness::new().unwrap();
    let jdk = toolchain();
    harness.prepare(&jdk).unwrap();
    let broken = b"package demo; class Broken {";
    let sources = [JavaSource {
        name: Path::new("demo/Broken.java"),
        bytes: broken,
    }];
    let mut output = Vec::new();
    let error = harness
        .image(
            &jdk,
            HarnessRequest {
                sources: &sources,
                classpath: &[],
                release: JavaRelease::Java21,
            },
            &mut output,
        )
        .unwrap_err();
    match error {
        HarnessError::Command {
            command, stderr, ..
        } => {
            assert!(command.contains("java"));
            assert!(stderr.len() <= 64 * 1024);
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn drop_removes_owned_scratch() {
    let path;
    {
        let harness = Harness::new().unwrap();
        path = harness.classes_dir().parent().unwrap().to_owned();
    }
    assert!(!path.exists());
}

#[test]
fn missing_environment_is_typed() {
    if env::var_os("NUDOX_JDK").is_some() {
        return;
    }
    assert!(matches!(
        JdkToolchain::from_env(),
        Err(HarnessError::MissingEnvironment {
            variable: "NUDOX_JDK"
        })
    ));
}
