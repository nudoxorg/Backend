//! Spawn, framed I/O, and teardown for a persistent native process.

use super::*;

impl PersistentProcess {
    pub(in crate::native) fn spawn(command: &SupervisedCommand) -> Result<Self, NativeRunnerError> {
        if !cfg!(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        )) {
            return Err(NativeRunnerError::Process(ProcessError::UnsupportedLimit(
                crate::UnsupportedLimit::ProcessGroup,
            )));
        }
        command
            .limits()
            .validate_supported()
            .map_err(NativeRunnerError::Process)?;
        let workspace_baseline = match command.limits().workspace_limit() {
            Some(_) => workspace_size(command.workspace()).map_err(NativeRunnerError::Process)?,
            None => 0,
        };
        command
            .verify_executable()
            .map_err(NativeRunnerError::Process)?;
        let executable = ExecutableLease::prepare(command).map_err(NativeRunnerError::Process)?;
        let reaper_permit = reserve_child_reaper().map_err(NativeRunnerError::Process)?;
        let mut process = authority_command(command, executable.path());
        let child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| NativeRunnerError::Process(ProcessError::SpawnFailure))?;
        let child_pid = child.id();
        let mut child = OwnedChild::new(child, reaper_permit);
        let stop_workers = Arc::new(AtomicBool::new(false));
        let group_retirement =
            ProcessGroupRetirement::with_stop_workers(child_pid, Arc::clone(&stop_workers));
        let Some(stdin) = child.child_mut().ok().and_then(|child| child.stdin.take()) else {
            let _ = terminate_child_with_group(&mut child, &group_retirement);
            return Err(NativeRunnerError::Process(ProcessError::SpawnFailure));
        };
        let Some(stdout) = child.child_mut().ok().and_then(|child| child.stdout.take()) else {
            let _ = terminate_child_with_group(&mut child, &group_retirement);
            return Err(NativeRunnerError::Process(ProcessError::SpawnFailure));
        };
        let Some(child_stderr) = child.child_mut().ok().and_then(|child| child.stderr.take())
        else {
            let _ = terminate_child_with_group(&mut child, &group_retirement);
            return Err(NativeRunnerError::Process(ProcessError::SpawnFailure));
        };
        if let Err(error) = set_nonblocking_pipe(&stdin)
            .and_then(|()| set_nonblocking_pipe(&stdout))
            .and_then(|()| set_nonblocking_pipe(&child_stderr))
        {
            let _ = terminate_child_with_group(&mut child, &group_retirement);
            return Err(NativeRunnerError::Process(error));
        }
        let (sender, responses) = mpsc::sync_channel(1);
        let stdout_observed = Arc::new(AtomicUsize::new(0));
        let output_observed = Arc::new(AtomicUsize::new(0));
        let stdout_drain = Arc::new((
            Mutex::new(NativeDrainProbe::default()),
            std::sync::Condvar::new(),
        ));
        #[cfg(test)]
        let stdout_reader_gate = Arc::new((
            Mutex::new(TestNativeStdoutGate::default()),
            std::sync::Condvar::new(),
        ));
        let stderr_drain = Arc::new((
            Mutex::new(StderrDrainProbe::default()),
            std::sync::Condvar::new(),
        ));
        let stdout_fault = Arc::new(AtomicU8::new(0));
        let stdout_reader = match spawn_stdout_reader(
            stdout,
            sender,
            group_retirement.clone(),
            Arc::clone(&stop_workers),
            Arc::clone(&stdout_fault),
            Arc::clone(&stdout_observed),
            Arc::clone(&output_observed),
            Arc::clone(&stdout_drain),
            #[cfg(test)]
            Arc::clone(&stdout_reader_gate),
            command.limits().stdout(),
            command.limits().output_bytes(),
        ) {
            Ok(reader) => reader,
            Err(error) => {
                stop_workers.store(true, Ordering::Release);
                #[cfg(test)]
                release_native_stdout_reader_for_test(&stdout_reader_gate);
                let _ = terminate_child_with_group(&mut child, &group_retirement);
                return Err(error);
            }
        };
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_observed = Arc::new(AtomicUsize::new(0));
        let stderr_exceeded = Arc::new(AtomicBool::new(false));
        let stderr_fault = Arc::new(AtomicU8::new(0));
        let stderr_reader = match spawn_stderr_reader(
            child_stderr,
            Arc::clone(&stderr),
            Arc::clone(&stderr_observed),
            Arc::clone(&output_observed),
            Arc::clone(&stderr_exceeded),
            Arc::clone(&stderr_fault),
            command.limits().stderr(),
            command.limits().output_bytes(),
            Arc::clone(&stderr_drain),
            group_retirement.clone(),
            Arc::clone(&stop_workers),
        ) {
            Ok(reader) => reader,
            Err(error) => {
                stop_workers.store(true, Ordering::Release);
                #[cfg(test)]
                release_native_stdout_reader_for_test(&stdout_reader_gate);
                let _ = terminate_child_with_group(&mut child, &group_retirement);
                let _ = stdout_reader.join();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            group_retirement,
            stop_workers,
            _executable: executable,
            stdin,
            responses,
            stderr,
            stderr_observed,
            stderr_exceeded,
            stderr_fault,
            stdout_observed,
            stdout_fault,
            output_observed,
            stdout_drain,
            stderr_drain,
            #[cfg(test)]
            stdout_reader_gate,
            stdout_limit: command.limits().stdout(),
            output_limit: command.limits().output_bytes(),
            workspace: command.workspace().to_owned(),
            workspace_baseline,
            workspace_limit: command.limits().workspace_limit(),
            payload_offset: 0,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
        })
    }

    pub(in crate::native) fn send(
        &mut self,
        frame: &SessionFrame,
        cancellation: &Cancellation,
        deadline: Instant,
    ) -> Result<(), NativeRunnerError> {
        self.send_bytes(&frame.encode(), cancellation, deadline)
    }

    pub(in crate::native) fn send_bytes(
        &mut self,
        bytes: &[u8],
        cancellation: &Cancellation,
        deadline: Instant,
    ) -> Result<(), NativeRunnerError> {
        write_cancellable(&mut self.stdin, bytes, cancellation, deadline)
            .map_err(NativeRunnerError::Process)
    }

    pub(in crate::native) fn receive(
        &mut self,
        cancellation: &Cancellation,
        response_payload: bool,
        deadline: Instant,
    ) -> Result<(Vec<u8>, Option<NativeEnvelope>), NativeRunnerError> {
        loop {
            if cancellation.is_cancelled() {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Cancelled));
            }
            if self.stderr_exceeded.load(Ordering::Acquire)
                || self.stderr_fault.load(Ordering::Acquire) != 0
                || self.stdout_fault.load(Ordering::Acquire) != 0
                || self.output_observed.load(Ordering::Acquire) > self.output_limit
                || output_exceeds_limit(
                    self.stdout_observed.load(Ordering::Acquire),
                    self.stderr_observed.load(Ordering::Acquire),
                    self.output_limit,
                )
            {
                let error = match self.stdout_fault.load(Ordering::Acquire) {
                    2 => ProcessError::OutputLimit,
                    3 => ProcessError::Protocol,
                    _ if self.stderr_fault.load(Ordering::Acquire) == 1 => ProcessError::Io,
                    _ if self.stderr_fault.load(Ordering::Acquire) == 2 => {
                        ProcessError::OutputLimit
                    }
                    _ if self.stderr_exceeded.load(Ordering::Acquire) => ProcessError::OutputLimit,
                    _ => ProcessError::Io,
                };
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(error));
            }
            if let Some(limit) = self.workspace_limit {
                let current =
                    workspace_size(&self.workspace).map_err(NativeRunnerError::Process)?;
                if workspace_growth_exceeds(current, self.workspace_baseline, limit) {
                    self.terminate().map_err(NativeRunnerError::Process)?;
                    return Err(NativeRunnerError::Process(ProcessError::WorkspaceLimit));
                }
            }
            match self.responses.recv_timeout(Duration::from_millis(2)) {
                Ok(Ok(bytes)) => {
                    if self.stderr_exceeded.load(Ordering::Acquire)
                        || bytes.len() > self.stdout_limit
                        || self.output_observed.load(Ordering::Acquire) > self.output_limit
                        || output_exceeds_limit(
                            self.stdout_observed.load(Ordering::Acquire),
                            self.stderr_observed.load(Ordering::Acquire),
                            self.output_limit,
                        )
                        || output_exceeds_limit(
                            bytes.len(),
                            self.stderr_observed.load(Ordering::Acquire),
                            self.output_limit,
                        )
                    {
                        self.terminate().map_err(NativeRunnerError::Process)?;
                        return Err(NativeRunnerError::Process(ProcessError::OutputLimit));
                    }
                    let payload = if response_payload && bytes.get(5) == Some(&5) {
                        Some(self.receive_payload(cancellation, deadline)?)
                    } else {
                        None
                    };
                    self.wait_stdout_idle_gate(cancellation, deadline)?;
                    self.wait_stderr_idle_gate(cancellation, deadline)?;
                    return Ok((bytes, payload));
                }
                Ok(Err(error)) => {
                    let _ = self.terminate();
                    return Err(NativeRunnerError::Process(error));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.terminate();
                    return Err(NativeRunnerError::Unavailable);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        self.terminate().map_err(NativeRunnerError::Process)?;
                        return Err(NativeRunnerError::Process(ProcessError::Deadline));
                    }
                    if self
                        .leader_exit_observed()
                        .map_err(NativeRunnerError::Process)?
                    {
                        let _ = self.terminate();
                        return Err(NativeRunnerError::Unavailable);
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn pause_stdout_after_next_frame_for_test(&self) -> bool {
        let (state, _) = &*self.stdout_reader_gate;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.paused {
            return false;
        }
        state.pause_after_frame = Some(state.frames_seen.saturating_add(1));
        state.paused = true;
        true
    }

    fn receive_payload(
        &mut self,
        cancellation: &Cancellation,
        deadline: Instant,
    ) -> Result<NativeEnvelope, NativeRunnerError> {
        loop {
            if cancellation.is_cancelled() {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Cancelled));
            }
            if self.stderr_exceeded.load(Ordering::Acquire)
                || output_exceeds_limit(
                    self.stdout_observed.load(Ordering::Acquire),
                    self.stderr_observed.load(Ordering::Acquire),
                    self.output_limit,
                )
                || self.output_observed.load(Ordering::Acquire) > self.output_limit
            {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::OutputLimit));
            }
            if self.stderr_fault.load(Ordering::Acquire) != 0 {
                let error = if self.stderr_fault.load(Ordering::Acquire) == 2 {
                    ProcessError::OutputLimit
                } else {
                    ProcessError::Io
                };
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(error));
            }
            if self.stdout_fault.load(Ordering::Acquire) != 0 {
                let error = match self.stdout_fault.load(Ordering::Acquire) {
                    2 => ProcessError::OutputLimit,
                    3 => ProcessError::Protocol,
                    _ => ProcessError::Io,
                };
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(error));
            }
            if let Some(limit) = self.workspace_limit {
                let current =
                    workspace_size(&self.workspace).map_err(NativeRunnerError::Process)?;
                if workspace_growth_exceeds(current, self.workspace_baseline, limit) {
                    self.terminate().map_err(NativeRunnerError::Process)?;
                    return Err(NativeRunnerError::Process(ProcessError::WorkspaceLimit));
                }
            }
            if let Some(payload) = self.decode_stderr_payload()? {
                return Ok(payload);
            }
            if Instant::now() >= deadline {
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Deadline));
            }
            if self
                .leader_exit_observed()
                .map_err(NativeRunnerError::Process)?
            {
                let _ = self.terminate();
                return Err(NativeRunnerError::Unavailable);
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Requires the stderr pump to observe an empty nonblocking pipe before publishing a native
    /// response. This accounts for bytes already queued beside the stdout frame; writes after the
    /// pump's `WouldBlock` observation fall outside this gate's protocol boundary.
    fn wait_stderr_idle_gate(
        &mut self,
        cancellation: &Cancellation,
        deadline: Instant,
    ) -> Result<(), NativeRunnerError> {
        let requested = {
            let (probe, _) = &*self.stderr_drain;
            let mut probe = probe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            probe.requested = probe
                .requested
                .checked_add(1)
                .ok_or(NativeRunnerError::Process(ProcessError::Protocol))?;
            probe.requested
        };
        let (probe, changed) = &*self.stderr_drain;
        let mut probe = probe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while probe.completed < requested {
            if cancellation.is_cancelled() {
                drop(probe);
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Cancelled));
            }
            if let Some(error) = self.reader_fault_error() {
                drop(probe);
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(error));
            }
            let now = Instant::now();
            if now >= deadline {
                drop(probe);
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Deadline));
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(2));
            let (next, _) = changed
                .wait_timeout(probe, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            probe = next;
        }
        drop(probe);
        if cancellation.is_cancelled() {
            self.terminate().map_err(NativeRunnerError::Process)?;
            return Err(NativeRunnerError::Process(ProcessError::Cancelled));
        }
        if Instant::now() >= deadline {
            self.terminate().map_err(NativeRunnerError::Process)?;
            return Err(NativeRunnerError::Process(ProcessError::Deadline));
        }
        if let Some(error) = self.reader_fault_error() {
            self.terminate().map_err(NativeRunnerError::Process)?;
            return Err(NativeRunnerError::Process(error));
        }
        if self.output_observed.load(Ordering::Acquire) > self.output_limit {
            self.terminate().map_err(NativeRunnerError::Process)?;
            return Err(NativeRunnerError::Process(ProcessError::OutputLimit));
        }
        Ok(())
    }

    /// Waits until the stdout pump has drained bytes queued beside the response
    /// frame, then rejects any surplus response event before publishing it.
    fn wait_stdout_idle_gate(
        &mut self,
        cancellation: &Cancellation,
        deadline: Instant,
    ) -> Result<(), NativeRunnerError> {
        let requested = {
            let (probe, _) = &*self.stdout_drain;
            let mut probe = probe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            probe.requested = probe
                .requested
                .checked_add(1)
                .ok_or(NativeRunnerError::Process(ProcessError::Protocol))?;
            probe.requested
        };
        #[cfg(test)]
        release_native_stdout_reader_for_test(&self.stdout_reader_gate);

        let (probe, changed) = &*self.stdout_drain;
        let mut probe = probe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while probe.completed < requested {
            if cancellation.is_cancelled() {
                drop(probe);
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Cancelled));
            }
            if let Some(error) = self.reader_fault_error() {
                drop(probe);
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(error));
            }
            let now = Instant::now();
            if now >= deadline {
                drop(probe);
                self.terminate().map_err(NativeRunnerError::Process)?;
                return Err(NativeRunnerError::Process(ProcessError::Deadline));
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(2));
            let (next, _) = changed
                .wait_timeout(probe, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            probe = next;
        }
        drop(probe);
        if cancellation.is_cancelled() {
            self.terminate().map_err(NativeRunnerError::Process)?;
            return Err(NativeRunnerError::Process(ProcessError::Cancelled));
        }
        if Instant::now() >= deadline {
            self.terminate().map_err(NativeRunnerError::Process)?;
            return Err(NativeRunnerError::Process(ProcessError::Deadline));
        }
        if let Some(error) = self.reader_fault_error() {
            self.terminate().map_err(NativeRunnerError::Process)?;
            return Err(NativeRunnerError::Process(error));
        }
        match self.responses.try_recv() {
            Err(TryRecvError::Empty) => Ok(()),
            Ok(Err(error)) => {
                self.terminate().map_err(NativeRunnerError::Process)?;
                Err(NativeRunnerError::Process(error))
            }
            Ok(Ok(_surplus)) => {
                self.terminate().map_err(NativeRunnerError::Process)?;
                Err(NativeRunnerError::Process(ProcessError::Protocol))
            }
            Err(TryRecvError::Disconnected) => {
                self.terminate().map_err(NativeRunnerError::Process)?;
                Err(NativeRunnerError::Unavailable)
            }
        }
    }

    fn reader_fault_error(&self) -> Option<ProcessError> {
        match self.stderr_fault.load(Ordering::Acquire) {
            1 => Some(ProcessError::Io),
            2 => Some(ProcessError::OutputLimit),
            _ => match self.stdout_fault.load(Ordering::Acquire) {
                1 => Some(ProcessError::Io),
                2 => Some(ProcessError::OutputLimit),
                3 => Some(ProcessError::Protocol),
                _ if self.stderr_exceeded.load(Ordering::Acquire) => {
                    Some(ProcessError::OutputLimit)
                }
                _ => None,
            },
        }
    }

    fn decode_stderr_payload(&mut self) -> Result<Option<NativeEnvelope>, NativeRunnerError> {
        let stderr = match self.stderr.lock() {
            Ok(stderr) => stderr,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(candidate) = stderr.get(self.payload_offset..) else {
            return Ok(None);
        };
        if candidate.is_empty() {
            return Ok(None);
        }
        match NativeEnvelope::decode(candidate) {
            Ok(payload) => {
                self.payload_offset = stderr.len();
                Ok(Some(payload))
            }
            Err(NativeProtocolError::Truncated) => Ok(None),
            Err(_) => {
                drop(stderr);
                self.terminate().map_err(NativeRunnerError::Process)?;
                Err(NativeRunnerError::Protocol)
            }
        }
    }

    pub(in crate::native) fn take_stderr(&mut self) -> Vec<u8> {
        self.payload_offset = 0;
        match self.stderr.lock() {
            Ok(mut bytes) => std::mem::take(&mut *bytes),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }

    pub(in crate::native) fn terminate(&mut self) -> Result<(), ProcessError> {
        self.stop_workers.store(true, Ordering::Release);
        #[cfg(test)]
        release_native_stdout_reader_for_test(&self.stdout_reader_gate);
        let termination = terminate_child_with_group(&mut self.child, &self.group_retirement);
        let stdout_join = self
            .stdout_reader
            .take()
            .map(|reader| reader.join().map_err(|_| ProcessError::Io))
            .unwrap_or(Ok(()));
        let stderr_join = self
            .stderr_reader
            .take()
            .map(|reader| reader.join().map_err(|_| ProcessError::Io))
            .unwrap_or(Ok(()));
        termination?;
        stdout_join?;
        stderr_join
    }

    fn leader_exit_observed(&mut self) -> Result<bool, ProcessError> {
        #[cfg(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        ))]
        {
            let child_pid = self.child.id()?;
            child_exited_without_reaping(child_pid)
        }
        #[cfg(not(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        )))]
        {
            Err(ProcessError::UnsupportedLimit(
                crate::UnsupportedLimit::ProcessGroup,
            ))
        }
    }
}

fn write_cancellable(
    writer: &mut impl Write,
    bytes: &[u8],
    cancellation: &Cancellation,
    deadline: Instant,
) -> Result<(), ProcessError> {
    let mut offset = 0;
    while offset < bytes.len() {
        if cancellation.is_cancelled() {
            return Err(ProcessError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(ProcessError::Deadline);
        }
        match writer.write(&bytes[offset..]) {
            Ok(0) => return Err(ProcessError::Io),
            Ok(written) => offset += written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return Err(ProcessError::Io),
        }
    }
    Ok(())
}

#[cfg(all(
    test,
    unix,
    not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
#[test]
fn persistent_native_pipe_writes_observe_cancellation_and_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::net::UnixStream;

    let (mut writer, _reader) = UnixStream::pair()?;
    writer.set_nonblocking(true)?;
    let payload = vec![b'x'; 2 * 1024 * 1024];
    let (cancellation, cancel) = Cancellation::new();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        cancel.cancel();
    });
    let started = Instant::now();
    assert_eq!(
        write_cancellable(
            &mut writer,
            &payload,
            &cancellation,
            Instant::now() + Duration::from_secs(2),
        ),
        Err(ProcessError::Cancelled)
    );
    canceller
        .join()
        .map_err(|_| "cancellation thread panicked")?;
    assert!(started.elapsed() < Duration::from_secs(1));

    let (mut writer, _reader) = UnixStream::pair()?;
    writer.set_nonblocking(true)?;
    let (cancellation, _cancel) = Cancellation::new();
    let started = Instant::now();
    assert_eq!(
        write_cancellable(
            &mut writer,
            &payload,
            &cancellation,
            started + Duration::from_millis(20),
        ),
        Err(ProcessError::Deadline)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    Ok(())
}
