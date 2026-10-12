//! Real installed CPU-model experiment. This is never a protocol-fixture substitute.
use super::*;
use backend_compile::EmbeddingPurpose;
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

type NativeResult = Result<(), Box<dyn std::error::Error>>;

fn real_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find(|line| line.starts_with("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[derive(Debug)]
struct HelperSample {
    pid: u32,
    start_ticks: String,
    threads: usize,
    root_dev_inode: (u64, u64),
    argv: Vec<u8>,
    status: String,
    limits: String,
}

#[derive(Debug)]
struct TaskObservation {
    helpers: Vec<HelperSample>,
    observed_uid_threads: usize,
    unreadable_processes: usize,
}

fn helper_samples(uid: u32, image: (u64, u64)) -> std::io::Result<TaskObservation> {
    let self_root = fs::metadata("/")?;
    let expected_root = (self_root.dev(), self_root.ino());
    let mut samples = TaskObservation {
        helpers: Vec::new(),
        observed_uid_threads: 0,
        unreadable_processes: 0,
    };
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let root = entry.path();
        let Ok(status) = fs::read_to_string(root.join("status")) else {
            samples.unreadable_processes += 1;
            continue;
        };
        if real_uid(&status) != Some(uid) {
            continue;
        }
        let threads: usize = status
            .lines()
            .find(|line| line.starts_with("Threads:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
            .ok_or(std::io::ErrorKind::InvalidData)?;
        samples.observed_uid_threads = samples
            .observed_uid_threads
            .checked_add(threads)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        let Ok(executable) = fs::metadata(root.join("exe")) else {
            continue;
        };
        if (executable.dev(), executable.ino()) != image {
            continue;
        }
        let before = fs::read_to_string(root.join("stat"))?;
        let start = before
            .rsplit_once(')')
            .ok_or(std::io::ErrorKind::InvalidData)?
            .1
            .split_whitespace()
            .nth(19)
            .ok_or(std::io::ErrorKind::InvalidData)?
            .to_owned();
        let process_root = fs::metadata(root.join("root"))?;
        let argv = fs::read(root.join("cmdline"))?;
        let limits = fs::read_to_string(root.join("limits"))?;
        let after = fs::read_to_string(root.join("stat"))?;
        let after_start = after
            .rsplit_once(')')
            .ok_or(std::io::ErrorKind::InvalidData)?
            .1
            .split_whitespace()
            .nth(19)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        let after_image = fs::metadata(root.join("exe"))?;
        let after_root = fs::metadata(root.join("root"))?;
        let after_status = fs::read_to_string(root.join("status"))?;
        let after_argv = fs::read(root.join("cmdline"))?;
        if start == after_start
            && (after_image.dev(), after_image.ino()) == image
            && real_uid(&after_status) == Some(uid)
            && argv == after_argv
            && (process_root.dev(), process_root.ino()) == expected_root
            && (after_root.dev(), after_root.ino()) == expected_root
        {
            samples.helpers.push(HelperSample {
                pid,
                start_ticks: start,
                threads,
                root_dev_inode: expected_root,
                argv,
                status,
                limits,
            });
        }
    }
    Ok(samples)
}

struct SamplerStop {
    running: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SamplerStop {
    fn finish(mut self) -> NativeResult {
        self.running.store(false, Ordering::Release);
        self.thread
            .take()
            .ok_or("sampler already joined")?
            .join()
            .map_err(|_| "passive task sampler panicked")?;
        Ok(())
    }
}

impl Drop for SamplerStop {
    fn drop(&mut self) {
        // Also retire the passive observer if a real-model assertion unwinds.
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
#[ignore = "requires a non-root broker-owned ordinary CPU setup and actual pinned MiniLM artifacts"]
fn installed_linux_cpu_runtime_activates_and_infers_exact_1_16_256() -> NativeResult {
    let uid = real_uid(&fs::read_to_string("/proc/self/status")?).ok_or("actual UID")?;
    let status = fs::read_to_string("/proc/self/status")?;
    let effective = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| line.split_whitespace().nth(2))
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or("actual effective UID")?;
    assert_eq!(
        uid, effective,
        "ordinary model proof forbids setuid UID ambiguity"
    );
    assert_ne!(uid, 0, "real root bypasses Linux per-user NPROC");
    let path = PathBuf::from(std::env::var("NUDOX_CPU_MODEL_CONFIG")?);
    assert!(
        path.is_absolute(),
        "exact installed private config required"
    );
    let config_bytes = fs::read(&path)?;
    let config = read_private_config(&path)?.ok_or("ordinary setup config missing")?;
    assert_eq!(config.spec.dimension().get(), 384);
    assert_eq!(config.spec.normalization(), EmbeddingNormalization::L2);
    assert_eq!(config.spec.maximum_text_bytes().get(), 65_536);
    assert_eq!(config.model_bytes, 90_868_376);
    assert_eq!(config.tokenizer_bytes, 466_247);
    assert_eq!(
        blake3::Hash::from_bytes(config.spec.model())
            .to_hex()
            .as_str(),
        "8087e9bf97c265f8435ed268733ecf3791825ad24850fd5d84d89e32ee3a589a"
    );
    assert_eq!(
        blake3::Hash::from_bytes(config.spec.tokenizer())
            .to_hex()
            .as_str(),
        "82483bb4f0bdb81779f295ecc5a93285d2156834e994a2169f9800e4c8f250c1"
    );
    assert!(
        config.arguments.is_empty(),
        "current CPU helper is one-shot"
    );
    assert_eq!(
        config.environment,
        vec![
            ("BACKEND_EMBEDDING_DEVICE".into(), "cpu".into()),
            ("RAYON_NUM_THREADS".into(), "1".into())
        ]
    );
    assert_eq!(config.limits.memory_bytes, Some(1024 * 1024 * 1024));
    assert_eq!(config.limits.process_count, 4);
    assert_eq!(config.limits.cpu_time_ms, Some(120_000));
    assert_eq!(config.limits.stdout_bytes, 1024 * 1024);
    assert_eq!(config.limits.stderr_bytes, 65_536);
    assert_eq!(config.limits.output_bytes, 1024 * 1024 + 65_536);
    assert_eq!(config.limits.input_bytes, 1024 * 1024);
    assert_eq!(config.limits.workspace_bytes, 128 * 1024 * 1024);
    assert_eq!(config.limits.wall_time_ms, 120_000);
    assert_eq!(config.limits.resident_credit_bytes, 2 * 1024 * 1024 * 1024);
    config.limits.validate_platform()?;
    let metadata = fs::metadata(config.program.canonicalize()?)?;
    assert_eq!(
        metadata.uid(),
        uid,
        "use the actual installed user-owned helper"
    );
    let image = (metadata.dev(), metadata.ino());
    let sampling = Arc::new(AtomicBool::new(true));
    let rows = Arc::new(Mutex::new(
        Vec::<(Duration, Result<TaskObservation, String>)>::new(),
    ));
    let flag = Arc::clone(&sampling);
    let observed = Arc::clone(&rows);
    let epoch = Instant::now();
    let sampler = std::thread::spawn(move || {
        while flag.load(Ordering::Acquire) {
            let sample = helper_samples(uid, image).map_err(|error| error.to_string());
            observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((epoch.elapsed(), sample));
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    let sampler = SamplerStop {
        running: Arc::clone(&sampling),
        thread: Some(sampler),
    };
    // Open the exact installed config through the production artifact, toolchain,
    // private-directory, limit, probe and activation admission path.
    let experiment = (|| -> NativeResult {
        let activation = Instant::now();
        let provision = EmbeddingRuntimeProvision::open(&path)?;
        if provision.status()
            != (EmbeddingRuntimeStatus::Available {
                recipe: config.spec.recipe_identity(),
            })
        {
            return Err(format!("production activation refused: {:?}", provision.status()).into());
        }
        let runtime = provision
            .runtime()
            .ok_or("actual production activation refused")?;
        config
            .spec
            .validate_execution_identity(runtime.execution_identity())?;
        eprintln!(
            "CPU_MODEL activation_ms={} uid={uid} recipe={:?} execution={:?} limits={:?}",
            activation.elapsed().as_millis(),
            config.spec.recipe_identity(),
            runtime.execution_identity(),
            config.limits
        );
        for (count, text_mode) in [
            (1, "short"),
            (16, "short"),
            (256, "short"),
            (1, "long_truncated_256"),
        ] {
            let texts = if text_mode == "long_truncated_256" {
                // Verify the actual admitted tokenizer makes each lower-case word
                // a known whole WordPiece. BertPreTokenizer splits these spaces;
                // 300 known words exceed the helper's 256-token sequence bound.
                let tokenizer_path = artifact_path(&path, config.spec.tokenizer(), "tokenizer");
                let bytes = read_private_artifact(
                    &tokenizer_path,
                    config.tokenizer_bytes,
                    MAX_EMBEDDING_TOKENIZER_BYTES,
                )?;
                if blake3::hash(&bytes).as_bytes() != &config.spec.tokenizer() {
                    return Err("long-input tokenizer identity changed".into());
                }
                let tokenizer: serde_json::Value = serde_json::from_slice(&bytes)?;
                if tokenizer["model"]["type"] != "WordPiece"
                    || !tokenizer["model"]["vocab"]["hello"].is_u64()
                    || tokenizer["pre_tokenizer"]["type"] != "BertPreTokenizer"
                    || tokenizer["normalizer"]["type"] != "BertNormalizer"
                {
                    return Err("pinned tokenizer cannot establish repeated known-word law".into());
                }
                let text = "hello ".repeat(300);
                assert_eq!(text.split_whitespace().count(), 300);
                eprintln!(
                    "CPU_MODEL_LONG_INPUT known_word=hello wordpiece_words_before_special_tokens=300 helper_sequence_limit=256 tokenizer_identity_verified=true"
                );
                vec![text]
            } else {
                (0..count).map(|index| format!(
                    "Parse HTTP request arguments and prepare headers. Actual model batch {count} item {index}."
                )).collect::<Vec<_>>()
            };
            let borrowed = texts.iter().map(String::as_str).collect::<Vec<_>>();
            eprintln!(
                "CPU_MODEL_REQUEST count={count} mode={text_mode} started_us={}",
                epoch.elapsed().as_micros()
            );
            let started = Instant::now();
            let coordinates = runtime.infer_batch(EmbeddingPurpose::Document, &borrowed)?;
            if coordinates.len() != count {
                return Err(
                    "production BEM2 result cardinality differs from requested identities".into(),
                );
            }
            if text_mode == "long_truncated_256" {
                // Both known-word sequences exceed the available non-special-token
                // slots. The actual pinned helper must reduce them to exactly the
                // same 256-token sequence, including CLS/SEP, before F32 inference.
                let boundary = "hello ".repeat(256);
                let compare_started = Instant::now();
                let comparison =
                    runtime.infer_batch(EmbeddingPurpose::Document, &[boundary.as_str()])?;
                if comparison.len() != 1 {
                    return Err("truncation comparison result cardinality differs".into());
                }
                let expected = &comparison[0];
                let actual = &coordinates[0];
                if expected.recipe() != config.spec.recipe_identity()
                    || expected.model().as_bytes() != config.spec.model()
                    || expected.tokenizer().as_bytes() != config.spec.tokenizer()
                    || expected.purpose() != EmbeddingPurpose::Document
                {
                    return Err("truncation comparison returned another identity".into());
                }
                eprintln!(
                    "CPU_MODEL_TRUNCATION_COMPARE words=256 count=1 inference_ms={} values={:?}",
                    compare_started.elapsed().as_millis(),
                    expected.values()
                );
                eprintln!(
                    "CPU_MODEL_TRUNCATION_LONG words=300 count=1 values={:?}",
                    actual.values()
                );
                if actual
                    .values()
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>()
                    != expected
                        .values()
                        .iter()
                        .map(|value| value.to_bits())
                        .collect::<Vec<_>>()
                {
                    return Err("300 and 256 known words did not reduce to identical pinned 256-token production vectors".into());
                }
            }
            for (index, row) in coordinates.into_iter().enumerate() {
                eprintln!(
                    "CPU_MODEL_VECTOR count={count} mode={text_mode} index={index} text={:?} values={:?}",
                    texts[index],
                    row.values()
                );
                if row.recipe() != config.spec.recipe_identity()
                    || row.model().as_bytes() != config.spec.model()
                    || row.tokenizer().as_bytes() != config.spec.tokenizer()
                    || row.purpose() != EmbeddingPurpose::Document
                    || row.values().len() != 384
                    || !row.values().iter().all(|value| value.is_finite())
                {
                    return Err(format!("actual vector identity/dimension/finite law failed count={count} mode={text_mode} index={index}").into());
                }
                let norm = row
                    .values()
                    .iter()
                    .map(|value| f64::from(*value).powi(2))
                    .sum::<f64>();
                if (norm - 1.0).abs() >= 0.001 {
                    return Err(format!("actual vector L2 squared norm {norm}").into());
                }
            }
            eprintln!(
                "CPU_MODEL count={count} mode={text_mode} inference_ms={} finished_us={}",
                started.elapsed().as_millis(),
                epoch.elapsed().as_micros()
            );
        }
        assert_eq!(fs::read(&path)?, config_bytes, "installed config unchanged");
        Ok(())
    })();
    sampler.finish()?;
    let samples = rows
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut observed_helper = false;
    let mut policy_errors = Vec::new();
    for (elapsed, row) in samples.iter() {
        match row {
            Ok(observation) => {
                eprintln!(
                    "CPU_MODEL_UID_TASKS elapsed_us={} observed_uid_threads={} unreadable_processes={} note=per-user-not-job-local-snapshot-can-miss-transients",
                    elapsed.as_micros(),
                    observation.observed_uid_threads,
                    observation.unreadable_processes
                );
                for process in &observation.helpers {
                    observed_helper = true;
                    eprintln!(
                        "CPU_MODEL_SAMPLE elapsed_us={} pid={} start={} threads={} root_dev_inode={:?} argv={:?} status={:?} limits={:?}",
                        elapsed.as_micros(),
                        process.pid,
                        process.start_ticks,
                        process.threads,
                        process.root_dev_inode,
                        process.argv,
                        process.status,
                        process.limits
                    );
                    if process.threads != 1 {
                        policy_errors.push(format!(
                            "helper {} observed {} OS threads",
                            process.pid, process.threads
                        ));
                    }
                    let words = process
                        .limits
                        .lines()
                        .map(|line| line.split_whitespace().collect::<Vec<_>>())
                        .collect::<Vec<_>>();
                    if !words
                        .iter()
                        .any(|line| line.as_slice() == ["Max", "processes", "4", "4", "processes"])
                    {
                        policy_errors.push(format!("helper {} NPROC limits differ", process.pid));
                    }
                    if !words.iter().any(|line| {
                        line.as_slice()
                            == [
                                "Max",
                                "address",
                                "space",
                                "1073741824",
                                "1073741824",
                                "bytes",
                            ]
                    }) {
                        policy_errors.push(format!(
                            "helper {} address-space limits differ",
                            process.pid
                        ));
                    }
                }
            }
            Err(error) => eprintln!(
                "CPU_MODEL_SAMPLE_INCOMPLETE elapsed_us={} error={error}",
                elapsed.as_micros()
            ),
        }
    }
    experiment?;
    assert!(
        policy_errors.is_empty(),
        "observed helper policy refusals: {policy_errors:?}"
    );
    assert!(
        observed_helper,
        "no actual helper task/limit observation; model output alone cannot prove OS task policy"
    );
    Ok(())
}
