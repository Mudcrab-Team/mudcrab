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
        reader: Mutex<(BufReader<File>, Option<usize>)>,
        len: usize,
    },
}

impl<'a> ArchiveSource<'a> {
    pub(super) fn from_file(file: File) -> io::Result<Self> {
        let len = i64::try_from(file.metadata()?.len())
            .and_then(usize::try_from)
            .map_err(|_| io::Error::other("archive length does not fit file offsets"))?;
        Ok(Self::File {
            reader: Mutex::new((BufReader::new(file), None)),
            len,
        })
    }

    pub(super) fn with_file<T>(&self, read: impl FnOnce(&File) -> io::Result<T>) -> io::Result<T> {
        let Self::File { reader, .. } = self else {
            return Err(io::Error::other("archive has no open file"));
        };
        let mut guard = reader
            .lock()
            .map_err(|_| io::Error::other("archive reader lock is poisoned"))?;
        let (reader, cursor) = &mut *guard;
        *cursor = None;
        let result = read(reader.get_ref());
        // Discard buffered metadata after an operation that may have repositioned the file.
        reader.seek(SeekFrom::Start(0))?;
        result
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
                let mut guard = reader.lock().unwrap();
                let (reader, cursor) = &mut *guard;
                let previous = cursor.take();
                // Payload reads bypass metadata read-ahead and check for truncation
                // before reserving the requested length.
                if payload {
                    let file_len = reader.get_ref().metadata()?.len();
                    if end as u64 > file_len {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "short archive read",
                        ));
                    }
                    reader.seek(SeekFrom::Start(start as u64))?;
                    let mut bytes = Vec::new();
                    bytes.try_reserve_exact(len).map_err(io::Error::other)?;
                    bytes.resize(len, 0);
                    reader.get_mut().read_exact(&mut bytes)?;
                    *cursor = Some(end);
                    return Ok(Cow::Owned(bytes));
                }

                if let Some(previous) = previous {
                    let start = i64::try_from(start).map_err(io::Error::other)?;
                    let previous = i64::try_from(previous).map_err(io::Error::other)?;
                    reader.seek_relative(start - previous)?;
                } else {
                    reader.seek(SeekFrom::Start(start as u64))?;
                }
                let mut bytes = Vec::with_capacity(len.min(64 * 1024));
                reader.take(len as u64).read_to_end(&mut bytes)?;
                *cursor = Some(start + bytes.len());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn first_metadata_read_seeks_from_a_prepositioned_file() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"BSA\0data").unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        let source = ArchiveSource::from_file(file).unwrap();

        assert_eq!(source.read(0, 4).unwrap().as_ref(), b"BSA\0");
        assert_eq!(source.read(4, 4).unwrap().as_ref(), b"data");
    }

    #[test]
    fn owned_payload_read_has_exact_capacity_and_rejects_truncation() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let size = 96 * 1024 + 1;
        let data = vec![b'x'; size];
        file.write_all(&data).unwrap();
        file.write_all(b"suffix").unwrap();
        let source = ArchiveSource::from_file(file.reopen().unwrap()).unwrap();
        let Cow::Owned(payload) = source.read_payload(0, size).unwrap() else {
            panic!("file payload must be owned");
        };
        assert_eq!(payload, data);
        assert_eq!(payload.capacity(), size);
        assert_eq!(source.read(size, 6).unwrap().as_ref(), b"suffix");
        assert_eq!(
            source.read_payload(1, usize::MAX).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );

        file.as_file().set_len(size as u64).unwrap();
        assert_eq!(
            source.read_payload(size, 6).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(source.read(size - 1, 1).unwrap().as_ref(), b"x");
    }
}
