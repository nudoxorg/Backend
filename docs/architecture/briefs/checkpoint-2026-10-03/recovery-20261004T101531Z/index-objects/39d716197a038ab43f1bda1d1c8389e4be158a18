use super::*;

pub(super) fn lifecycle_class() -> CorpusClass {
    let text = "pub fn lifecycle_fixture() -> usize { 7 }\n".to_owned();
    CorpusClass {
        name: "lifecycle",
        files: vec![SourceFile {
            language: "rust".to_owned(),
            path: PathBuf::from("lifecycle.rs"),
            relative: "lifecycle.rs".to_owned(),
            bytes: text.len(),
            text,
        }],
    }
}

pub(super) fn run_turso_child(args: &[String]) -> BenchResult<()> {
    let root = PathBuf::from(args.first().ok_or("--child-turso needs a root")?);
    let mode = args.get(1).ok_or("--child-turso needs a mode")?;
    let ready = args.get(2).map(PathBuf::from);
    fs::create_dir_all(&root)?;
    let (view, _) = build_view(&lifecycle_class())?;
    let database_path = root.join(backend_extension_turso::FILE_NAME);
    let mut projection = futures_executor::block_on(TursoProjection::open(&database_path))?;
    let update = futures_executor::block_on(projection.synchronize(&view))?;
    if !matches!(
        update,
        ProjectionUpdate::Rebuilt { .. } | ProjectionUpdate::Reused { .. }
    ) {
        return Err(format!("unexpected lifecycle child update: {update:?}").into());
    }
    if mode == "hold" {
        let ready = ready.ok_or("hold mode needs a ready marker")?;
        fs::write(ready, b"ready")?;
        thread::sleep(Duration::from_secs(30));
    }
    Ok(())
}

fn child_exit_label(status: std::process::ExitStatus) -> String {
    if status.success() {
        "success".to_owned()
    } else {
        status
            .code()
            .map_or_else(|| "signaled".to_owned(), |code| format!("exit-{code}"))
    }
}

pub(super) fn run_process_lifecycle(profile: Profile) -> BenchResult<Vec<LifecycleMeasurement>> {
    let executable = env::current_exe()?;
    let rounds = profile.repetitions().min(3);
    let mut phase_timings = [
        ("fresh_process", Vec::with_capacity(rounds)),
        ("graceful_restart", Vec::with_capacity(rounds)),
        ("sigkill", Vec::with_capacity(rounds)),
        ("offline_restart_after_sigkill", Vec::with_capacity(rounds)),
    ];
    let mut storage_bytes = [0_usize; 4];
    let mut exits = [String::new(), String::new(), String::new(), String::new()];
    let mut correctness = [true; 4];
    for round in 0..rounds {
        let root = temp_root(&format!("turso-process-{round}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root)?;
        let database_path = root.join(backend_extension_turso::FILE_NAME);
        let spawn = |mode: &str, ready: Option<&Path>| -> BenchResult<std::process::Child> {
            let mut command = Command::new(&executable);
            command
                .arg("--child-turso")
                .arg(&root)
                .arg(mode)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(ready) = ready {
                command.arg(ready);
            }
            Ok(command.spawn()?)
        };

        let started = Instant::now();
        let mut fresh = spawn("fresh", None)?;
        let fresh_status = fresh.wait()?;
        phase_timings[0].1.push(started.elapsed().as_nanos());
        storage_bytes[0] = dir_bytes(&root);
        exits[0] = child_exit_label(fresh_status);
        correctness[0] &= fresh_status.success() && database_path.is_file();

        let started = Instant::now();
        let mut graceful = spawn("graceful", None)?;
        let graceful_status = graceful.wait()?;
        phase_timings[1].1.push(started.elapsed().as_nanos());
        storage_bytes[1] = dir_bytes(&root);
        exits[1] = child_exit_label(graceful_status);
        correctness[1] &= graceful_status.success();

        let ready = root.join("sigkill.ready");
        let _ = fs::remove_file(&ready);
        let started = Instant::now();
        let mut killed = spawn("hold", Some(&ready))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let ready_before_kill = ready.is_file();
        let kill_result = killed.kill();
        let killed_status = killed.wait()?;
        phase_timings[2].1.push(started.elapsed().as_nanos());
        storage_bytes[2] = dir_bytes(&root);
        exits[2] = child_exit_label(killed_status);
        correctness[2] &= ready_before_kill && kill_result.is_ok() && !killed_status.success();

        let started = Instant::now();
        let mut offline = futures_executor::block_on(TursoProjection::open(&database_path))?;
        let (view, _) = build_view(&lifecycle_class())?;
        let offline_update = futures_executor::block_on(offline.synchronize(&view))?;
        phase_timings[3].1.push(started.elapsed().as_nanos());
        storage_bytes[3] = dir_bytes(&root);
        exits[3] = format!("{offline_update:?}");
        correctness[3] &= matches!(offline_update, ProjectionUpdate::Reused { .. });
        let _ = fs::remove_dir_all(root);
    }
    let mut measurements = Vec::with_capacity(phase_timings.len());
    for (index, (phase, mut timings)) in phase_timings.into_iter().enumerate() {
        measurements.push(LifecycleMeasurement {
            schema: JSON_SCHEMA,
            status: if correctness[index] { "ok" } else { "failed" },
            phase,
            wall: stats(&mut timings),
            storage_bytes: storage_bytes[index],
            child_exit: exits[index].clone(),
            correctness: Correctness {
                passed: correctness[index],
                assertions: vec![format!("{} lifecycle rounds completed", rounds)],
            },
        });
    }
    Ok(measurements)
}
