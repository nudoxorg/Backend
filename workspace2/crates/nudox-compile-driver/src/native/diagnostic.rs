use std::{
    io::{self, Read},
    sync::mpsc::{Receiver, SyncSender, TryRecvError},
};

use crate::types::NativeDiagnostic;

pub(super) const DIAGNOSTIC_CHUNK_BYTES: usize = 512;

pub(super) struct DiagnosticMessage {
    bytes: [u8; DIAGNOSTIC_CHUNK_BYTES],
    length: usize,
    terminal: DiagnosticTerminal,
}

pub(super) enum DiagnosticTerminal {
    Bytes,
    End,
    ReadFailure(io::Error),
}

/// One bounded receiver-owned merge of native stdout and stderr bytes.
pub(super) struct DiagnosticCollector<'diagnostic> {
    output: &'diagnostic mut [u8],
    retained: usize,
    pub(super) observed: usize,
    truncated: bool,
    streams_open: u8,
    pub(super) limit_exceeded: bool,
}

impl<'diagnostic> DiagnosticCollector<'diagnostic> {
    pub(super) const fn new(output: &'diagnostic mut [u8], streams_open: u8) -> Self {
        Self {
            output,
            retained: 0,
            observed: 0,
            truncated: false,
            streams_open,
            limit_exceeded: false,
        }
    }

    pub(super) fn drain(
        &mut self,
        receiver: &Receiver<DiagnosticMessage>,
    ) -> Result<(), io::Error> {
        loop {
            match receiver.try_recv() {
                Ok(message) => self.accept(message)?,
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }

    pub(super) fn finish(
        &mut self,
        receiver: &Receiver<DiagnosticMessage>,
    ) -> Result<(), io::Error> {
        while self.streams_open != 0 {
            match receiver.recv() {
                Ok(message) => self.accept(message)?,
                Err(_disconnected) => return Err(io::Error::from(io::ErrorKind::BrokenPipe)),
            }
        }
        Ok(())
    }

    fn accept(&mut self, message: DiagnosticMessage) -> Result<(), io::Error> {
        match message {
            DiagnosticMessage {
                bytes,
                length,
                terminal: DiagnosticTerminal::Bytes,
            } => {
                self.observed = self.observed.saturating_add(length);
                let room = self.output.len().saturating_sub(self.retained);
                let copied = room.min(length);
                self.output[self.retained..self.retained + copied]
                    .copy_from_slice(&bytes[..copied]);
                self.retained += copied;
                self.truncated |= copied != length;
                self.limit_exceeded |= self.observed > self.output.len();
                Ok(())
            }
            DiagnosticMessage {
                terminal: DiagnosticTerminal::End,
                ..
            } => {
                self.streams_open = self.streams_open.saturating_sub(1);
                Ok(())
            }
            DiagnosticMessage {
                terminal: DiagnosticTerminal::ReadFailure(cause),
                ..
            } => Err(cause),
        }
    }

    pub(super) fn into_view(self) -> NativeDiagnostic<'diagnostic> {
        NativeDiagnostic {
            bytes: &self.output[..self.retained],
            observed: self.observed,
            truncated: self.truncated,
        }
    }
}

pub(super) fn read_diagnostic<NativeOutput: Read>(
    mut native_output: NativeOutput,
    sender: SyncSender<DiagnosticMessage>,
) {
    loop {
        let mut bytes = [0; DIAGNOSTIC_CHUNK_BYTES];
        match native_output.read(&mut bytes) {
            Ok(0) => {
                // The collector can close only after its owner has selected a terminal and no
                // longer requires stream facts. A closed receiver is therefore an explicit
                // successful reader termination, not a discarded compiler I/O error.
                match sender.send(DiagnosticMessage {
                    bytes,
                    length: 0,
                    terminal: DiagnosticTerminal::End,
                }) {
                    Ok(()) => return,
                    Err(std::sync::mpsc::SendError(_message)) => return,
                }
            }
            Ok(length) => {
                if sender
                    .send(DiagnosticMessage {
                        bytes,
                        length,
                        terminal: DiagnosticTerminal::Bytes,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(cause) => {
                match sender.send(DiagnosticMessage {
                    bytes,
                    length: 0,
                    terminal: DiagnosticTerminal::ReadFailure(cause),
                }) {
                    Ok(()) => return,
                    Err(std::sync::mpsc::SendError(_message)) => return,
                }
            }
        }
    }
}
