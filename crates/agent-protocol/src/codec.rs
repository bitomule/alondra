use crate::{Command, Error, Event, Frame, MAX_FRAME_BYTES, MAX_FRAME_DEPTH, PROTOCOL_VERSION};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub fn encode(frame: &Frame) -> Result<Vec<u8>, Error> {
    let payload = payload(frame)?;
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(&payload);
    Ok(framed)
}

pub(crate) fn payload(frame: &Frame) -> Result<Vec<u8>, Error> {
    frame.validate()?;
    serialize_payload(frame)
}

fn serialize_payload(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    // Bound recursive serialization before a descendant can outrun the input codec.
    let mut writer = CappedBuffer::default();
    serde_json::to_writer(&mut writer, value)
        .map_err(|error| writer.limit.take().unwrap_or(Error::Json(error)))?;
    Ok(writer.bytes)
}

pub fn command_digest(command: &Command) -> Result<String, Error> {
    command.validate()?;
    Ok(fingerprint(&serialize_payload(&BorrowedFrame {
        protocol_version: PROTOCOL_VERSION,
        body: BorrowedBody::Command(command),
    })?))
}

#[derive(Serialize)]
struct BorrowedFrame<'a> {
    protocol_version: u16,
    body: BorrowedBody<'a>,
}
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BorrowedBody<'a> {
    Command(&'a Command),
    Event(&'a Event),
}

pub(crate) fn event_digest(event: &Event) -> Result<String, Error> {
    event.validate()?;
    Ok(fingerprint(&serialize_payload(&BorrowedFrame {
        protocol_version: PROTOCOL_VERSION,
        body: BorrowedBody::Event(event),
    })?))
}

pub(crate) fn fingerprint(payload: &[u8]) -> String {
    format!("{:x}", Sha256::digest(payload))
}

#[derive(Default)]
struct CappedBuffer {
    bytes: Vec<u8>,
    depth: usize,
    quoted: bool,
    escaped: bool,
    limit: Option<Error>,
}
impl CappedBuffer {
    fn reject(&mut self, error: Error) -> std::io::Result<usize> {
        self.limit = Some(error);
        Err(std::io::Error::other("frame limit"))
    }
}
impl Write for CappedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for &byte in bytes {
            if self.bytes.len() == MAX_FRAME_BYTES {
                return self.reject(Error::FrameTooLarge);
            }
            if self.quoted {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.quoted = false;
                }
            } else {
                match byte {
                    b'"' => self.quoted = true,
                    b'{' | b'[' => {
                        if self.depth == MAX_FRAME_DEPTH {
                            return self.reject(Error::FrameTooDeep);
                        }
                        self.depth += 1;
                    }
                    b'}' | b']' => self.depth -= 1,
                    _ => {}
                }
            }
            self.bytes.push(byte);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One bounded payload and its validated decoding from the same transport read.
/// This is correlation evidence, never execution or ownership authority.
pub struct ReceivedFrame {
    frame: Frame,
    payload: Vec<u8>,
    digest: String,
}
impl ReceivedFrame {
    pub fn frame(&self) -> &Frame {
        &self.frame
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn into_parts(self) -> (Frame, Vec<u8>, String) {
        (self.frame, self.payload, self.digest)
    }
}

pub struct FramedReader<R> {
    reader: R,
    failed: bool,
}
impl<R: Read> FramedReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            failed: false,
        }
    }
    pub fn read(&mut self) -> Result<Option<Frame>, Error> {
        self.read_received().map(|frame| frame.map(|frame| frame.frame))
    }
    /// The digest covers the received bytes, preserving sender key order and spacing.
    pub fn read_with_digest(&mut self) -> Result<Option<(Frame, String)>, Error> {
        self.read_received()
            .map(|frame| frame.map(|frame| (frame.frame, frame.digest)))
    }
    pub fn read_received(&mut self) -> Result<Option<ReceivedFrame>, Error> {
        if self.failed {
            return Err(Error::Poisoned);
        }
        let result = self.read_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn read_inner(&mut self) -> Result<Option<ReceivedFrame>, Error> {
        let mut header = [0; 4];
        loop {
            match self.reader.read(&mut header[..1]) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::Io(error)),
            }
        }
        self.reader.read_exact(&mut header[1..]).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::TruncatedHeader
            } else {
                Error::Io(error)
            }
        })?;
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 {
            return Err(Error::Invalid("empty frame"));
        }
        if length > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge);
        }
        let mut bytes = vec![0; length];
        self.reader.read_exact(&mut bytes).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::TruncatedPayload
            } else {
                Error::Io(error)
            }
        })?;
        let frame: Frame = serde_json::from_slice(&bytes).map_err(Error::Json)?;
        frame.validate()?;
        let digest = fingerprint(&bytes);
        Ok(Some(ReceivedFrame {
            frame,
            payload: bytes,
            digest,
        }))
    }
}

pub struct FramedWriter<W> {
    writer: W,
    failed: bool,
}
impl<W: Write> FramedWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            failed: false,
        }
    }
    pub fn write(&mut self, frame: &Frame) -> Result<(), Error> {
        if self.failed {
            return Err(Error::Poisoned);
        }
        let bytes = encode(frame)?;
        if let Err(error) = self
            .writer
            .write_all(&bytes)
            .and_then(|()| self.writer.flush())
        {
            self.failed = true;
            return Err(Error::Io(error));
        }
        Ok(())
    }
}
