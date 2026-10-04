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
    let database_path = root.join(backend_extension_turso::FILE_NAME);
    let (mut projection, view, update) = open_or_seed_view(&database_path, {
        let class = lifecycle_class();
        let (initial, _) = build_view(&class)?;
        initial
    }, || build_view(&lifecycle_class()).map(|(current, _)| current))?;
    if !matches!(
        update,
        ProjectionUpdate::Rebuilt { .. } | ProjectionUpdate::Reused { .. }
    ) {
        return Err(format!("unexpected lifecycle child update: {update:?}").into());
    }
    let revision = futures_executor::block_on(projection.revision())?;
    let package_id = RowId::Package(package_key("polyglot@0.1.0")).stable_key();
    let rooted = futures_executor::block_on(projection.lookup_label("polyglot@0.1.0", 16))?;
    if revision.root() != *view.root().as_bytes()
        || rooted.root != revision.root()
        || rooted.ids.as_ref() != [package_id]
    {
        return Err("lifecycle child did not retain the exact admitted view and package row".into());
    }
    if mode == "hold" {
        let ready = ready.ok_or("hold mode needs a ready marker")?;
        fs::write(ready, b"ready")?;
        thread::sleep(Duration::from_secs(30));
    }
    Ok(())
}

fn verify_selected_projection(database_path: &Path) -> BenchResult<TursoStorageMeasurement> {
    let projection = futures_executor::block_on(TursoProjection::open(database_path))?;
    let revision = futures_executor::block_on(projection.revision())?;
    let (view, _) = build_view(&lifecycle_class())?;
    let package_id = RowId::Package(package_key("polyglot@0.1.0")).stable_key();
    let rooted = futures_executor::block_on(projection.lookup_label("polyglot@0.1.0", 16))?;
    if revision.root() != *view.root().as_bytes()
        || rooted.root != revision.root()
        || rooted.ids.as_ref() != [package_id]
    {
        return Err("reopened selected projection root or package query differed from the admitted fixture".into());
    }
    selected_storage_measurement(database_path, &projection)
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
    let mut projection_storage: [Option<TursoStorageMeasurement>; 4] = [None, None, None, None];
    let mut exits = [String::new(), String::new(), String::new(), String::new()];
    let mut correctness = [true; 4];
    for round in 0..rounds {
        let root = temp_root(&format!("turso-process-{round}"))?;
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
        exits[0] = child_exit_label(fresh_status);
        let fresh_storage = verify_selected_projection(&database_path)?;
        storage_bytes[0] = usize::try_from(fresh_storage.total_namespace_file_bytes)
            .unwrap_or(usize::MAX);
        projection_storage[0] = Some(fresh_storage.clone());
        correctness[0] &= fresh_status.success();

        let started = Instant::now();
        let mut graceful = spawn("graceful", None)?;
        let graceful_status = graceful.wait()?;
        phase_timings[1].1.push(started.elapsed().as_nanos());
        exits[1] = child_exit_label(graceful_status);
        let graceful_storage = verify_selected_projection(&database_path)?;
        storage_bytes[1] = usize::try_from(graceful_storage.total_namespace_file_bytes)
            .unwrap_or(usize::MAX);
        projection_storage[1] = Some(graceful_storage.clone());
        correctness[1] &= graceful_status.success()
            && graceful_storage.selected_generation == fresh_storage.selected_generation;

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
        exits[2] = child_exit_label(killed_status);
        correctness[2] &= ready_before_kill && kill_result.is_ok() && !killed_status.success();
        let killed_storage = turso_storage_measurement(
            &database_path,
            graceful_storage.selected_generation,
        )?;
        storage_bytes[2] = usize::try_from(killed_storage.total_namespace_file_bytes)
            .unwrap_or(usize::MAX);
        projection_storage[2] = Some(killed_storage);

        let started = Instant::now();
        let mut offline = futures_executor::block_on(TursoProjection::open(&database_path))?;
        let expected = futures_executor::block_on(offline.revision())?;
        let (view, _) = build_view(&lifecycle_class())?;
        let offline_update =
            futures_executor::block_on(offline.synchronize_from(expected, &view))?;
        let revision = futures_executor::block_on(offline.revision())?;
        let package_id = RowId::Package(package_key("polyglot@0.1.0")).stable_key();
        let rooted =
            futures_executor::block_on(offline.lookup_label("polyglot@0.1.0", 16))?;
        phase_timings[3].1.push(started.elapsed().as_nanos());
        exits[3] = format!("{offline_update:?}");
        correctness[3] &= matches!(offline_update, ProjectionUpdate::Reused { .. })
            && revision.root() == *view.root().as_bytes()
            && rooted.root == revision.root()
            && rooted.ids.as_ref() == [package_id];
        let offline_storage = selected_storage_measurement(&database_path, &offline)?;
        storage_bytes[3] = usize::try_from(offline_storage.total_namespace_file_bytes)
            .unwrap_or(usize::MAX);
        projection_storage[3] = Some(offline_storage);
        drop(offline);
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
            projection_storage: projection_storage[index].clone(),
            child_exit: exits[index].clone(),
            correctness: Correctness {
                passed: correctness[index],
                assertions: vec![format!("{} lifecycle rounds completed", rounds)],
            },
        });
    }
    Ok(measurements)
}
