use std::{
    borrow::Cow,
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    sync::Mutex,
};

/// Read metadata and payload ranges from one open file or from an existing byte slice.
pub(super) enum ArchiveSource<'a> {
    Slice(&'a [u8]),
    File {
        reader: Mutex<(BufReader<File>, usize)>,
        len: usize,
    },
}

impl<'a> ArchiveSource<'a> {
    pub(super) fn from_file(file: File) -> io::Result<Self> {
        let len = i64::try_from(file.metadata()?.len())
            .ok()
            .and_then(|len| usize::try_from(len).ok())
            .ok_or_else(|| io::Error::other("archive length does not fit file offsets"))?;
        Ok(Self::File {
            reader: Mutex::new((BufReader::new(file), 0)),
            len,
        })
    }

    pub(super) fn len(&self) -> usize {
        match self {
            Self::Slice(bytes) => bytes.len(),
            Self::File { len, .. } => *len,
        }
    }

    pub(super) fn read(&self, start: usize, len: usize) -> io::Result<Cow<'a, [u8]>> {
        self.read_range(start, len, false)
    }

    pub(super) fn read_payload(&self, start: usize, len: usize) -> io::Result<Cow<'a, [u8]>> {
        self.read_range(start, len, true)
    }

    fn read_range(&self, start: usize, len: usize, payload: bool) -> io::Result<Cow<'a, [u8]>> {
        let end = start
            .checked_add(len)
            .filter(|&end| end <= self.len())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "archive range out of bounds")
            })?;
        match self {
            Self::Slice(bytes) => Ok(Cow::Borrowed(&bytes[start..end])),
            Self::File { reader, .. } => {
                let mut bytes = Vec::with_capacity(len.min(64 * 1024));
                let mut guard = reader.lock().unwrap();
                let (reader, cursor) = &mut *guard;
                // Payload reads start after arbitrary predicates may have run. Read
                // directly from the file, bypassing any buffered metadata read-ahead.
                let result = if payload {
                    reader.seek(SeekFrom::Start(start as u64))?;
                    reader.get_mut().take(len as u64).read_to_end(&mut bytes)
                } else {
                    reader.seek_relative(start as i64 - *cursor as i64)?;
                    reader.take(len as u64).read_to_end(&mut bytes)
                };
                *cursor = start + bytes.len();
                result?;
                if bytes.len() != len {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "short archive read",
                    ));
                }
                Ok(Cow::Owned(bytes))
            }
        }
    }
}
